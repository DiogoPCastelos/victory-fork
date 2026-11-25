# Wave translation: what this repository has, against the handover

An audit of the repository against [XBOX_SERIES_GPU_TRANSLATION.md](XBOX_SERIES_GPU_TRANSLATION.md)
(phase 0 of its milestones), written 2026-11-25 from the source as it stands. It
says what exists and where; it measures nothing new. Paths are relative to
`crates/victory-shader/src` unless they start with `crates/`.

## Summary

The handover is written for a project with no wave subsystem yet. This one has
most of it, in a different shape. It does not have an IR layer with logical
lane identities. It translates GFX10 machine code one instruction at a time to
DXIL, and decides per program how a console wave runs on the host. What is
missing against the handover:

1. A **CPU reference model** of 64-lane execution (the handover's test oracle).
2. The handover's **deterministic boundary tests** (31↔32, XOR-32 in place,
   ballot {0,31,32,63}, first-active in the high half, and so on). The GPU tests
   that exist cover reductions, scans and masks, not this list.
3. **Dual-bank virtual Wave64** (handover §6). The paired shared-memory path
   (§7) exists; dual-bank does not, so nothing has been measured between them.
4. One **approximate fallback is on by default**, against CLAUDE.md's rule that
   approximations be opt-in (below, "Departures").

## Inventory

| Handover item | Here | Where |
|---|---|---|
| Guest shader format | DXBC container, `DXIL` part with the console's `XDAT`/`XBCH`/`FSSM` blobs holding native GFX10 programs | `dxbc.rs`, `dxil.rs`, `native.rs` (`native_programs`) |
| Guest wave mode from evidence | The program record's flags, bit 5 (wave32, else wave64) | `native.rs` module doc, `NativeProgram::wave64` |
| Decoder | GFX10 decoder and opcode tables | `gfx10/mod.rs` (`decode`), `gfx10/opcodes.rs`, `gfx10/fields.rs` |
| Semantic IR | SSA, one `Inst` per machine instruction, implicit operands explicit (EXEC, SCC, M0; a VGPR write also reads its old value for disabled lanes) | `ir.rs` |
| Control flow / divergence | Structurisation into lane masks; branches taken wave-wide on request | `structure.rs` (`structure`, `structure_wide`) |
| Uniformity analysis | `lane_uniform`, `lds_settled` (shared-memory reads proven uniform), `all_lane_blocks` (blocks every lane reaches) | `emit/lower.rs` |
| Width analysis | Per program: cross-lane opcodes (DPP, `ds_swizzle`, `v_permlane*`, `v_mbcnt`, `v_readlane`/`v_writelane`, `ds_(b)permute`), masks read as data, lane-constant masks, wave-wide branches | `emit/lower.rs`, the `console_lanes` / `crosses` tests near the `WaveSize` choice |
| Host capability probe | D3D12 `OPTIONS1` `WaveLaneCountMin/Max`, logged ("host wave lanes"), handed to the translator; part of the shader cache key | `crates/victory-gpu/src/host.rs` (`CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS1)`), `emit/lower.rs` (`set_host_max_wave`), `emit/mod.rs` (`translation_fingerprint`) |
| Native Wave32 (§5) | Guest wave32 programs as they are. Guest wave64 compute whose cross-lane code stays inside 32-lane halves runs in waves of 32, each host wave one half (lanes 32k..32k+31) | `emit/lower.rs`, the `wave = 32` paths |
| `WaveSize(64)` never on a 32-lane host | `WaveSize` is chosen from `host_max_wave()`; 64 is kept only where the host allows it | `emit/lower.rs`, `emit/shader.rs` (`wave_size`) |
| Paired Wave32 + shared memory (§7) | Two host waves per console wave exchanging through LDS dwords past the program's own (`exchange_halves`, `exchange_pair`, `wave_base`). Group barriers only in blocks every lane reaches (`all_lane_blocks`), or where the halves provably meet (`lds_meetings`). Branches around cross-lane code taken wave-wide so the halves meet | `emit/lower.rs`, `structure.rs` (`structure_wide`), `emit/mod.rs` (the WAVE64-RETRY forms) |
| Dual-bank virtual Wave64 (§6) | **Missing** | — |
| Interleaved mapping (§8) | Missing (optional in the handover) | — |
| Whole-wave reductions and prefixes | Whole-wave sections simulated symbolically over the console's lanes and lowered to DXIL wave operations; the console's float summation tree reproduced bit for bit | `emit/wave.rs`, `emit/wave64_tests.rs` |
| Scalarised loops over wave ORs | Proven exact per thread, no barrier needed | `emit/own_bits.rs` |
| Graphics stages | Pixel, vertex and mesh programs lowered per lane; whole-wave sections become DXIL wave operations over the host's wave (what HLSL promises). Interpolation pairs, helper and derivative handling in `emit/lower.rs` | `emit/lower.rs`, `emit/wave.rs` |
| Unsupported diagnostics | `ShaderError::Unsupported` with offset and reason. Logged per shader at runtime ("shader does not translate"); pack census with `victory translate --list`; `VICTORY_WAVE_WHY=1` says why a program keeps 64-lane waves | `lib.rs`, `emit/mod.rs`, `apps/victory` |
| Translation and pipeline cache keyed on host facts | Translator fingerprint (source, switches, host wave width, view-heap limit) and the D3D12 pipeline library (adapter and driver checked) | `crates/victory-gpu/src/shader_cache.rs`, `crates/victory-gpu/src/pipeline/library.rs` |
| CPU reference model (§12) | **Missing**. `emit/wave.rs` simulates whole-wave sections symbolically for lowering, but it is part of the translator, not an independent oracle | — |
| Tests on the GPU | Synthetic GFX10 programs lowered, validated by `dxil.dll` when present, run on the host GPU and WARP: wave64 on 32-lane hosts, waterfall loops, VALU semantics, differential HLSL-vs-console tests | `emit/wave64_tests.rs`, `emit/waterfall_tests.rs`, `emit/valu_tests.rs`, `emit/diff_tests.rs` |

