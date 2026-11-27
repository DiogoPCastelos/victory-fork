//! Title memory by address, for translated shaders that read and write
//! through raw 40-bit addresses (`victory_shader::emit::SLOT_MEMORY`: the
//! console's ray-tracing structure builder and the programs that walk its
//! structures; the structures themselves lie in plain title memory, in no
//! buffer object).
//!
//! The copies are the ones every other access uses ([`crate::shadow`]: a
//! title buffer's copy, or the 4 MiB window of memory outside buffers), so
//! a root view and a raw address reach the same host bytes. This module
//! keeps what a shader needs to find them:
//!
//! - the **directory**: per 64 KiB page of title address space the number
//!   of the copy that holds the page (0: none) and the page's byte offset
//!   in it; one buffer for all lists, entries only ever go from empty to
//!   filled;
//! - per view heap a run of slots with a raw unordered access view of each
//!   numbered copy (slot 0 of the run: a null view, for pages without a
//!   copy);
//! - the **faults**: the page numbers shaders found empty, read back here
//!   once a frame, so the copies exist from the next frame on. A pointer a
//!   shader takes out of memory cannot be known before it runs; the
//!   addresses a dispatch is given as root views are made ready before it.
//!
//! **A page that is not there when a shader touches it is a lost access**:
//! the read gives 0, the store is dropped, and the fault note only helps
//! the next frame. For the structure builder that is the same as a dropped
//! stage, so before a dispatch the host makes ready everything it can see
//! the dispatch name: the whole copy around each root view, around each
//! pair of root constants that is a title address and around each root
//! constant that is one in 256-byte units (the builder passes structures
//! all three ways), and the 4 MiB after each (a structure may cross a
//! window's end).
//! shortcut: what a stage reaches only through pointers it reads out of
//! memory (InitHeaders' table of structure addresses, LoadInstances'
//! instance records) is not made ready unless something else named it;
//! the fault count in the log says whether that happens. The whole answer
//! is the extent of the title's allocation around an address, which the
//! memory module of the layer knows and this one does not ask yet.
//!
//! shortcut: a copy that is numbered is kept for the rest of the run, and
//! there are at most [`COPIES`] of them (4 MiB windows mostly: 4 GiB at
//! most). Every dispatch of such a shader brings all numbered copies up to
//! date and, if it can store, declares all of them written (the bytes are
//! compared on the CPU afterwards, as for any unordered access view): a
//! dirty-page mask written by the shader would bound both.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use victory_shader::emit::{MEMORY_FAULTS, MEMORY_PAGES};
use windows::Win32::Graphics::Direct3D12::*;

use crate::descriptor::host::HostHeap;
use crate::host::{HostList, host};
use crate::shadow::Shadow;

/// Copies that can be numbered (the run of heap slots is one longer). A
/// listed frame of the opening cutscene names about 50 windows.
const COPIES: u32 = 1023;
/// A window of memory outside buffers (`shadow.rs`).
const CELL: u64 = 4 << 20;
/// A directory page.
const PAGE: u64 = 1 << 16;

struct Memory {
    /// Buffer, its mapping, its GPU address.
    directory: (ID3D12Resource, *mut u32, u64),
    faults: (ID3D12Resource, *mut u32, u64),
    /// Copy `n` is `copies[n - 1]`; `None`: a number let go, free for reuse.
    copies: Vec<Option<Numbered>>,
    /// Numbers let go ([`Memory::let_go_idle`]), taken again first.
    free: Vec<usize>,
    /// Numbers given since the last [`ready`] took them: brought up to date
    /// and declared written by the list that numbered them.
    fresh: Vec<usize>,
    /// Per host view heap (by address): the first slot of its run, and how
    /// many copies have their view there.
    heaps: HashMap<usize, (u32, u32)>,
    /// The frame the faults were last read in.
    frame: u64,
    /// Pages asked for this frame that are not the title's memory.
    refused: std::collections::HashSet<u64>,
    /// Since the start: copies let go, and their bytes; copies declared
    /// written by lists (each a CPU compare after the list).
    stats: [u64; 3],
}

/// A numbered copy.
struct Numbered {
    shadow: Arc<Shadow>,
    /// When a dispatch's arguments last named it (`crate::host::now_ms`).
    used: u64,
    /// Reached by a shader through a pointer it read out of memory (a page
    /// of it faulted): such a copy is declared written by every list that
    /// stores, and never let go.
    pointer: bool,
}

