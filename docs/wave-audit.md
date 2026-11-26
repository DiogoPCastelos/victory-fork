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

## Dual-bank Wave64 (CLAUDE.md step 4, handover §6)

`crates/victory-shader/src/dualbank.rs` rewrites a console wave64 compute program into a
console wave32 program in which each lane carries two console lanes: p (bank 0) and p + 32
(bank 1). One host wave of 32 then runs a whole console wave, so nothing has to meet across
host waves: no barrier, no exchange through LDS. The result goes through the translator like
any wave32 program, with `WaveSize(32)` pinned.

- **Registers.** Bank 1's VGPRs are bank 0's plus `K` (the program's VGPR count); seven
  scratch VGPRs follow at `2K`. The console's EXEC lives in a pair of spare SGPRs (the trap
  temporaries, which compute code never names), so control flow stays scalar, as on the
  console.
- **Vector instructions** are written twice: bank 0, then bank 1, each after
  `s_mov_b32 exec_lo` of that bank's 32 bits of the console EXEC. Inactive lanes keep their
  values and side effects happen only in active lanes, as on the console.
- **Lane masks.** A 64-bit lane mask (VCC, compare results, carries, the EXEC pair) splits by
  dword: bank b's bits are `s[x + b]`. Bank 1's implicit-VCC compares and carries are
  rewritten into VOP3 (or SDWA `sd`) form, writing `vcc_hi`.
- **Lane-identity instructions.**
  - `v_readlane`, `v_writelane` and `v_readfirstlane` pick the bank by bit 5 of the lane.
  - `v_mbcnt_lo` and `_hi` count the other bank's bits.
  - `ds_bpermute` reads both banks and picks by bit 5 of the lane it names.
  - EXEC- and VCC-zero branches test both halves of the pair.

  None of the added code writes SCC or VCC, except the far jumps below, which save SCC and set
  it again before they jump.
- **Far jumps.** The program's own far jumps (`s_getpc_b64`, `s_add_u32` of a literal,
  `s_addc_u32`, `s_setpc_b64`) get their offset again after the rewrite. A branch that the
  growth puts out of reach of its 16-bit offset becomes a far jump of the rewrite's own, in
  the scratch SGPR pair. Conditional branches become the opposite branch over the far jump.
  The rewrite is done again until every branch reaches.
- **Refused, with a reason:**
  - more than 124 VGPRs;
  - program-counter instructions other than far jumps;
  - scratch memory and buffer loads into LDS (per-lane addresses the two banks would share);
  - `ds_permute`, `ds_append`/`ds_consume`, ordered counts, GWS, `*_addtid`;
  - DPP compares and SDWA `v_cmpx`, and DPP/SDWA forms of VOP2 with an implicit VCC in
    bank 1;
  - multi-dimensional groups whose dimensions are not powers of two.
- **Selection.** `emit` tries dual-bank after the exact paired forms and before any
  approximate form (`VICTORY_DUAL_BANK=0`: not tried).

Tests, all run on the RTX 3060 Laptop (2026-11-25): every lane test runs through both paths,
paired and dual-bank, against the same CPU model, and all of them pass. Two more were added
for dual-bank: an if/else on lane masks (`s_and_saveexec_b64`, `s_cbranch_execz` both ways,
`s_andn2_b64` to the else lanes), and the groups of 48 and 96. Making them pass found a
fourth translator defect: `v_readlane`/`v_writelane` took a computed lane number as it is,
where the ISA takes it modulo the wave size. Also added: `s_wqm_b64` on scalar data.

Census on the title's pack, 32-lane host, after dual-bank:

| | Before | After |
|---|---:|---:|
| compute programs kept at 64 lanes (the RTX 3060 refuses them) | 27 | 5 |
| compute programs on an APPROXIMATE half-wave form | 38 | 15 |
| compute programs taken by dual-bank (exact) | – | 45 |

With `--validate --pipelines` on the 65 programs that had no exact path: all 65 pass
`dxil.dll`; 60 build a pipeline on the RTX 3060, and the 5 that do not are the ones still kept
at 64 lanes. Every dual-bank program builds. Of the 7 compute pipelines the host refused in
today's game runs, 4 now build through dual-bank; the other 3 are among those 5.

Still refused by dual-bank: 8 with more than 124 VGPRs, 5 with DPP compares, 4 with
program-counter instructions, 2 whose branches no longer reach once the code grows, and 3
translator limits. Not measured yet: whether dual-bank or the paired path is faster
(CLAUDE.md asks for both to be measured before choosing defaults). Dual-bank is used only
where the paired path has no exact form, so the defaults for programs that had one are
unchanged.