## Departures from CLAUDE.md's rules

- **Approximate half-wave fallback, on by default.** For a wave64 compute program
  that does not translate any exact way, `emit/mod.rs` finally lowers it with each
  host wave as a wave of its own (`lower::with_local_waves_any`). The source says
  this is approximate: "a tile-wide result becomes two half-tile ones".
  `VICTORY_WAVE_LOCAL=idiom` refuses those programs instead, and
  `VICTORY_WAVE_LOCAL=0` turns all local forms off. CLAUDE.md asks for
  approximations to be opt-in and flagged. Making `idiom` the default follows the
  rule, but programs that render approximately now would then not run at all. How
  many of the title's programs take this path has not been counted. That count is
  the first thing to measure before changing the default.
- **The exact local form for the allocation idiom** (`lower::wave_local`) also
  splits a console wave into two independent host waves. It is exact for what it
  proves (each lane still gets its own slot; only the order differs), with one
  shortcut written in the source: where a count or sum goes is not checked. The
  handover forbids splitting dependent waves; this case is argued independent, but
  the argument has that one unchecked step.
- **Graphics wave operations** lower to the host wave's own operations. That
  matches HLSL's contract (wave membership is the implementation's), not
  necessarily the console's 64-lane grouping. This is correct for programs that
  rely only on HLSL's guarantees, which compiler-generated reductions do. It is
  not proven for hand-written lane code in pixel programs.

## Census: the paths the title's programs take (32-lane host)

`VICTORY_HOST_WAVE=32 VICTORY_VIEW_LIMIT=999999 victory translate <pack>` prints the census
of lowering paths (`--list` gives one line per program). Measured 2026-11-25 on the title's
pack: 26,555 containers (614 duplicates), 25,687 programs emitted.

| Programs | Stage, console wave | Path |
|---:|---|---|
| 1,569 | compute, wave32 | as it is |
| 1,123 | pixel, wave32 | as it is |
| 527 | compute, wave64 | 32-lane halves, no cross-lane code |
| 263 | compute, wave64 | 32-lane halves, cross-lane code confined to halves |
| 116 | compute, wave64 | two host waves exchanging lanes through LDS (59 first try, 57 in a retried form) |
| 48 | compute, wave64 | the same with the console's shared LDS split (lane exchange too) |
| 111 | compute, wave64 | shared LDS split only |
| 27 | compute, wave64 | each half its own wave, the allocation idiom (exact, one unchecked step) |
| **38** | compute, wave64 | **each half its own wave: APPROXIMATE** (27, plus 11 with every mask and lane read per half) |
| **27** | compute, wave64 | **kept 64-lane waves: a 32-lane host refuses the pipeline** |
| 6,585 / 532 / 8,762 | pixel / vertex / mesh, wave64 | per lane, wave operations over the host's wave (HLSL's contract) |
| 4,300 / 1,659 | pixel / vertex, wave64 | per lane, no cross-lane code |

Not emitted: 195 (an address in an SGPR that is no root parameter), 40 (a descriptor chosen
by control flow), 10 (a hardware SGPR input), 5 (a descriptor read only as data), 2 (a lane
mask holding lanes elsewhere read as data), 1 (a lane mask exchanged where not every lane
runs), 1 (an opaque lane mask). This census does not validate or build pipelines: add
`--validate --pipelines` for those numbers.

What this settles for the plan below:
- **Step 4.** Making the approximate fallback opt-in would stop 38 compute programs
  from running. They are the handover's dual-bank or paired targets.
- **Dual-bank target set.** Together with the 27 that keep 64-lane waves, that's 65 compute
  programs (about 2.4% of the 2,734) without an exact path on this GPU.
- **Graphics.** 15,879 wave64 graphics programs use wave operations under HLSL's contract.
  Whether any of them depend on the console's 64-lane grouping is unproven. CLAUDE.md asks
  for an explicit report on such programs. Building that report needs a check for hand-written
  lane code in graphics programs, which the census does not do yet.

## Reference model and lane tests (CLAUDE.md step 3)