/// How long a copy no dispatch names, and no shader reached by a pointer,
/// is kept (`VICTORY_RAW_IDLE_MS`, 30 s).
fn idle_ms() -> u64 {
    static MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *MS.get_or_init(|| std::env::var("VICTORY_RAW_IDLE_MS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(30_000))
}

/// The most bytes made ready around a page that faulted, half before and
/// half after it, inside its region of the address space.
const AROUND_FAULT: u64 = 16 << 20;

// SAFETY: the mappings are of buffers the struct owns; all access is under
// the mutex.
unsafe impl Send for Memory {}

static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// What a draw binds.
pub(crate) struct Ready {
    pub(crate) directory: u64,
    pub(crate) faults: u64,
    /// The view heap slot of the null view; copy `n`'s view is `n` further.
    pub(crate) base: u32,
}

impl Memory {
    fn new() -> Option<Memory> {
        let host = host()?;
        let make = |dwords: u64, what: &str| {
            let (buffer, data) = host.shared_buffer(dwords * 4)?;
            crate::host::name(&buffer, || format!("title memory by address: {what}"));
            // SAFETY: a fresh mapping of at least that many bytes.
            unsafe { std::ptr::write_bytes(data, 0, (dwords * 4) as usize) };
            // SAFETY: a live buffer.
            let gpu = unsafe { buffer.GetGPUVirtualAddress() };
            Some((buffer, data.cast::<u32>(), gpu))
        };
        tracing::info!("title memory by address: a shader reads or writes through raw addresses");
        Some(Memory {
            directory: make(u64::from(MEMORY_PAGES) * 2, "directory")?,
            faults: make(u64::from(MEMORY_FAULTS), "faults")?,
            copies: Vec::new(),
            free: Vec::new(),
            fresh: Vec::new(),
            heaps: HashMap::new(),
            frame: u64::MAX,
            refused: Default::default(),
            stats: [0; 3],
        })
    }

    /// Lets go of copies no dispatch has named for [`idle_ms`] and no shader
    /// reached by a pointer: their directory entries are emptied (a page
    /// touched again faults and is copied anew the next frame), the number
    /// is free, and the copy goes when nothing else holds it. At most 64 a
    /// frame.
    fn let_go_idle(&mut self) {
        let now = crate::host::now_ms();
        let idle = idle_ms();
        let mut went = 0;
        for n in 0..self.copies.len() {
            if went >= 64 {
                break;
            }
            let Some(c) = &self.copies[n] else { continue };
            if c.pointer || now < c.used + idle {
                continue;
            }
            let (first, len) = c.shadow.span();
            let mut at = first & !(PAGE - 1);
            while at < first + len && at / PAGE < u64::from(MEMORY_PAGES) - 1 {
                // SAFETY: inside the directory's mapping.
                let entry = unsafe { self.directory.1.add((at / PAGE) as usize * 2) };
                // SAFETY: as above; the number first, so a shader sees the
                // page empty before its offset changes.
                unsafe {
                    if entry.read() == n as u32 + 1 {
                        entry.write_volatile(0);
                    }
                }
                at += PAGE;
            }
            self.stats[0] += 1;
            self.stats[1] += len;
            self.copies[n] = None;
            self.free.push(n);
            went += 1;
        }
    }

    /// The number of a newly numbered copy: a free one (every heap's run
    /// then writes that slot's view again), else the next.
    fn number(&mut self, shadow: Arc<Shadow>, pointer: bool) -> Option<usize> {
        let numbered = Numbered { shadow, used: crate::host::now_ms(), pointer };
        if let Some(n) = self.free.pop() {
            self.copies[n] = Some(numbered);
            for run in self.heaps.values_mut() {
                run.1 = run.1.min(n as u32);
            }
            self.fresh.push(n);
            return Some(n);
        }
        if self.copies.len() >= COPIES as usize {
            return None;
        }
        self.copies.push(Some(numbered));
        self.fresh.push(self.copies.len() - 1);
        Some(self.copies.len() - 1)
    }

    /// The line of the copies, every ten seconds.
    fn say(&self) {
        static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = crate::host::now_ms();
        if now < LAST.load(std::sync::atomic::Ordering::Relaxed) + 10_000 {
            return;
        }
        LAST.store(now, std::sync::atomic::Ordering::Relaxed);
        let (n, bytes, pointer) = self.census();
        tracing::info!(
            "title memory by address: {n} copies numbered now ({} MB), {pointer} of them reached by pointers; since the start {} let go ({} MB) after {} ms with nothing naming them, {} declared written by lists",
            bytes >> 20,
            self.stats[0],
            self.stats[1] >> 20,
            idle_ms(),
            self.stats[2]
        );
    }

    /// The live copies' count and bytes, and how many are reached by
    /// pointers.
    fn census(&self) -> (usize, u64, usize) {
        let live = self.copies.iter().flatten();
        let (mut n, mut bytes, mut pointer) = (0, 0, 0);
        for c in live {
            n += 1;
            bytes += c.shadow.span().1;
            pointer += usize::from(c.pointer);
        }
        (n, bytes, pointer)
    }

    /// Gives the page of `address` its copy, if it has none and the memory
    /// is the title's; the copy's number, marked used now (`pointer`: a
    /// shader reached it through a pointer).
    fn map(&mut self, address: u64, pointer: bool) -> Option<usize> {
        let page = address / PAGE;
        // The last entry stays empty: addresses past the directory read it.
        if page >= u64::from(MEMORY_PAGES) - 1 {
            return None;
        }
        // SAFETY: inside the directory's mapping.
        let entry = unsafe { self.directory.1.add(page as usize * 2) };
        // SAFETY: as above.
        let held = unsafe { entry.read() };
        if held != 0 {
            let n = held as usize - 1;
            if let Some(Some(c)) = self.copies.get_mut(n) {
                c.used = crate::host::now_ms();
                c.pointer |= pointer;
            }
            return Some(n);
        }
        let start = page * PAGE;
        // A shader's pointer, or a constant taken for an address, may be
        // anything: only memory the title has.
        if self.refused.contains(&page) {
            return None;
        }
        if !crate::texture::committed(start as *const u8, PAGE as usize) {
            self.refused.insert(page);
            return None;
        }
        // The copy a root view at that address would get; a buffer that
        // ends inside the page is passed over for what holds all of it.
        let whole = |s: &Arc<Shadow>| s.offset_in(start).is_some_and(|_| s.left(start) >= PAGE);
        let shadow = crate::shadow::find(start, 1)
            .filter(whole)
            .or_else(|| crate::shadow::find(start, PAGE).filter(whole))?;
        // SAFETY: a live buffer.
        let flags = unsafe { shadow.buffer.GetDesc() }.Flags;
        if flags.0 & D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS.0 == 0 {
            return None;
        }
        let known = self.copies.iter().position(|c| c.as_ref().is_some_and(|c| Arc::ptr_eq(&c.shadow, &shadow)));
        let n = match known {
            Some(n) => {
                if let Some(Some(c)) = self.copies.get_mut(n) {
                    c.used = crate::host::now_ms();
                    c.pointer |= pointer;
                }
                n
            }
            None => match self.number(shadow.clone(), pointer) {
                Some(n) => {
                // Every page the copy holds whole, not only the one asked
                // for: a structure is more than 64 KiB.
                // shortcut: a page of a title buffer inside this one that
                // has a copy of its own is given this copy too.
                let (first, len) = shadow.span();
                // A copy in place begins in its buffer after the bytes before
                // the title memory's first (its heap starts on a 64 KiB
                // line): offsets in the directory are the buffer's.
                let bias = shadow.offset(first);
                let mut at = first.next_multiple_of(PAGE);
                while at + PAGE <= first + len && at / PAGE < u64::from(MEMORY_PAGES) - 1 {
                    // SAFETY: inside the directory's mapping.
                    let other = unsafe { self.directory.1.add((at / PAGE) as usize * 2) };
                    // SAFETY: as above; the offset first, as below.
                    unsafe {
                        if other.read() == 0 {
                            other.add(1).write_volatile((at - first + bias) as u32);
                            other.write_volatile(n as u32 + 1);
                        }
                    }
                    at += PAGE;
                }
                n
                }
                None => {
                    static ONCE: std::sync::Once = std::sync::Once::new();
                    ONCE.call_once(|| {
                        tracing::warn!(COPIES, "title memory by address: no number left for another copy; its pages read zeros")
                    });
                    return None;
                }
            },
        };
        // In the buffer (`Shadow::offset`: past the bytes before the title
        // memory in a copy in place).
        let offset = shadow.offset_in(start).map_or(0, |_| shadow.offset(start));
        // The offset first: a shader that sees the number sees it too.
        // SAFETY: inside the directory's mapping.
        unsafe {
            entry.add(1).write_volatile(offset as u32);
            entry.write_volatile(n as u32 + 1);
        }
        Some(n)
    }

    /// The pages shaders found empty since the last frame.
    fn take_faults(&mut self) {
        let mut pages = Vec::new();
        for i in 1..MEMORY_FAULTS as usize {
            // SAFETY: inside the faults' mapping; the GPU may be writing,
            // a number missed now is written again next frame.
            let at = unsafe { self.faults.1.add(i) };
            // SAFETY: as above.
            let page = unsafe { at.read_volatile() };
            if page != 0 {
                pages.push(page);
                // SAFETY: as above.
                unsafe { at.write_volatile(0) };
            }
        }
        if !pages.is_empty() {
            // Each is an access that read 0 or was dropped.
            static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 50 {
                tracing::warn!(
                    pages = pages.len(),
                    first = format_args!("{:#x}", u64::from(pages[0]) * PAGE),
                    "title memory by address: shaders touched pages that had no copy (those accesses read 0 or were dropped); the copies are made now"
                );
            }
        }
        // Pages that are not the title's memory at all: a shader computed
        // an address nothing holds (a wild pointer, or a bad offset).
        let mut wild = Vec::new();
        let faulted: Vec<u64> = pages.iter().map(|&p| u64::from(p)).collect();
        for page in pages {
            let at = u64::from(page) * PAGE;
            self.map(at, true);
            if self.refused.contains(&u64::from(page)) {
                wild.push(at);
                continue;
            }
            // What lies around it in the same region is likely reached the
            // same way (a structure's nodes after its header): made ready now
            // too, up to `AROUND_FAULT`, so it does not fault a frame later.
            if let Some((region, end)) = region_of(at) {
                let from = at.saturating_sub(AROUND_FAULT / 2).max(region) & !(CELL - 1);
                let to = (at + AROUND_FAULT / 2).min(end);
                let mut cell = from;
                while cell < to {
                    self.map(cell.max(region), true);
                    cell += CELL;
                }
            }
        }
        if !wild.is_empty() {
            static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 50 {
                let first: Vec<String> = wild.iter().take(6).map(|a| format!("{a:#x}")).collect();
                tracing::warn!(
                    pages = wild.len(),
                    copies = self.copies.len(),
                    first = %first.join(" "),
                    "title memory by address: shaders touched pages that are not the title's memory (a wild address)"
                );
            }
        }
        classify_faults(&faulted, &self.refused);
    }

    /// What a dispatch names: the copy around `address` and the 4 MiB
    /// after it; their numbers go into `named`.
    fn name(&mut self, address: u64, named: &mut Vec<usize>) {
        named.extend(self.map(address, false));
        named.extend(self.map((address & !(CELL - 1)) + CELL, false));
    }
}

/// The region of the address space `at` lies in (same allocation state and
/// protection, from Windows), as `[start, end)`.
fn region_of(at: u64) -> Option<(u64, u64)> {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEMORY_BASIC_INFORMATION, VirtualQuery};
    // SAFETY: plain data the call fills in.
    let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid out pointer of the right size.
    let n = unsafe { VirtualQuery(at as *const std::ffi::c_void, &mut info, size_of_val(&info)) };
    (n != 0 && info.State == MEM_COMMIT).then(|| (info.BaseAddress as u64, info.BaseAddress as u64 + info.RegionSize as u64))
}

/// Before a draw whose shader reads title memory by address: what its
/// root arguments name gets its copies (`addresses`: its root views;
/// `constants`: its root constants, see the module), every numbered copy
/// has its view in `heap`'s run and is brought up to date when `list` is
/// submitted (and, with `writes`, copied back after). None if the host
/// cannot hold it; the draw is then dropped.
pub(crate) fn ready(
    list: &HostList,
    heap: &HostHeap,
    addresses: &[u64],
    constants: &[u32],
    writes: bool,
) -> Option<Ready> {
    let mut guard = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Memory::new();
    }
    let m = guard.as_mut()?;
    let frame = crate::frame::current();
    if m.frame != frame {
        m.frame = frame;
        m.refused.clear();
        m.take_faults();
        m.let_go_idle();
        m.say();
    } else {
        // The faults are read every 250 ms as well: as the world appears a
        // frame takes seconds, and the pages a dispatch found empty are then
        // copied (and counted) before the next frame begins.
        static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = crate::host::now_ms();
        if now >= LAST.load(std::sync::atomic::Ordering::Relaxed) + 250 {
            LAST.store(now, std::sync::atomic::Ordering::Relaxed);
            m.take_faults();
        }
    }
    let mut named = Vec::new();
    for &address in addresses {
        m.name(address, &mut named);
    }
    // A title address in two constants (low dword, then bits 32 to 39), or
    // in one in 256-byte units. A constant that only looks like one names
    // memory nothing reads; one that is not the title's memory is refused.
    for (i, &c) in constants.iter().enumerate() {
        if let Some(&high) = constants.get(i + 1)
            && (1..=0xFF).contains(&high)
            && c >= 0x1_0000
        {
            m.name(u64::from(c) | u64::from(high) << 32, &mut named);
        }
        if c >= 0x0100_0000 {
            m.name(u64::from(c) << 8, &mut named);
        }
    }
    let fresh = std::mem::take(&mut m.fresh);
    let key = std::ptr::from_ref(heap) as usize;
    let (base, made) = match m.heaps.get(&key) {
        Some(&run) => run,
        None => {
            // Held for the heap's life: not given back with a list's slots.
            let base = heap.dynamic(COPIES + 1)?;
            heap.null_raw_uav_at(base);
            (base, 0)
        }
    };
    for (n, slot) in m.copies.iter().enumerate().skip(made as usize) {
        match slot {
            Some(c) => {
                let (first, len) = c.shadow.span();
                // The whole buffer the directory's offsets count in, the bytes
                // before the title memory of a copy in place included.
                heap.raw_uav_at(base + 1 + n as u32, &c.shadow.buffer, 0, len + c.shadow.offset(first));
            }
            // A number let go: its pages are empty in the directory.
            None => heap.null_raw_uav_at(base + 1 + n as u32),
        }
    }
    m.heaps.insert(key, (base, m.copies.len() as u32));
    // Brought up to date once per list (every live copy: a shader may read
    // any of them through a pointer), and copies numbered since. Declared
    // written (each a CPU compare after the list) only where a store can
    // land: copies this list's dispatches name, copies shaders reached by
    // pointers, and copies numbered since. Before, every numbered copy was
    // declared by every list that stores.
    // shortcut: a copy another list numbers meanwhile is brought up to date
    // by that list's submission, which may run after this one's.
    const READ: usize = usize::MAX - 0x6d;
    /// Per copy `n`, `WRITTEN - n`: declared written in this list.
    const WRITTEN: usize = usize::MAX - 0x1_0000;
    let first_read = list.first_use(READ);
    let mut declared = 0;
    for (n, slot) in m.copies.iter().enumerate() {
        let Some(c) = slot else { continue };
        let (start, len) = c.shadow.span();
        let new = fresh.contains(&n);
        if first_read || new {
            c.shadow.refresh(list, start, len);
        }
        let reached = new || c.pointer || named.contains(&n);
        if writes && reached && list.first_use(WRITTEN - n) {
            c.shadow.gpu_writes(list, start, len);
            declared += 1;
        }
    }
    m.stats[2] += declared;
    if first_read {
        list.keep(&m.directory.0);
        list.keep(&m.faults.0);
    }
    Some(Ready {
        directory: m.directory.2,
        faults: m.faults.2,
        base,
    })
}