Far jumps (2026-11-26, census of the title's pack, 32-lane host): dual-bank takes 51 compute
programs (47 before). These are the compute program the opening dropped in every world frame
(no exact form before: a lane mask exchanged where not every lane runs, and dual-bank refused
its far jump), and 3 that were on an APPROXIMATE half-wave form (15 → 12). All four pass
`dxil.dll` and build a pipeline on the RTX 3060. Compute programs that do not translate: 6 → 5.
In the game (RTX 3060, 2026-11-26, four runs to the world): the program translates at start
("shader translated"), runs in the world's captured frames (1-3 dispatches a frame) and is
dropped nowhere; no run logged a shader that does not translate. That it computes the right
picture is not shown: two other programs write the same two targets, so its own output cannot
be told apart in a capture.

Pack census after the far-jump commits (`945fe1f`, `47eaf6e`; `translate --list --validate
--pipelines`, 32-lane host, view limit 999,999): 26,151 programs lowered; DXIL for 14,224 of
14,262 vertex and pixel programs, 2,729 of 2,734 compute and 8,762 of 8,765 mesh programs;
**every emitted program makes a pipeline** on the RTX 3060; 216 fail `dxil.dll`'s validator
(111 vertex/pixel, 105 mesh, 0 compute: mesh output over 32 KB, signatures not as the module
expects, rows packing elements of different interpolation modes; the driver takes them).
Compute paths: 51 dual-bank (exact), 27 of the allocation idiom (exact), 12 half-wave forms
marked APPROXIMATE and 36 wave32 programs whose lane write is APPROXIMATE.

### More lane tests (both paths)

- **3D groups** (4×4×4, one console wave; 8×4×4, two): lanes follow the linear thread id,
  x fastest. Each thread computes its id from v0, v1, v2 and shuffles by it. 24 cases per
  shape.
- **A loop with a per-lane trip count** that differs between the halves, left lane by lane
  (`s_and_b64 exec, exec, vcc`, `s_cbranch_execz`), with an if inside on odd iterations
  (saveexec, execz, `s_or_b64` back). This is nested EXEC inside a loop.

- **Far jumps** (2026-11-26): `s_cbranch_scc1` and `s_cbranch_execz`, each over 10,000
  instructions, which the rewrite puts out of reach. SCC is read after the first (a far jump
  must keep it). Then a far jump of the program's own over a block that must not run, then a
  `ds_bpermute` across the halves. 12 cases (s0 picks the first branch, s1 the lanes of the
  second). The dual-bank case also checks that the rewrite made the far jumps. A far jump made
  to forget SCC fails it.

All of them pass on the RTX 3060 through both the paired and the dual-bank path.

### Far jumps against a CPU model of the program (2026-11-26)

`emit/far_jump_tests.rs` runs each program's own words on a small CPU interpreter of the
GFX10 subset the tests use (written from the public ISA; it shares only the opcode tables
with the translator). It then runs them on the RTX 3060 in both 32-lane forms and compares
every thread. There is one test per property:

- forward jumps (`scc1`, `scc0`, an if/else closed by `s_branch`, and the program's own far
  jump);
- backward jumps (loops closed by `scc1`, by `s_branch`, by the program's far jump
  subtracting, and by one adding a negative offset with a carry of -1);
- SCC kept across far `scc1`, `scc0`, `s_branch` and `execz`;
- the rewrite's scratch SGPRs, which never overlap SGPRs the program names (s24-s105, VCC
  and M0 live across; or ttmp0-1 named), plus a refusal when none is free;
- divergent EXEC: `execz`, `vccz`, `execnz` and `vccnz` under masks in one half only, lane
  31 or 32 only, none, all, and random ones;
- branch relaxation in rounds: four rewrites and three far jumps, conditional and
  unconditional;
- groups of 96, 128 and 192 threads.

The program's own far jump is now read only when its carry has the literal's sign: the
64-bit offset is then the literal sign-extended. A far jump whose direction or reach changes
in the rewrite is refused.

**Found by the divergent-EXEC test (both 32-lane forms): per-lane branches that are not the
console's** (`lane_exact.rs`). The structurer makes `s_cbranch_exec*`/`vcc*` per-lane, and
that differs from the console in two cases:

- **A scalar written in a region only some lanes run, then read after it.** Each lane then
  keeps its own copy. A loop counter read after a lane-mask loop is one example.
- **A `vccz`/`vccnz` over unmasked vector code when VCC differs between active lanes.**
  Compilers emit that branch only for conditions they know to be uniform
  (`s_and_b64 vcc, exec, m`), so the analysis does not count it.

`lane_exact::inexact` finds the first case. It follows plain copies, since the compiler parks
a scalar and moves it back, and it leaves out lane masks.

The translator does not act on it yet: the census lists these programs per program
("per-lane branches not exact"; 1,373 programs), and their translation is unchanged. Where
the far-jump tests' programs have such branches, the paired form is not run and the
dual-bank form's differences are reported, not asserted.

