//! Far jumps in the dual-bank rewrite (`docs/wave-audit.md`): CPU against GPU.
//!
//! [`cpu`] runs a program's words, one console wave of 64 lanes at a time, for the small
//! part of GFX10 these programs use. It is written from the public RDNA 2 ISA and shares
//! nothing with the translator but the opcode tables (names to numbers). A lane value the
//! ISA leaves undefined (a `ds_bpermute` from a lane that is not active, a register never
//! written) is `None` and is not compared.
//!
//! Each test builds one program, runs it on the CPU for every case, then on the hardware
//! device in both 32-lane forms (paired waves and dual-bank), and compares every thread's
//! stored value. Without a hardware device the GPU half prints "NOT RUN" and passes: report
//! those as not run. One test per property the far-jump rewrite must keep: forward jumps,
//! backward jumps, SCC, its scratch SGPRs, divergent EXEC, branch relaxation over several
//! rounds, and several console waves in a group.

use std::collections::HashMap;

use super::*;
use crate::gfx10::{Encoding, opcode};

/// The CPU reference: one console wave of 64 lanes, instruction by instruction.
mod cpu {
    use std::collections::HashMap;

    use crate::gfx10::{Encoding, opcode};

    type Lanes = [Option<u32>; 64];

    /// The instructions this model knows, by (class, opcode).
    fn names() -> HashMap<(u8, u32), &'static str> {
        let known: &[(u8, Encoding, &[&'static str])] = &[
            (0, Encoding::Sopp, &[
                "s_nop", "s_waitcnt", "s_endpgm", "s_branch", "s_cbranch_scc0", "s_cbranch_scc1",
                "s_cbranch_vccz", "s_cbranch_vccnz", "s_cbranch_execz", "s_cbranch_execnz",
            ]),
            (1, Encoding::Sopc, &[
                "s_cmp_eq_u32", "s_cmp_lg_u32", "s_cmp_gt_u32", "s_cmp_ge_u32", "s_cmp_lt_u32", "s_cmp_le_u32",
            ]),
            (2, Encoding::Sop1, &[
                "s_mov_b32", "s_mov_b64", "s_not_b64", "s_getpc_b64", "s_setpc_b64", "s_and_saveexec_b64",
            ]),
            (3, Encoding::Sopk, &["s_movk_i32", "s_addk_i32"]),
            (4, Encoding::Sop2, &[
                "s_add_u32", "s_sub_u32", "s_addc_u32", "s_subb_u32", "s_cselect_b32", "s_and_b32", "s_or_b32",
                "s_and_b64", "s_or_b64", "s_xor_b64", "s_andn2_b64",
            ]),
            (5, Encoding::Vop1, &["v_mov_b32", "v_readfirstlane_b32"]),
            (6, Encoding::Vopc, &[
                "v_cmp_eq_u32", "v_cmp_ne_u32", "v_cmp_gt_u32", "v_cmp_lt_u32", "v_cmp_ge_u32", "v_cmp_le_u32",
                "v_cmpx_eq_u32", "v_cmpx_ne_u32", "v_cmpx_gt_u32", "v_cmpx_lt_u32", "v_cmpx_ge_u32", "v_cmpx_le_u32",
            ]),
            (7, Encoding::Vop2, &[
                "v_add_nc_u32", "v_sub_nc_u32", "v_xor_b32", "v_and_b32", "v_or_b32", "v_lshlrev_b32", "v_lshrrev_b32",
                "v_cndmask_b32",
            ]),
            (8, Encoding::Vop3, &["v_readlane_b32", "v_writelane_b32"]),
            (9, Encoding::Ds, &["ds_bpermute_b32"]),
            (10, Encoding::Mubuf, &["buffer_store_dword"]),
        ];
        let mut out = HashMap::new();
        for &(class, e, list) in known {
            for &n in list {
                out.insert((class, u32::from(opcode(e, n).unwrap_or_else(|| panic!("{n}")))), n);
            }
        }
        out
    }

    struct Wave {
        s: [u32; 128],
        scc: bool,
        v: Vec<Lanes>,
    }

    impl Wave {
        fn mask(&self, code: usize) -> u64 {
            u64::from(self.s[code]) | u64::from(self.s[code + 1]) << 32
        }
        fn set_mask(&mut self, code: usize, m: u64) {
            self.s[code] = m as u32;
            self.s[code + 1] = (m >> 32) as u32;
        }
        fn exec(&self) -> u64 {
            self.mask(126)
        }
        fn sget(&self, code: u32, lit: u32) -> u32 {
            match code {
                0..=124 | 126 | 127 => self.s[code as usize],
                125 => 0,
                128..=192 => code - 128,
                193..=208 => (192i32 - code as i32) as u32,
                253 => u32::from(self.scc),
                255 => lit,
                _ => panic!("cpu model: scalar operand {code}"),
            }
        }
        fn sget64(&self, code: u32, lit: u32) -> u64 {
            match code {
                0..=124 | 126 => self.mask(code as usize),
                128..=208 => self.sget(code, lit) as i32 as i64 as u64,
                _ => panic!("cpu model: 64-bit operand {code}"),
            }
        }
        fn vget(&self, code: u32, lit: u32, lane: usize) -> Option<u32> {
            if code >= 256 { self.v[code as usize - 256][lane] } else { Some(self.sget(code, lit)) }
        }
    }

    /// Runs `code` (a whole program, words) for a group of `threads` threads, with s0..s3
    /// set; the dwords stored (`buffer_store_dword`), `None` where nothing was stored or the
    /// value is undefined.
    pub fn run(code: &[u32], threads: u32, s: [u32; 4]) -> Vec<Option<u32>> {
        let names = names();
        let mut mem = vec![None; threads.max(64) as usize];
        for wave in 0..threads.div_ceil(64) {
            run_wave(code, &names, wave, threads, s, &mut mem);
        }
        mem
    }

    fn run_wave(
        code: &[u32],
        names: &HashMap<(u8, u32), &'static str>,
        wave: u32,
        threads: u32,
        s4: [u32; 4],
        mem: &mut [Option<u32>],
    ) {
        let mut w = Wave { s: [0; 128], scc: false, v: vec![[None; 64]; 256] };
        w.s[..4].copy_from_slice(&s4);
        let lanes = (threads - 64 * wave).min(64);
        w.set_mask(126, if lanes == 64 { u64::MAX } else { (1u64 << lanes) - 1 });
        for l in 0..lanes as usize {
            w.v[0][l] = Some(64 * wave + l as u32);
        }
        let mut pc = 0usize;
        for _ in 0..50_000_000u64 {
            let i = code[pc];
            let lit = code.get(pc + 1).copied().unwrap_or(0);
            let (class, opn) = if i >> 23 == 0x17F {
                (0, i >> 16 & 0x7F)
            } else if i >> 23 == 0x17E {
                (1, i >> 16 & 0x7F)
            } else if i >> 23 == 0x17D {
                (2, i >> 8 & 0xFF)
            } else if i >> 28 == 0xB {
                (3, i >> 23 & 0x1F)
            } else if i >> 30 == 2 {
                (4, i >> 23 & 0x7F)
            } else if i >> 26 == 0x3D {
                // (the scalar load of the store's descriptor: nothing to model)
                pc += 2;
                continue;
            } else if i >> 25 == 0x3F {
                (5, i >> 9 & 0xFF)
            } else if i >> 25 == 0x3E {
                (6, i >> 17 & 0xFF)
            } else if i >> 31 == 0 {
                (7, i >> 25 & 0x3F)
            } else if i >> 26 == 0x35 {
                (8, i >> 16 & 0x3FF)
            } else if i >> 26 == 0x36 {
                (9, i >> 18 & 0xFF)
            } else if i >> 26 == 0x38 {
                (10, i >> 18 & 0x7F)
            } else {
                panic!("cpu model: word {i:#010x} at {pc}");
            };
            let name = *names.get(&(class, opn)).unwrap_or_else(|| panic!("cpu model: class {class} op {opn:#x} at {pc}"));
            let exec = w.exec();
            let active = |l: usize| exec >> l & 1 != 0;
            match class {
                0 => {
                    let to = (pc as i64 + 1 + i64::from(i as u16 as i16)) as usize;
                    let vcc = w.mask(106);
                    let taken = match name {
                        "s_endpgm" => return,
                        "s_branch" => true,
                        "s_cbranch_scc0" => !w.scc,
                        "s_cbranch_scc1" => w.scc,
                        "s_cbranch_vccz" => vcc == 0,
                        "s_cbranch_vccnz" => vcc != 0,
                        "s_cbranch_execz" => exec == 0,
                        "s_cbranch_execnz" => exec != 0,
                        _ => false,
                    };
                    pc = if taken { to } else { pc + 1 };
                }
                1 => {
                    let (a, b) = (w.sget(i & 0xFF, lit), w.sget(i >> 8 & 0xFF, lit));
                    w.scc = match name {
                        "s_cmp_eq_u32" => a == b,
                        "s_cmp_lg_u32" => a != b,
                        "s_cmp_gt_u32" => a > b,
                        "s_cmp_ge_u32" => a >= b,
                        "s_cmp_lt_u32" => a < b,
                        _ => a <= b,
                    };
                    pc += 1 + usize::from(i & 0xFF == 255 || i >> 8 & 0xFF == 255);
                }
                2 => {
                    let (d, src) = ((i >> 16 & 0x7F) as usize, i & 0xFF);
                    let len = 1 + usize::from(src == 255);
                    match name {
                        "s_mov_b32" => w.s[d] = w.sget(src, lit),
                        "s_mov_b64" => w.set_mask(d, w.sget64(src, lit)),
                        "s_not_b64" => {
                            let r = !w.sget64(src, lit);
                            w.set_mask(d, r);
                            w.scc = r != 0;
                        }
                        "s_getpc_b64" => w.set_mask(d, 4 * (pc as u64 + 1)),
                        "s_setpc_b64" => {
                            let to = w.sget64(src, lit);
                            assert!(to.is_multiple_of(4) && (to / 4) < code.len() as u64, "cpu model: s_setpc_b64 to {to:#x}");
                            pc = (to / 4) as usize;
                            continue;
                        }
                        _ => {
                            // s_and_saveexec_b64
                            let m = w.sget64(src, lit) & exec;
                            w.set_mask(d, exec);
                            w.set_mask(126, m);
                            w.scc = m != 0;
                        }
                    }
                    pc += len;
                }
                3 => {
                    let d = (i >> 16 & 0x7F) as usize;
                    let k = i32::from(i as u16 as i16);
                    match name {
                        "s_movk_i32" => w.s[d] = k as u32,
                        _ => {
                            let (r, o) = (w.s[d] as i32).overflowing_add(k);
                            w.s[d] = r as u32;
                            w.scc = o;
                        }
                    }
                    pc += 1;
                }
                4 => {
                    let (d, s0, s1) = ((i >> 16 & 0x7F) as usize, i & 0xFF, i >> 8 & 0xFF);
                    let len = 1 + usize::from(s0 == 255 || s1 == 255);
                    let (a, b) = (w.sget(s0, lit), w.sget(s1, lit));
                    let c = u64::from(w.scc);
                    match name {
                        "s_add_u32" | "s_addc_u32" => {
                            let r = u64::from(a) + u64::from(b) + if name == "s_addc_u32" { c } else { 0 };
                            w.s[d] = r as u32;
                            w.scc = r >> 32 != 0;
                        }
                        "s_sub_u32" | "s_subb_u32" => {
                            let borrow = if name == "s_subb_u32" { c } else { 0 };
                            w.s[d] = a.wrapping_sub(b).wrapping_sub(borrow as u32);
                            w.scc = u64::from(b) + borrow > u64::from(a);
                        }
                        "s_cselect_b32" => w.s[d] = if w.scc { a } else { b },
                        "s_and_b32" | "s_or_b32" => {
                            let r = if name == "s_and_b32" { a & b } else { a | b };
                            w.s[d] = r;
                            w.scc = r != 0;
                        }
                        _ => {
                            let (a, b) = (w.sget64(s0, lit), w.sget64(s1, lit));
                            let r = match name {
                                "s_and_b64" => a & b,
                                "s_or_b64" => a | b,
                                "s_xor_b64" => a ^ b,
                                _ => a & !b,
                            };
                            w.set_mask(d, r);
                            w.scc = r != 0;
                        }
                    }
                    pc += len;
                }
                5 => {
                    let (d, src) = ((i >> 17 & 0xFF) as usize, i & 0x1FF);
                    if name == "v_readfirstlane_b32" {
                        assert!(exec != 0, "cpu model: v_readfirstlane_b32 with EXEC empty");
                        let l = exec.trailing_zeros() as usize;
                        w.s[d] = w.vget(src, lit, l).expect("cpu model: an undefined value into an SGPR");
                    } else {
                        for l in (0..64).filter(|&l| active(l)) {
                            w.v[d][l] = w.vget(src, lit, l);
                        }
                    }
                    pc += 1 + usize::from(src == 255);
                }
                6 => {
                    let (src0, src1) = (i & 0x1FF, 256 + (i >> 9 & 0xFF));
                    let mut m = 0u64;
                    for l in (0..64).filter(|&l| active(l)) {
                        let a = w.vget(src0, lit, l).expect("cpu model: compare of an undefined value");
                        let b = w.vget(src1, lit, l).expect("cpu model: compare of an undefined value");
                        let p = match &name[name.len() - 6..] {
                            "eq_u32" => a == b,
                            "ne_u32" => a != b,
                            "gt_u32" => a > b,
                            "lt_u32" => a < b,
                            "ge_u32" => a >= b,
                            _ => a <= b,
                        };
                        m |= u64::from(p) << l;
                    }
                    // (GFX10: v_cmpx writes EXEC only)
                    w.set_mask(if name.starts_with("v_cmpx") { 126 } else { 106 }, m);
                    pc += 1 + usize::from(src0 == 255);
                }
                7 => {
                    let (d, src0, src1) = ((i >> 17 & 0xFF) as usize, i & 0x1FF, 256 + (i >> 9 & 0xFF));
                    let vcc = w.mask(106);
                    for l in (0..64).filter(|&l| active(l)) {
                        let (a, b) = (w.vget(src0, lit, l), w.vget(src1, lit, l));
                        w.v[d][l] = match name {
                            "v_cndmask_b32" => {
                                if vcc >> l & 1 != 0 { b } else { a }
                            }
                            _ => a.zip(b).map(|(a, b)| match name {
                                "v_add_nc_u32" => a.wrapping_add(b),
                                "v_sub_nc_u32" => a.wrapping_sub(b),
                                "v_xor_b32" => a ^ b,
                                "v_and_b32" => a & b,
                                "v_or_b32" => a | b,
                                "v_lshlrev_b32" => b << (a & 31),
                                _ => b >> (a & 31),
                            }),
                        };
                    }
                    pc += 1 + usize::from(src0 == 255);
                }
                8 => {
                    let (d, x) = ((i & 0xFF) as usize, lit);
                    let (src0, src1) = (x & 0x1FF, x >> 9 & 0x1FF);
                    assert!(src0 != 255 && src1 != 255, "cpu model: a VOP3 literal");
                    let lane = (w.sget(src1, 0) & 63) as usize;
                    if name == "v_readlane_b32" {
                        w.s[d] = w.vget(src0, 0, lane).expect("cpu model: an undefined value into an SGPR");
                    } else {
                        w.v[d][lane] = Some(w.sget(src0, 0));
                    }
                    pc += 2;
                }
                9 => {
                    let offset = i & 0xFFFF;
                    let (addr, data, d) = ((lit & 0xFF) as usize, (lit >> 8 & 0xFF) as usize, (lit >> 24) as usize);
                    let (a, x) = (w.v[addr], w.v[data]);
                    for l in (0..64).filter(|&l| active(l)) {
                        w.v[d][l] = a[l].and_then(|a| {
                            let src = (a.wrapping_add(offset) >> 2 & 63) as usize;
                            if active(src) { x[src] } else { None }
                        });
                    }
                    pc += 2;
                }
                _ => {
                    let (offen, offset) = (i >> 12 & 1 != 0, i & 0xFFF);
                    let (vaddr, vdata, soff) = ((lit & 0xFF) as usize, (lit >> 8 & 0xFF) as usize, lit >> 24);
                    let soff = w.sget(soff, 0);
                    for l in (0..64).filter(|&l| active(l)) {
                        let base = if offen { w.v[vaddr][l].expect("cpu model: an undefined address") } else { 0 };
                        let at = (base + offset + soff) as usize / 4;
                        mem[at] = w.v[vdata][l];
                    }
                    pc += 2;
                }
            }
        }
        panic!("cpu model: the program did not end");
    }
}

fn o(e: Encoding, name: &str) -> u32 {
    u32::from(opcode(e, name).unwrap_or_else(|| panic!("{name}")))
}
fn sop1n(name: &str, sdst: u32, ssrc0: u32) -> u32 {
    0xBE80_0000 | sdst << 16 | o(Encoding::Sop1, name) << 8 | ssrc0
}
fn sop2n(name: &str, sdst: u32, ssrc0: u32, ssrc1: u32) -> u32 {
    0x8000_0000 | o(Encoding::Sop2, name) << 23 | sdst << 16 | ssrc1 << 8 | ssrc0
}
fn sopc(name: &str, ssrc0: u32, ssrc1: u32) -> u32 {
    0xBF00_0000 | o(Encoding::Sopc, name) << 16 | ssrc1 << 8 | ssrc0
}
fn sopk(name: &str, sdst: u32, imm: i16) -> u32 {
    0xB000_0000 | o(Encoding::Sopk, name) << 23 | sdst << 16 | u32::from(imm as u16)
}
fn sopp(name: &str) -> u32 {
    0xBF80_0000 | o(Encoding::Sopp, name) << 16
}
fn vopcn(name: &str, src0: u32, vsrc1: u32) -> u32 {
    vopc(o(Encoding::Vopc, name), src0, vsrc1)
}
const S_NOP: u32 = 0xBF80_0000;
const MINUS_ONE: u32 = 193;

/// A console program after [`prologue`], with branches to labels filled in at the end.
struct Asm {
    w: Vec<u32>,
    labels: HashMap<&'static str, usize>,
    /// SOPP branches: (word, label)
    near: Vec<(usize, &'static str)>,
    /// The compiler's far jumps: (getpc word, label, backward form subtracts)
    far: Vec<(usize, &'static str, bool)>,
}

impl Asm {
    fn new() -> Self {
        let mut w = prologue();
        w.push(vop1(V_MOV_B32, 12, v(1))); // x = 0x1000 + thread
        Asm { w, labels: HashMap::new(), near: Vec::new(), far: Vec::new() }
    }
    fn at(&mut self, label: &'static str) {
        self.labels.insert(label, self.w.len());
    }
    fn br(&mut self, name: &str, label: &'static str) {
        self.near.push((self.w.len(), label));
        self.w.push(sopp(name));
    }
    /// The compiler's far jump in s[30:31]; a backward one subtracts (`sub`) or adds a
    /// negative offset with a carry of -1.
    fn far_jump(&mut self, label: &'static str, sub: bool) {
        self.far.push((self.w.len(), label, sub));
        // (the add or sub, and the carry's sign, are set in `finish`)
        self.w.extend([sop1n("s_getpc_b64", 30, 0), 0, 0, 0, sop1n("s_setpc_b64", 0, 30)]);
    }
    /// `n` adds of `k` to x.
    fn adds(&mut self, n: usize, k: u32) {
        self.w.extend((0..n).map(|_| vop2(V_ADD_NC_U32, 12, int(k), 12)));
    }
    fn nops(&mut self, n: usize) {
        self.w.extend(std::iter::repeat_n(S_NOP, n));
    }
    fn finish(mut self) -> Vec<u32> {
        epilogue(&mut self.w);
        for (at, label) in self.near {
            let to = self.labels[label] as i64 - (at as i64 + 1);
            let imm = i16::try_from(to).unwrap_or_else(|_| panic!("the test's own branch to {label} is out of reach"));
            self.w[at] |= u32::from(imm as u16);
        }
        for (getpc, label, sub) in self.far {
            let delta = 4 * (self.labels[label] as i64 - (getpc as i64 + 1));
            let (add, literal, carry) = if delta >= 0 {
                (sop2n("s_add_u32", 30, 30, LITERAL), delta as u32, sop2n("s_addc_u32", 31, 31, int(0)))
            } else if sub {
                (sop2n("s_sub_u32", 30, 30, LITERAL), (-delta) as u32, sop2n("s_subb_u32", 31, 31, int(0)))
            } else {
                (sop2n("s_add_u32", 30, 30, LITERAL), delta as i32 as u32, sop2n("s_addc_u32", 31, 31, MINUS_ONE))
            };
            self.w[getpc + 1..getpc + 4].copy_from_slice(&[add, literal, carry]);
        }
        self.w
    }
}

/// Instructions in a region the dual-bank rewrite makes too long for a 16-bit branch (each
/// vector instruction is written about four times as long).
const N: usize = 9_000;

/// The program on the CPU and on the hardware device (paired and dual-bank) for every case.
/// Where its per-lane branches are not the console's ([`crate::lane_exact`]), neither form is
/// exact as the translator takes it (both structure lane-mask branches per lane): the paired
/// form is not run, and the dual-bank one is run and its differences reported, not asserted
/// (a known gap, `docs/wave-audit.md`). Returns whether they are not.
fn differential(what: &str, w: &[u32], threads: u32, cases: &[[u32; 4]]) -> bool {
    let native = native_of(w, true, 64);
    let code: Vec<u32> = native.code.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
    let p = crate::ir::build(&native.code, true, native.vgprs).expect("ir::build");
    let st = crate::structure::structure(&p).expect("structure");
    let inexact = crate::lane_exact::inexact(&p, &st);
    let wants: Vec<Vec<Option<u32>>> = cases.iter().map(|&s| cpu::run(&code, threads, s)).collect();
    for want in &wants {
        assert!(want.iter().take(threads as usize).all(Option::is_some) || !threads.is_multiple_of(64), "{what}: the model stores every thread");
    }
    let _one = one_at_a_time();
    // (both forms run before failing: which of them differs is the finding)
    let mut failed = Vec::new();
    for dual in [false, true] {
        if !dual && !inexact.is_empty() {
            eprintln!("{what} [paired]: NOT RUN, per-lane branches not the console's ({inexact:?})");
            continue;
        }
        DUAL.set(dual);
        let card = card_of(what, w, threads);
        DUAL.set(false);
        let Some(card) = card else { return !inexact.is_empty() };
        let form = if dual { "dual-bank" } else { "paired" };
        let mut bad = 0;
        for (s, want) in cases.iter().zip(&wants) {
            let got = card.run(*s);
            let wrong: Vec<String> = want
                .iter()
                .enumerate()
                .filter_map(|(t, w)| match w {
                    Some(w) if got[t] != *w => Some(format!("thread {t}: got {:#x}, want {w:#x}", got[t])),
                    _ => None,
                })
                .take(6)
                .collect();
            if !wrong.is_empty() {
                bad += 1;
                failed.push(format!("{what} [{form}], s {s:?}: {}", wrong.join("; ")));
            }
        }
        eprintln!("{what} [{form}]: {} of {} cases as the CPU model", cases.len() - bad, cases.len());
    }
    if inexact.is_empty() {
        assert!(failed.is_empty(), "{}", failed.join("\n"));
    } else if !failed.is_empty() {
        eprintln!("{what}: KNOWN, per-lane branches not the console's:\n{}", failed.join("\n"));
    }
    !inexact.is_empty()
}

/// The dual-bank rewrite of `w`: how many times it was done, and its `s_setpc_b64`s.
fn rewritten(w: &[u32], threads: u32) -> (usize, Vec<crate::gfx10::Instruction>) {
    let mut native = native_of(w, true, 64);
    if let crate::native::StageIo::Cs(cs) = &mut native.io {
        cs.threads = [threads, 1, 1];
    }
    let (p, rounds) = crate::dualbank::transform_in_rounds(&native).expect("rewritten");
    let insts = crate::gfx10::decode(&p.code).expect("decodes");
    (rounds, insts)
}

fn setpcs(insts: &[crate::gfx10::Instruction]) -> usize {
    insts.iter().filter(|i| i.name() == Some("s_setpc_b64")).count()
}

/// Forward: `s_cbranch_scc1`, `s_cbranch_scc0`, an if/else whose then-part ends in an
/// `s_branch` over the else-part, and the compiler's far jump over code that must not run.
#[test]
fn far_jumps_forward() {
    let mut a = Asm::new();
    a.w.push(sopc("s_cmp_eq_u32", 0, int(0)));
    a.br("s_cbranch_scc1", "a");
    a.adds(N, 1);
    a.at("a");
    a.w.push(sopc("s_cmp_lg_u32", 0, int(1)));
    a.br("s_cbranch_scc0", "b");
    a.adds(N, 2);
    a.at("b");
    a.w.push(sopc("s_cmp_eq_u32", 0, int(2)));
    a.br("s_cbranch_scc0", "else");
    a.adds(N, 4);
    a.br("s_branch", "end");
    a.at("else");
    a.adds(N, 8);
    a.at("end");
    a.far_jump("past", false);
    a.w.push(vop1(V_MOV_B32, 12, LITERAL)); // must not run
    a.w.push(0xBAD0_0000);
    a.at("past");
    let w = a.finish();
    let (_, insts) = rewritten(&w, 64);
    // the three conditionals and the s_branch made far, and the program's own kept
    assert_eq!(setpcs(&insts), 5, "far jumps in the rewritten program");
    differential("far jumps forward", &w, 64, &[[0, 0, 0, 0], [1, 0, 0, 0], [2, 0, 0, 0], [3, 0, 0, 0]]);
}

/// Backward: a loop closed by `s_cbranch_scc1` (s0 times), one closed by the compiler's far
/// jump subtracting (s1 times), one closed by `s_branch` and left by a forward
/// `s_cbranch_scc1` at its top (s1 times), and a short one closed by a far jump adding a
/// negative offset (s0 times).
#[test]
fn far_jumps_backward() {
    let mut a = Asm::new();
    a.w.push(sop1n("s_mov_b32", 7, 0));
    a.at("l1");
    a.adds(N, 1);
    a.w.push(sop2n("s_sub_u32", 7, 7, int(1)));
    a.w.push(sopc("s_cmp_lg_u32", 7, int(0)));
    a.br("s_cbranch_scc1", "l1");

    a.w.push(sop1n("s_mov_b32", 7, 1));
    a.at("l2");
    a.adds(N, 2);
    a.w.push(sop2n("s_sub_u32", 7, 7, int(1)));
    a.w.push(sopc("s_cmp_eq_u32", 7, int(0)));
    a.br("s_cbranch_scc1", "out2");
    a.far_jump("l2", true);
    a.at("out2");

    a.w.push(sop1n("s_mov_b32", 7, 1));
    a.at("l3");
    a.w.push(sopc("s_cmp_eq_u32", 7, int(0)));
    a.br("s_cbranch_scc1", "out3");
    a.adds(N, 4);
    a.w.push(sop2n("s_sub_u32", 7, 7, int(1)));
    a.br("s_branch", "l3");
    a.at("out3");

    a.w.push(sop1n("s_mov_b32", 7, 0));
    a.at("l4");
    a.adds(3, 8);
    a.w.push(sop2n("s_sub_u32", 7, 7, int(1)));
    a.w.push(sopc("s_cmp_eq_u32", 7, int(0)));
    a.br("s_cbranch_scc1", "out4");
    a.far_jump("l4", false);
    a.at("out4");
    let w = a.finish();
    let (_, insts) = rewritten(&w, 64);
    // made far: l1's and l3's closing branches and l3's exit; kept: the two of the program
    assert_eq!(setpcs(&insts), 5, "far jumps in the rewritten program");
    differential("far jumps backward", &w, 64, &[[1, 1, 0, 0], [2, 1, 0, 0], [3, 2, 0, 0], [1, 3, 0, 0]]);
}

/// SCC across far jumps: set by a carry, then read (`s_cselect_b32`, `s_addc_u32`) where a
/// far `s_cbranch_scc1` lands and where it falls through; across a far `s_branch` at the end
/// of a then-part (SCC set just before it); and across a far `s_cbranch_execz` (the rewrite's
/// lane test is vector code, SCC must survive it too).
#[test]
fn far_jumps_keep_scc() {
    let mut a = Asm::new();
    a.w.push(sop2n("s_add_u32", 7, 0, 1)); // SCC = carry of s0 + s1
    a.br("s_cbranch_scc1", "b1");
    a.adds(N, 1);
    a.at("b1");
    a.w.push(sop2n("s_cselect_b32", 40, int(3), int(5)));
    a.w.push(vop2(V_ADD_NC_U32, 12, 40, 12));
    a.w.push(sop2n("s_add_u32", 7, 0, 1)); // the carry again
    a.w.push(sop2n("s_addc_u32", 41, int(0), int(0))); // s41 = SCC, SCC = 0
    a.w.push(sop2n("s_add_u32", 7, 0, 1));
    a.br("s_cbranch_scc0", "b2"); // far, taken without the carry
    a.adds(N, 2);
    a.at("b2");
    a.w.push(sop2n("s_addc_u32", 42, int(0), int(0))); // s42 = SCC
    a.w.push(sopk("s_movk_i32", 43, 0));

    a.w.push(sopc("s_cmp_gt_u32", 2, 3));
    a.br("s_cbranch_scc0", "else");
    a.adds(N, 4);
    a.w.push(sopc("s_cmp_eq_u32", 0, int(0))); // SCC across the s_branch
    a.br("s_branch", "end");
    a.at("else");
    a.adds(N, 8);
    a.at("end");
    a.w.push(sop2n("s_cselect_b32", 43, int(7), int(11)));

    a.w.push(sopc("s_cmp_lt_u32", 1, 0));
    a.w.push(vopcn("v_cmpx_lt_u32", 2, 0)); // exec: threads over s2
    a.br("s_cbranch_execz", "c");
    a.br("s_branch", "c0"); // (a branch inside: not a skip the rewrite leaves out)
    a.at("c0");
    a.adds(N, 16);
    a.at("c");
    a.w.push(sop2n("s_cselect_b32", 44, int(13), int(17)));
    a.w.push(sop1n("s_mov_b64", EXEC, 22));
    for s in [40u32, 41, 42, 43, 44] {
        a.w.push(vop2(V_LSHLREV_B32, 12, int(2), 12));
        a.w.push(vop2(V_ADD_NC_U32, 12, s, 12));
    }
    let w = a.finish();
    let (_, insts) = rewritten(&w, 64);
    assert_eq!(setpcs(&insts), 5, "far jumps in the rewritten program");
    let big = 0xFFFF_FFF0;
    differential(
        "far jumps keep SCC",
        &w,
        64,
        &[[0, 0, 5, 1], [big, 0x20, 1, 5], [big, 0x10, 0, 0], [3, big, 63, 2], [0, 0, 64, 64], [1, 2, 32, 7]],
    );
}

/// The program's values in every SGPR it names must survive the rewrite's far jumps, which
/// use SGPRs of their own: from ttmp0 when the program names no trap temporary (values in
/// s24-s105, VCC and M0 here), else past the highest SGPR it names (s24-s59 and ttmp0-1).
/// A program that names s104 and a trap temporary leaves none: refused.
#[test]
fn far_jumps_use_sgprs_the_program_does_not() {
    const M0: u32 = 124;
    let program = |names: &[u32]| {
        let mut a = Asm::new();
        for &s in names {
            a.w.push(sop2n("s_add_u32", s, 0, LITERAL));
            a.w.push(0x5000 + 7 * s);
        }
        a.w.push(sopc("s_cmp_eq_u32", 1, int(0)));
        a.br("s_cbranch_scc1", "a");
        a.adds(N, 1);
        a.at("a");
        a.w.push(vopcn("v_cmpx_lt_u32", 2, 0));
        a.br("s_cbranch_execz", "b");
        a.br("s_branch", "b0");
        a.at("b0");
        a.adds(N, 2);
        a.at("b");
        a.w.push(sop1n("s_mov_b64", EXEC, 22));
        for &s in names {
            a.w.push(vop2(V_LSHLREV_B32, 12, int(1), 12));
            a.w.push(vop2(V_XOR_B32, 12, s, 12));
        }
        a.finish()
    };
    let wide: Vec<u32> = (24..=105).chain([VCC_LO, VCC_HI, M0]).collect();
    let past: Vec<u32> = (24..=59).chain([108, 109]).collect();
    for (what, names, first) in [("no trap temporaries named", &wide, 108u32), ("ttmp0-1 named", &past, 60)] {
        let w = program(names);
        let (_, insts) = rewritten(&w, 64);
        // the rewrite's far jumps: (s_cselect_b32 sa), s_getpc_b64 st
        let mut theirs = Vec::new();
        for (n, i) in insts.iter().enumerate() {
            if i.name() == Some("s_getpc_b64") {
                let st = i.words[0] >> 16 & 0x7F;
                theirs.extend([st, st + 1]);
                if let Some(p) = n.checked_sub(1).map(|p| &insts[p]).filter(|p| p.name() == Some("s_cselect_b32")) {
                    theirs.push(p.words[0] >> 16 & 0x7F);
                }
            }
        }
        assert_eq!(theirs.len(), 6, "{what}: two far jumps of the rewrite's own");
        assert!(theirs.iter().all(|s| !names.contains(s) && *s >= first && *s < 124), "{what}: {theirs:?}");
        differential(&format!("far jumps' SGPRs, {what}"), &w, 64, &[[1, 0, 0, 0], [0xFFFF_0000, 1, 40, 0], [9, 0, 64, 0]]);
    }
    let w = program(&[24, 104, 108]);
    let mut native = native_of(&w, true, 64);
    native.vgprs = 16;
    let e = crate::dualbank::transform(&native).expect_err("refused");
    assert!(e.to_string().contains("no spare SGPRs"), "{e}");
}

/// Divergent EXEC: far `s_cbranch_execz`, `s_cbranch_vccz`, `s_cbranch_execnz` and
/// `s_cbranch_vccnz` under masks active in one half only, lane 31 or 32 only, none, all and
/// at random (the rewrite must test both halves of the console mask), with scalar code in the
/// skipped regions; then lane g takes lane g ^ 32's value.
#[test]
fn far_jumps_under_divergent_exec() {
    let w = divergent_program(N);
    let (_, insts) = rewritten(&w, 64);
    assert_eq!(setpcs(&insts), 4, "far jumps in the rewritten program");
    assert!(differential("far jumps under divergent EXEC", &w, 64, &divergent_cases()), "the per-lane branches found not the console's");
}

/// The same with short regions (no far jumps): whether a difference is the far jumps'.
#[test]
fn whole_wave_mask_branches_without_far_jumps() {
    assert!(differential("whole-wave mask branches", &divergent_program(3), 64, &divergent_cases()), "the per-lane branches found not the console's");
}

fn divergent_program(n: usize) -> Vec<u32> {
    let mut a = Asm::new();
    a.w.push(sopk("s_movk_i32", 40, 0));
    a.w.push(sopk("s_movk_i32", 41, 0));
    a.w.push(sop1n("s_mov_b64", EXEC, 2));
    a.br("s_cbranch_execz", "a");
    a.br("s_branch", "a0");
    a.at("a0");
    a.adds(n, 1);
    a.at("a");
    a.w.push(sop1n("s_mov_b64", EXEC, 22));
    a.w.push(vopcn("v_cmp_gt_u32", 1, 0)); // vcc: threads under s1
    a.br("s_cbranch_vccz", "b");
    a.adds(n, 2);
    a.at("b");
    a.w.push(sop1n("s_mov_b64", EXEC, 2));
    a.br("s_cbranch_execnz", "c");
    a.w.push(sopk("s_movk_i32", 40, 1)); // runs only with no lane active
    a.adds(n, 4);
    a.at("c");
    a.w.push(vopcn("v_cmp_lt_u32", 0, 0)); // vcc: active threads over s0
    a.br("s_cbranch_vccnz", "d");
    a.w.push(sopk("s_movk_i32", 41, 1));
    a.adds(n, 8);
    a.at("d");
    a.w.push(sop1n("s_mov_b64", EXEC, 22));
    a.w.push(vop2(V_LSHLREV_B32, 12, int(2), 12));
    a.w.push(vop2(V_ADD_NC_U32, 12, 40, 12));
    a.w.push(vop2(V_LSHLREV_B32, 12, int(1), 12));
    a.w.push(vop2(V_ADD_NC_U32, 12, 41, 12));
    a.w.push(vop2(V_XOR_B32, 2, int(32), 0)); // v2 = (lane ^ 32) * 4
    a.w.push(vop2(V_LSHLREV_B32, 2, int(2), 2));
    a.w.extend(ds(DS_BPERMUTE_B32, 12, 2, 12));
    a.w.push(S_WAITCNT_LGKM0);
    a.finish()
}

fn divergent_cases() -> Vec<[u32; 4]> {
    let mut cases = vec![
        [31, 0, 0, 0],
        [63, 64, u32::MAX, u32::MAX],
        [64, 32, 0x8000_0000, 0],
        [0, 33, 0, 1],
        [31, 1, 1, 0],
        [32, 0, 0, 0x8000_0000],
        [63, 31, 0xFFFF_FFFF, 0],
        [62, 63, 0, 0xFFFF_FFFF],
    ];
    let mut rng = Rng(0x5EED_FA12_0000_0001);
    for _ in 0..8 {
        let m = rng.next();
        cases.push([(rng.next() % 65) as u32, (rng.next() % 65) as u32, m as u32, (m >> 32) as u32]);
    }
    cases
}

/// Branch relaxation over several rounds: only `b1` is out of reach after the first rewrite
/// (its region grows); making it far puts the unconditional `b2` (whose region holds `b1`
/// and is otherwise copied word for word) out of reach, and that the conditional `b3`
/// (holding `b2`). Four rewrites, three far jumps; then the paths on the CPU and the GPU.
/// (The padding is `s_nop`s, all reachable: the rewrite leaves out code nothing reaches.)
#[test]
fn far_jumps_relax_in_rounds() {
    // offsets just short of the limit: one far jump inside each is enough (a conditional
    // branch made far grows by 7 words, an unconditional one by 6)
    const OFF: usize = 32_767 - 6; // b2: holds b1 (conditional)
    const OFF3: usize = 32_767 - 5; // b3: holds b2 (unconditional)
    let (b, c) = (30_000, OFF - 3 - 30_000);
    let mut a = Asm::new();
    a.w.push(sopk("s_movk_i32", 40, 0));
    a.w.push(sopc("s_cmp_eq_u32", 0, int(0)));
    a.br("s_cbranch_scc1", "t3"); // b3
    a.nops(OFF3 - 1);
    a.br("s_branch", "t2"); // b2
    a.at("t3");
    a.nops(b);
    a.w.push(sopk("s_addk_i32", 40, 100));
    a.w.push(sopc("s_cmp_eq_u32", 1, int(0)));
    a.br("s_cbranch_scc1", "t1"); // b1
    a.nops(c);
    a.at("t2");
    a.w.push(sopk("s_addk_i32", 40, 1000));
    a.adds(N, 1);
    a.at("t1");
    a.w.push(vop2(V_ADD_NC_U32, 12, 40, 12));
    let w = a.finish();
    let (rounds, insts) = rewritten(&w, 64);
    assert_eq!(setpcs(&insts), 3, "far jumps in the rewritten program");
    assert_eq!(rounds, 4, "rewrites until every branch reaches");
    differential("far jumps in rounds", &w, 64, &[[0, 0, 0, 0], [0, 1, 0, 0], [1, 0, 0, 0], [1, 1, 0, 0]]);
}

/// Several console waves in one group (96, 128 and 192 threads): EXEC by thread id across
/// the group (a far `s_cbranch_execz` taken by some waves only), a far `s_cbranch_scc1` on
/// each wave's first thread id (scalar control flow that differs between waves), the
/// compiler's far jump, then lane g takes lane g ^ 32's value inside its own console wave.
#[test]
fn far_jumps_in_groups_of_several_waves() {
    let mut a = Asm::new();
    a.w.push(vopcn("v_cmpx_gt_u32", 1, 0)); // exec: threads under s1
    a.br("s_cbranch_execz", "a");
    a.br("s_branch", "a0");
    a.at("a0");
    a.adds(N, 1);
    a.at("a");
    a.w.push(sop1n("s_mov_b64", EXEC, 22));
    a.w.extend(vop3(V_READLANE_B32, 7, v(0), int(0))); // s7 = the wave's first thread
    a.w.push(sopc("s_cmp_lt_u32", 7, 0));
    a.br("s_cbranch_scc1", "b");
    a.adds(N, 2);
    a.at("b");
    a.w.push(vop2(V_ADD_NC_U32, 12, 7, 12));
    a.far_jump("c", false);
    a.w.push(vop1(V_MOV_B32, 12, LITERAL)); // must not run
    a.w.push(0xBAD0_0000);
    a.at("c");
    a.w.push(vop2(V_XOR_B32, 2, int(32), 0));
    a.w.push(vop2(V_LSHLREV_B32, 2, int(2), 2));
    a.w.extend(ds(DS_BPERMUTE_B32, 12, 2, 12));
    a.w.push(S_WAITCNT_LGKM0);
    let w = a.finish();
    for threads in [96u32, 128, 192] {
        let (_, insts) = rewritten(&w, threads);
        assert_eq!(setpcs(&insts), 3, "far jumps in the rewritten program");
        differential(
            &format!("far jumps, {threads} threads"),
            &w,
            threads,
            &[[0, 0, 0, 0], [64, 50, 0, 0], [128, 100, 0, 0], [200, 192, 0, 0], [65, 64, 0, 0]],
        );
    }
}

/// The CPU model against the lane model's expectations for the handover's shuffles under a
/// mask (`model::bpermute`): the interpreter is checked against something it did not
/// compute.
#[test]
fn the_cpu_model_runs_the_handovers_shuffle() {
    let w = shuffle_program(false);
    let code: Vec<u32> = native_of(&w, true, 64)
        .code
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    for (x, y, lo, hi) in [(0, 1, u32::MAX, u32::MAX), (0, 63, u32::MAX, u32::MAX), (32, 0, 0xFFFF_0F0F, 0x1234_5678)] {
        let got = cpu::run(&code, 64, [x, y, lo, hi]);
        let want = shuffle_want(x, y, u64::from(lo) | u64::from(hi) << 32);
        for l in 0..64 {
            if let Some(wv) = want[l] {
                assert_eq!(got[l], Some(wv), "lane {l}, x {x}, y {y}");
            }
        }
    }
}

/// A scalar parked in another SGPR and moved back inside a region only some lanes run (what
/// the compiler does to reuse a register): the same value on every path, so the per-lane form
/// is the console's and the paired form is run and compared.
#[test]
fn per_lane_branches_with_a_scalar_parked_and_restored_are_exact() {
    let mut a = Asm::new();
    a.w.push(sop1n("s_mov_b32", 40, 0));
    a.w.push(vopcn("v_cmpx_gt_u32", 1, 0)); // exec: threads under s1
    a.br("s_cbranch_execz", "l");
    a.w.push(sop1n("s_mov_b32", 41, 40)); // park
    a.w.push(sopk("s_movk_i32", 40, 7));
    a.w.push(vop2(V_ADD_NC_U32, 12, 40, 12));
    a.w.push(sop1n("s_mov_b32", 40, 41)); // back
    a.at("l");
    a.w.push(sop1n("s_mov_b64", EXEC, 22));
    a.w.push(vop2(V_ADD_NC_U32, 12, 40, 12));
    a.w.push(vop2(V_XOR_B32, 2, int(32), 0));
    a.w.push(vop2(V_LSHLREV_B32, 2, int(2), 2));
    a.w.extend(ds(DS_BPERMUTE_B32, 12, 2, 12));
    a.w.push(S_WAITCNT_LGKM0);
    let w = a.finish();
    let flagged = differential("a scalar parked and restored", &w, 64, &[[5, 0, 0, 0], [9, 1, 0, 0], [3, 31, 0, 0], [0x100, 33, 0, 0], [1, 64, 0, 0]]);
    assert!(!flagged, "the per-lane form is the console's here");
}

/// A loop closed by `s_cbranch_execnz` with a scalar counter read after it: on the console
/// the counter is the wave's trip count (its longest lane's), per lane each lane's own. The
/// analysis finds it (the translator's per-lane form differs from the console here).
#[test]
fn a_scalar_counter_read_after_a_lane_mask_loop() {
    let mut a = Asm::new();
    a.w.push(sopk("s_movk_i32", 40, 0));
    a.w.push(vop2(V_AND_B32, 5, 0, 0)); // trip count: thread & s0
    a.at("top");
    a.w.push(sopk("s_addk_i32", 40, 1));
    a.w.push(vop2(V_ADD_NC_U32, 12, int(1), 12));
    a.w.push(vopcn("v_cmp_lt_u32", 40, 5)); // vcc: counter < trip count
    a.w.push(sop2n("s_and_b64", EXEC, EXEC, VCC_LO));
    a.br("s_cbranch_execnz", "top");
    a.w.push(sop1n("s_mov_b64", EXEC, 22));
    a.w.push(vop2(V_LSHLREV_B32, 12, int(8), 12));
    a.w.push(vop2(V_ADD_NC_U32, 12, 40, 12));
    let w = a.finish();
    let flagged = differential("a scalar counter after a lane-mask loop", &w, 64, &[[0, 0, 0, 0], [3, 0, 0, 0], [7, 0, 0, 0], [63, 0, 0, 0], [32, 0, 0, 0]]);
    assert!(flagged, "the per-lane form is not the console's here");
}

/// The program's own far jump is read only when its carry adds the literal's sign (0 for a
/// forward offset): with -1 there instead, the 64-bit offset is not the literal
/// sign-extended, and the program is refused rather than rewritten to a wrong target. CPU
/// only (the rewrite, no device).
#[test]
fn a_far_jump_whose_carry_is_not_the_literals_sign_is_refused() {
    let mut a = Asm::new();
    a.far_jump("past", false);
    a.w.push(vop1(V_MOV_B32, 12, LITERAL)); // must not run
    a.w.push(0xBAD0_0000);
    a.at("past");
    let w = a.finish();
    let getpc = w.iter().position(|&x| x == sop1n("s_getpc_b64", 30, 0)).expect("the far jump");
    assert_eq!(w[getpc + 3], sop2n("s_addc_u32", 31, 31, int(0)), "a forward far jump");
    let (rounds, insts) = rewritten(&w, 64);
    assert_eq!((rounds, setpcs(&insts)), (1, 1), "the far jump as the compiler writes it is rewritten");
    let mut bad = w.clone();
    bad[getpc + 3] = sop2n("s_addc_u32", 31, 31, MINUS_ONE);
    let native = native_of(&bad, true, 64);
    assert!(crate::dualbank::transform(&native).is_err(), "a carry of -1 after a forward offset");
    // the carry of the other kind (`s_subb_u32` after `s_add_u32`)
    let mut bad = w.clone();
    bad[getpc + 3] = sop2n("s_subb_u32", 31, 31, int(0));
    assert!(crate::dualbank::transform(&native_of(&bad, true, 64)).is_err(), "s_subb_u32 after s_add_u32");
}