/// Forgets the run of title-memory views kept for host view heap `heap`
/// (`Memory::heaps`, by address), when the title releases its heap. A heap
/// made later at the same address found the old run: its null view and the
/// copies' views were never written there, and the run's first slot could
/// lie among the new heap's title slots or past its end, so shaders reading
/// through raw addresses indexed views that were not theirs. (After the
/// fxceless fork's pull request #589, by AscilCH.)
pub(crate) fn forget_heap(heap: &HostHeap) {
    let key = std::ptr::from_ref(heap) as usize;
    if let Some(m) = MEMORY.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.heaps.remove(&key);
    }
}

// The ranges the title's own builder calls name (`victory_hle`'s
// `VICTORY_BVH_TRACE`): each build's destination and scratch, so a page a
// shader touched without a copy can be told apart: inside a range the title
// asked the builder to use (a legal address the layer did not make ready),
// inside the title's memory but outside every such range (a store or read
// past a structure's end), or not the title's memory at all.

/// A range a builder call names: start, bytes, the call's number since the
/// start, and what it is.
#[derive(Clone, Copy)]
struct BuildRange {
    start: u64,
    len: u64,
    call: u64,
    what: &'static str,
}

/// The ranges noted, at most the last [`RANGES_KEPT`].
static RANGES: Mutex<std::collections::VecDeque<BuildRange>> = Mutex::new(std::collections::VecDeque::new());
const RANGES_KEPT: usize = 16_384;

