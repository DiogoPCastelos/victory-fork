//! A trace of the title's ray-tracing structure builds (`VICTORY_BVH_TRACE=1`,
//! off by default): the title calls the console builder's C++ class in
//! `XboxBvhBuilder.dll` (`BuildRaytracingAccelerationStructureBatched2X`,
//! `RehydrateRaytracingAccelerationStructureBatched2X`,
//! `GetRaytracingAccelerationStructurePrebuildInfo`,
//! `CopyRaytracingAccelerationStructure`). With the switch, those four imports
//! come here: each call is logged (what it builds, where, with what scratch
//! and what sizes) and goes on to the title DLL's own export, unchanged. The
//! DLL is still loaded as the title's ([`wraps`]). What the trace is for: which
//! calls run in a scene, whether a structure is a prebuilt one rehydrated or
//! a mesh built anew, and the real bounds of each destination and scratch
//! range against the first access a walker or a builder stage makes.
//!
//! The descriptors' layouts: the rehydrate and prebuild calls take the PC's
//! `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC` and `..._INPUTS`;
//! the batched build's `D3D12XBOX_..._BATCHED_DESC2` is not public, so its
//! first bytes are logged as words until its layout is known.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{HleExport, HleModule};

const DLL: &str = "xboxbvhbuilder.dll";

const BUILD: &str = "?BuildRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList6@@IPEBUD3D12XBOX_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_BATCHED_DESC2@@@Z";
const REHYDRATE: &str = "?RehydrateRaytracingAccelerationStructureBatched2X@XboxBvhBuilder@@QEAAJPEAUID3D12GraphicsCommandList8@@IPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC@@PEB_K@Z";
const PREBUILD: &str = "?GetRaytracingAccelerationStructurePrebuildInfo@XboxBvhBuilder@@QEAAXPEBUD3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS@@PEBUD3D12XBOX_XDXR_STREAM_DESC@@PEAUD3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO@@@Z";
const COPY: &str = "?CopyRaytracingAccelerationStructure@XboxBvhBuilder@@QEAAXPEAUID3D12GraphicsCommandList4@@_K1W4D3D12_RAYTRACING_ACCELERATION_STRUCTURE_COPY_MODE@@@Z";

/// Whether the trace is on (`VICTORY_BVH_TRACE=1`).
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_BVH_TRACE").is_some_and(|v| v == "1"))
}

/// Whether `dll` is one the trace only wraps: the title's own DLL is still
/// loaded, and the calls the trace does not take resolve to it.
pub fn wraps(dll: &str) -> bool {
    on() && dll.eq_ignore_ascii_case(DLL)
}

/// The title DLL's own export `name` (the DLL is mapped by then: the title
/// made its builder object with the DLL's constructor first).
fn original(name: &str) -> Option<usize> {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
    let base = crate::runtime::load_title_dll("XboxBvhBuilder.dll").ok().flatten()?;
    let mut cname = name.as_bytes().to_vec();
    cname.push(0);
    // SAFETY: a mapped module and a NUL-terminated name.
    let f = unsafe { GetProcAddress(base as *mut std::ffi::c_void, cname.as_ptr()) }?;
    Some(f as usize)
}

/// The original of each wrapped call, looked up once.
static ORIGINALS: [OnceLock<Option<usize>>; 4] = [const { OnceLock::new() }; 4];

fn original_of(k: usize, name: &str) -> Option<usize> {
    let found = *ORIGINALS[k].get_or_init(|| original(name));
    if found.is_none() {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| tracing::error!(name, "ray-tracing build trace: the title DLL's own export was not found; the call is dropped"));
    }
    found
}

/// Calls per kind since the start, and how many were logged in full (the
/// first [`FULL`] of each kind, then one line every 10 s with the counts).
static CALLS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
const FULL: u64 = 64;

fn full(k: usize) -> bool {
    let n = CALLS[k].fetch_add(1, Ordering::Relaxed);
    counts();
    n < FULL
}

fn counts() {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = now_ms();
    if now < LAST.load(Ordering::Relaxed) + 10_000 {
        return;
    }
    LAST.store(now, Ordering::Relaxed);
    tracing::info!(
        builds = CALLS[0].load(Ordering::Relaxed),
        rehydrates = CALLS[1].load(Ordering::Relaxed),
        prebuilds = CALLS[2].load(Ordering::Relaxed),
        copies = CALLS[3].load(Ordering::Relaxed),
        "ray-tracing build trace: calls since the start"
    );
}

fn now_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

/// `n` 64-bit words at `p` as hex, or a note if `p` is null.
fn words(p: *const u64, n: usize) -> String {
    if p.is_null() {
        return "null".to_owned();
    }
    // SAFETY: the title passes a descriptor of at least this size (the
    // batched descriptor's size is not public: `n` stays small).
    (0..n).map(|i| format!("{:#x}", unsafe { p.add(i).read_unaligned() })).collect::<Vec<_>>().join(" ")
}

/// `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_INPUTS` (PC layout): type
/// (0 top, 1 bottom), flags, number of descriptions, layout, then the
/// pointer to the instances or the geometry descriptions.
#[repr(C)]
struct Inputs {
    kind: u32,
    flags: u32,
    descs: u32,
    layout: u32,
    data: u64,
}

/// `D3D12_BUILD_RAYTRACING_ACCELERATION_STRUCTURE_DESC` (PC layout).
#[repr(C)]
struct BuildDesc {
    dest: u64,
    inputs: Inputs,
    source: u64,
    scratch: u64,
}

/// `D3D12_RAYTRACING_ACCELERATION_STRUCTURE_PREBUILD_INFO`.
#[repr(C)]
struct PrebuildInfo {
    result_max: u64,
    scratch: u64,
    update_scratch: u64,
}