Three steps were tried and taken out again (2026-11-26). Game runs with all three showed a
black world after the menus. Each changed programs the opening binds (census against the
far-jump fix alone; the bound programs are the union of four runs' first binds):

- Sending a compute program with such a branch to an exact form first. Wave32 programs then
  took those branches wave-wide (1,024 programs, 337 bound), and one console wave64 program
  went dual-bank.
- Dual-bank taking every lane-mask branch wave-wide (all 51 dual-bank programs, 19 bound).
- `v_readlane_b32` with a constant lane taken `& (wave - 1)` of the console wave: 430 pixel
  and 49 wave32 compute programs changed, 36 pixel programs bound. That is a no-op for the
  lane read, so it did not do what its comment said.

Which of them, if any, caused the black world is not known. Each needs its own game check.

### Paired against dual-bank (handover milestone 5)

`bench_paired_against_dual_bank` (an ignored test: `cargo test -p victory-shader --lib
bench_paired_against_dual_bank -- --ignored --nocapture --test-threads=1`). Each workload is
a synthetic one-wave program dispatched as 65,535 groups (4 million console threads), timed
with GPU timestamps around the dispatch, median of 5 after a warm-up. Totals in ms on the
RTX 3060 Laptop, four runs, 2026-11-25:

| Workload | Paired | Dual-bank |
|---|---:|---:|
| empty program (the store alone) | 0.02–0.13 | 0.42 |
| 128 dependent `ds_bpermute` across the halves | 2.4–2.5 | 1.58 |
| 128 `v_readlane` by a scalar lane, and adds | 2.5–2.6 | 0.80 |
| 128 compares read back as data | 2.4–2.5 | 0.42 |
| 1024 adds and xors, no cross-lane code | 0.1–1.6 | 1.59 |
| 8 whole-wave sums (the compiler's DPP/permlane form) | 0.3–0.4 | 0.42 |

How to read it:
- **Cross-lane code.** Where the paired path exchanges through LDS with group barriers,
  dual-bank is 1.6–6× faster.
- **No exchange.** With no cross-lane code, or the compiler's own sums (which pair within
  32-lane rows), the two are even or the paired path is ahead.
- **Fixed cost.** Dual-bank's program writes every vector instruction twice and uses twice
  the VGPRs, which costs it a fixed floor of about 0.4 ms per 4 million threads here.
- **Conditions.** A game instance from another session was using the GPU during these runs,
  and the laptop's power state was not controlled. The paired path's plain-arithmetic row
  is bimodal (0.1 or 1.6 ms) across runs. The cross-lane rows repeat to within 0.15 ms.

What this settles: dual-bank stays where the paired path has no exact form. Programs whose
paired form exchanges heavily would likely run faster as dual-bank too. Choosing per program
needs a cost estimate (exchanges and barriers against instruction count), which is not built.

### 64-lane graphics programs and the console's grouping (handover §9)

The census of `victory translate` now also lists, for console wave64 vertex, pixel and mesh
programs, the cross-lane code whose results may differ when the program runs in host waves
of 32 rather than the console's 64:
- whole-wave sections (lowered to DXIL wave operations over the host's wave);
- first-lane reads of values that may differ by lane;
- lane reads or writes by number;
- `v_mbcnt` lane counts;
- arbitrary lane shuffles.

Code confined to quads or rows of 16 runs the same in a 32-lane wave and is not listed. Two
kinds of first-lane read are not listed either: those that are waterfall loops' descriptor
reads (the translator gives each lane its own), and those of values a conservative walk
proves uniform. `--list` prints each listed program on a line of its own, with hash, stage
and reasons: the per-title report the handover asks for.

The title's pack (2026-11-25):

| Programs | Stage | Code that may depend on the grouping |
|---:|---|---|
| 8,265 | pixel | none |
| 2,041 | vertex | none |
| 1,848 | pixel | first-lane reads of possibly varying values |
| 433 | pixel | whole-wave sections and lane reads by number (149 with `v_mbcnt` too) |
| 302 | pixel | lane reads or writes by number (81 with first-lane reads) |
| 37 | pixel | `v_mbcnt` (2 with first-lane reads) |
| 150 | vertex | first-lane reads (1 with whole-wave sections) |
| 8,762 | mesh | first-lane reads and `v_mbcnt` (17 with whole-wave sections) |

This is a list of what *may* depend on the grouping, by instruction. The listing does not
follow how a result is used: a vote that only skips work gives the same image. Lane reads
by number in pixel programs are often SGPR spills parked in VGPR lanes. No program is shown
wrong by it yet. The next step for graphics is one differential test per listed kind, the
console's HLSL compiled both ways (`emit/diff_tests.rs` has the harness).

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