/// Notes a range the title's builder calls name (`what`: "destination",
/// "scratch", "update destination", "rehydrated"), for the faults' census.
pub fn note_build_range(start: u64, len: u64, call: u64, what: &'static str) {
    if len == 0 {
        return;
    }
    let mut ranges = RANGES.lock().unwrap_or_else(|e| e.into_inner());
    if ranges.len() >= RANGES_KEPT {
        ranges.pop_front();
    }
    ranges.push_back(BuildRange { start, len, call, what });
}

/// What `page` (a 64 KiB page's first byte) lies in: the latest noted range
/// that overlaps it, else the nearest one below it and the distance.
fn build_range_of(page: u64) -> (Option<BuildRange>, Option<(BuildRange, u64)>) {
    let ranges = RANGES.lock().unwrap_or_else(|e| e.into_inner());
    let end = page + PAGE;
    let inside = ranges.iter().rev().find(|r| r.start < end && page < r.start + r.len).copied();
    let below = ranges
        .iter()
        .filter(|r| r.start + r.len <= page)
        .min_by_key(|r| page - (r.start + r.len))
        .map(|r| (*r, page - (r.start + r.len)));
    (inside, below)
}

/// The faulted pages by what they are (with builder ranges noted): a line
/// each kind, with the first examples and the build they belong to or lie
/// past.
fn classify_faults(pages: &[u64], refused: &std::collections::HashSet<u64>) {
    if RANGES.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
        return;
    }
    let (mut legal, mut past, mut wild) = (Vec::new(), Vec::new(), Vec::new());
    for &page in pages {
        let at = page * PAGE;
        if refused.contains(&page) {
            wild.push(at);
            continue;
        }
        match build_range_of(at) {
            (Some(r), _) => legal.push((at, r)),
            (None, below) => past.push((at, below)),
        }
    }
    static SAID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if SAID.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 40 {
        return;
    }
    let first_legal: Vec<String> = legal
        .iter()
        .take(4)
        .map(|(at, r)| format!("{at:#x} in call {}'s {} {:#x}+{:#x}", r.call, r.what, r.start, r.len))
        .collect();
    let first_past: Vec<String> = past
        .iter()
        .take(4)
        .map(|(at, below)| match below {
            Some((r, by)) => format!("{at:#x}, {by:#x} past call {}'s {} {:#x}+{:#x}", r.call, r.what, r.start, r.len),
            None => format!("{at:#x}, below every noted range"),
        })
        .collect();
    tracing::warn!(
        legal = legal.len(),
        past = past.len(),
        wild = wild.len(),
        "title memory by address, the faulted pages against the builder calls' ranges: inside a build's destination or scratch (a legal address not made ready) [{}]; the title's memory outside every such range [{}]; not the title's memory {}",
        first_legal.join("; "),
        first_past.join("; "),
        wild.iter().take(4).map(|a| format!("{a:#x}")).collect::<Vec<_>>().join(" ")
    );
}

#[cfg(test)]
mod range_tests {
    use super::*;

    /// A faulted page is inside a noted range, or past the nearest one below
    /// it by its distance.
    #[test]
    fn faulted_pages_are_told_by_the_builder_ranges() {
        let base = 0x7_7000_0000u64;
        note_build_range(base, 0x2_b800, 1, "destination");
        note_build_range(base + 0x10_0000, 0x1_3674, 1, "scratch");
        let (inside, _) = build_range_of(base + 0x2_0000 & !(PAGE - 1));
        assert_eq!(inside.map(|r| r.what), Some("destination"));
        let page = base + 0x4_0000;
        let (inside, below) = build_range_of(page);
        assert!(inside.is_none(), "past the destination's end");
        let (r, by) = below.expect("the destination below it");
        assert_eq!((r.what, by), ("destination", page - (base + 0x2_b800)));
        let (inside, _) = build_range_of(base + 0x10_0000);
        assert_eq!(inside.map(|r| r.what), Some("scratch"));
    }
}