fn inputs_text(i: &Inputs) -> String {
    let kind = match i.kind {
        0 => "top (instances)",
        1 => "bottom (geometry)",
        _ => "?",
    };
    format!("{kind} flags {:#x} descs {} layout {} data {:#x}", i.flags, i.descs, i.layout, i.data)
}

type BuildFn = unsafe extern "system" fn(usize, usize, u32, *const u64);
type RehydrateFn = unsafe extern "system" fn(usize, usize, u32, *const BuildDesc, *const u64) -> i32;
type PrebuildFn = unsafe extern "system" fn(usize, *const Inputs, *const u64, *mut PrebuildInfo);
type CopyFn = unsafe extern "system" fn(usize, usize, u64, u64, u32);

/// `XboxBvhBuilder::BuildRaytracingAccelerationStructureBatched2X(list, count, descs)`.
unsafe extern "system" fn build(this: usize, list: usize, count: u32, descs: *const u64) {
    if full(0) && !descs.is_null() {
        // The batched descriptor is not public; read from a run's words, it
        // repeats every 10 words (80 bytes): destination; type (low dword:
        // 0 top, 1 bottom) and flags (high dword); description count; their
        // address; a word; scratch; a word; a pointer; two words.
        for k in 0..count.min(16) as usize {
            // SAFETY: the title passes `count` descriptors of 10 words.
            let d = unsafe { descs.add(k * 10) };
            // SAFETY: as above.
            let w = |i: usize| unsafe { d.add(i).read_unaligned() };
            let kind = match w(1) & 0xffff_ffff {
                0 => "top (instances)",
                1 => "bottom (geometry)",
                _ => "?",
            };
            tracing::info!(
                "ray-tracing build: build {k} of {count}: dest {:#x} {kind} flags {:#x} descs {} at {:#x} scratch {:#x}; other words {:#x} {:#x} {:#x}",
                w(0),
                w(1) >> 32,
                w(2),
                w(3),
                w(5),
                w(4),
                w(6),
                w(7)
            );
        }
    }
    if let Some(f) = original_of(0, BUILD) {
        // SAFETY: the title DLL's own export, with the title's arguments.
        unsafe { std::mem::transmute::<usize, BuildFn>(f)(this, list, count, descs) }
    }
}

/// `XboxBvhBuilder::RehydrateRaytracingAccelerationStructureBatched2X(list, count, descs, sizes)`.
unsafe extern "system" fn rehydrate(this: usize, list: usize, count: u32, descs: *const BuildDesc, sizes: *const u64) -> i32 {
    let log = full(1);
    if log {
        for k in 0..count.min(8) as usize {
            // SAFETY: the title passes `count` descriptors.
            let d = unsafe { &*descs.add(k) };
            // SAFETY: as above, one per descriptor if given.
            let size = if sizes.is_null() { 0 } else { unsafe { sizes.add(k).read_unaligned() } };
            tracing::info!(
                "ray-tracing build: rehydrate {k} of {count}: dest {:#x} source {:#x} scratch {:#x}; {}; size {size:#x}",
                d.dest,
                d.source,
                d.scratch,
                inputs_text(&d.inputs)
            );
        }
    }
    let Some(f) = original_of(1, REHYDRATE) else { return -1 };
    // SAFETY: the title DLL's own export, with the title's arguments.
    let hr = unsafe { std::mem::transmute::<usize, RehydrateFn>(f)(this, list, count, descs, sizes) };
    if log && hr != 0 {
        tracing::warn!(hr = format_args!("{hr:#x}"), "ray-tracing build: rehydrate answered");
    }
    hr
}

/// `XboxBvhBuilder::GetRaytracingAccelerationStructurePrebuildInfo(inputs, stream, info)`.
unsafe extern "system" fn prebuild(this: usize, inputs: *const Inputs, stream: *const u64, info: *mut PrebuildInfo) {
    let Some(f) = original_of(2, PREBUILD) else { return };
    // SAFETY: the title DLL's own export, with the title's arguments.
    unsafe { std::mem::transmute::<usize, PrebuildFn>(f)(this, inputs, stream, info) };
    if full(2) && !inputs.is_null() && !info.is_null() {
        // SAFETY: the title's own structures, filled by the call.
        let (i, out) = unsafe { (&*inputs, &*info) };
        tracing::info!(
            "ray-tracing build: prebuild info for {}: result at most {:#x}, scratch {:#x}, update scratch {:#x}; stream {}",
            inputs_text(i),
            out.result_max,
            out.scratch,
            out.update_scratch,
            words(stream, 4)
        );
    }
}

/// `XboxBvhBuilder::CopyRaytracingAccelerationStructure(list, dest, source, mode)`.
unsafe extern "system" fn copy(this: usize, list: usize, dest: u64, source: u64, mode: u32) {
    if full(3) {
        let what = match mode {
            0 => "clone",
            1 => "compact",
            2 => "visualization",
            3 => "serialize",
            4 => "deserialize",
            _ => "?",
        };
        tracing::info!("ray-tracing build: copy ({what}) {source:#x} -> {dest:#x}");
    }
    if let Some(f) = original_of(3, COPY) {
        // SAFETY: the title DLL's own export, with the title's arguments.
        unsafe { std::mem::transmute::<usize, CopyFn>(f)(this, list, dest, source, mode) }
    }
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "bvhtrace",
        dlls: &["xboxbvhbuilder.dll"],
        exports: vec![
            HleExport { name: BUILD, address: build as *const () as usize },
            HleExport { name: REHYDRATE, address: rehydrate as *const () as usize },
            HleExport { name: PREBUILD, address: prebuild as *const () as usize },
            HleExport { name: COPY, address: copy as *const () as usize },
        ],
    }
}