`crates/victory-shader/src/emit/lane_tests.rs`:
- **`model`.** A CPU model of one 64-lane console wave, written from the public RDNA 2 ISA and
  sharing no code with the translator. Lanes it does not claim to know are `None` and are
  not compared: a `ds_bpermute` source lane outside EXEC, and `v_readfirstlane` with EXEC
  empty.
- **CPU test.** The model gives the handover's literal expectations: 31→32 gives 0x1020,
  32→31 gives 0x101F, XOR-1 and XOR-32, the ballot of {0,31,32,63} gives 0x80000001 twice,
  the first active lane of a high-half-only mask is 37, the sum of lane ids is 2016, the
  exclusive prefix of ones gives each lane its own id.
- **GPU tests.** Each is a synthetic one-wave program lowered for 32-lane waves (the
  paired-wave exchange), validated by `dxil.dll` when present, and run on the hardware
  device. Every lane is compared with the model:
  - `ds_bpermute`: identity, both boundary reads, XOR-1/32/63, all 4,096 (source,
    destination) pairs, in place and not, and 64 random EXEC/XOR/rotation cases each;
  - `v_readlane`, `v_writelane` and `v_readfirstlane` under random masks;
  - a ballot read as data, with `s_ff1_i32_b64` and `s_bcnt1_i32_b64` (48 cases);
  - `v_mbcnt` prefixes (40 masks);
  - the compiler's whole-wave sum;
  - `v_cmpx` divergence across the boundary.

  They print "NOT RUN" without a hardware device of 32-lane waves. Report them as not run,
  never as passed.

Run on the RTX 3060 Laptop, 2026-11-25: all eleven pass, every case on the hardware device
(the eleventh: shuffles in groups of 48 and 96 threads, partial console waves and two console
waves per group). Three translator defects were found and fixed on the way:
1. **`v_writelane_b32` into a lane outside EXEC kept the old value.** The ISA's instruction
   ignores EXEC. The write now skips the EXEC guard. Where not every lane reaches the
   instruction, the target lane's thread may not be there. Refusing that case stopped 36 of
   the title's programs, ray-traced shadows among them, so they translate as before and the
   census marks them APPROXIMATE (36 wave32 compute programs; the census after these fixes
   is otherwise the same as before them).
2. **`ds_bpermute` across the two host waves of a console wave was refused.** It now goes
   through 64 LDS dwords per console wave between two group barriers: the handover's
   paired-wave shuffle (§7). This is only done where every thread of the group reaches it;
   elsewhere it is still refused, with that reason.
3. **Host waves were mapped to console lanes in arrival order (`wave_base`).** In a group
   that is not a multiple of 32 threads, the short host wave could take console lanes 0..31,
   leaving lanes with no thread: in a group of 48, lanes 16..31 read 0. This affects every
   wave64 compute program of such a group size that runs as two host waves, not only
   shuffles. Only full host waves now count off the counter; the short one takes the last run
   (`group & !31`).

Not covered yet: 3D groups, nested EXEC in loops, the ISA's rule that a `ds_bpermute` from a
source lane outside EXEC reads 0 (the translator implements it; the model does not claim it),
and graphics stages.

## Plan: the smallest steps, in order

1. **Count the paths** (done: the census above). Add a census to `victory translate` of which lowering each
   program took: native 32, native 64, halves exchanging through LDS, local
   (idiom), local (approximate), refused. Run it on the pack with
   `VICTORY_HOST_WAVE=32`. This decides step 4 with numbers rather than guesses.
2. **CPU reference model.** A small, independent 64-lane model for tests only:
   vector registers with 64 slots, EXEC as a 64-bit mask, and source snapshots,
   for the opcodes the boundary tests need (`v_mov`, `v_readlane`, `v_writelane`,
   `v_readfirstlane`, `ds_bpermute`, `ds_swizzle`, `v_permlanex16`, `v_mbcnt`,
   `s_ff1`, `s_bcnt1`, ballot through `v_cmp`).
3. **The handover's deterministic tests** on the GPU through the existing
   exchange path, each checked against the model: 31→32, 32→31, XOR-32 in place,
   XOR-1, all 4,096 (source, destination) pairs, ballot {0,31,32,63},
   first-active in the high half, sum of lane ids 2016, exclusive prefix, partial
   groups, aliasing. Then the same with randomized masks.
4. **Make the approximate fallback opt-in** if step 1 shows the cost is
   acceptable, with each such program named in the log. Otherwise keep it, flagged
   in the log and the census, and say so in the README.
5. **Dual-bank Wave64** only after steps 1–3, for the programs that still keep
   64-lane waves or stay refused on a 32-lane host. Measure it against the paired
   path on the RTX 3060 before choosing a default.

## Blockers and unknowns

- Five compute programs of the title still need 64-lane waves on a 32-lane GPU
  (shared memory read and written inside per-lane branches;
  [fork-findings-6gb-laptop.md](fork-findings-6gb-laptop.md)). Dual-bank is the
  handover's answer for them. It is not built.
- The console's exact behaviour for some lane instructions with inactive or
  out-of-range sources comes from the public RDNA 2 ISA. The custom console part
  is not documented separately.
- GPU tests need a GPU; on a machine without one they skip, and must be reported
  as not run.
