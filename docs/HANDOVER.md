# Handover: where this work stands (2026-11-26)

For whoever continues: a person, or an assistant reading this cold. It says what the project
is, what was done in the last session, what state it is in, how to run it, and what is open.
Read `CLAUDE.md` (project rules) and `docs/wave-audit.md` (wave translation: inventory,
census, tests, benchmark) next.

## The project in one paragraph

VIctory is a compatibility layer (Rust, Windows, D3D12) that runs an Xbox Series title on a
PC. It loads the title's executable, implements the console's OS and graphics APIs, and
translates the console GPU's shader machine code (GFX10 / RDNA 2) into DXIL for D3D12. This
checkout is a fork of `fxceless/victory-fork` (itself a fork of `Marvisak/VIctory`), kept
on the user's own fork `DiogoPCastelos/victory-fork`, branch `main`. It
is not meant as a pull request to the original.

The test machine: RTX 3060 Laptop (6 GB, 32-lane waves only), 16 GB RAM, i7-11800H.
The title's files live outside the repository and are **never written to**.

## Rules (from the project and the user)

- Never name the title in the tree, commits or PRs; no shader strings, archive names or
  screenshots in docs; game-derived output (caches, logs, captures) only outside the repo
  (`docs/asset-policy.md`, `.victory-root`).
- XCL (another such project) is GPL-2.0-only: ideas only, never code.
- Commit with the GitHub noreply identity, never to `main`; push only to the user's fork.
- `CLAUDE.md`: no silent approximations by default; report only what was actually run and
  measured; GPU tests count as "not run" without a suitable GPU.
- The user's game files are sacred: read them, never modify them.

## What the last session did

1. **Ported the user's older fork** (`fork/6gb-laptop-rt-builder`): ray-tracing builder
   scans on 32-lane GPUs, texture spill into shared memory, compute dispatches wait for
   their pipeline, pipeline workers pause at low free commit, Series S plane layout, and
   the findings doc `docs/fork-findings-6gb-laptop.md`.
2. **Shader cache keeps failed translations** (`VICTORY_SHADER_CACHE_FAILURES=0` turns it
   off). The texture spill got a commit floor (`VICTORY_TEXTURE_SPILL_COMMIT_MB`).
3. **Wave translation, per CLAUDE.md** (all in `docs/wave-audit.md`):
   - a census of how every shader program is lowered (`victory translate` prints it;
     `--list` per program);
   - a CPU model of a 64-lane wave and lane tests run on the GPU
     (`crates/victory-shader/src/emit/lane_tests.rs`): 31↔32 boundary, XOR-32 in place, all
     4,096 shuffle pairs, ballots, prefix counts, divergence, if/else, loops, 3D groups,
     partial groups;
   - four translator bugs those tests found, fixed: `v_writelane` into an inactive lane;
     `ds_bpermute` across the two 32-lane halves; host-wave-to-console-lane mapping in
     groups that are not a multiple of 32; lane numbers taken modulo the wave size;
   - **dual-bank Wave64** (`crates/victory-shader/src/dualbank.rs`): a 64-lane console
     program rewritten so one 32-lane host wave runs a whole console wave, two console
     lanes per host lane. It takes 45 of the 65 compute programs that had no exact path
     on a 32-lane GPU; the programs refused by the RTX 3060 went from 27 to 5, approximate
     ones from 38 to 15. Selected only where the older "paired waves" path has no exact
     form. Benchmarked: 1.6-6× faster than paired where paired needs LDS exchanges,
     slower on plain arithmetic;
   - a report of 64-lane graphics programs whose wave operations may depend on the
     console's 64-lane grouping (census section).
4. **Merged two parallel sessions' fixes:** pixel programs with rasterizer-ordered views
   (POPS) translate; a descriptor chosen by SCC branches translates; programs still
   needing 64-lane waves are refused up front instead of failing in the driver; the layer
   no longer ends the run when it runs out of memory bringing card copies up to date
   (records the sync list again).
5. A watchdog script (`scripts/upstream-watchdog.ps1`, Windows scheduled task) rebases the
   checked-out branch onto `origin/main` every 10 minutes (never pushes; skips when the
   tree is dirty or the game runs). Remove with `-Unregister`.

Tried and taken back out: GPU texture feedback (shaders marking sampled view slots so their
textures upload first). Reporting a 1080p display (`VICTORY_DISPLAY_MODE=1920x1080`)
changed nothing: the title still renders at 2560×1440.

## Where the game stands

- Reaches the world and runs for 10+ minutes at about 0.5-2 fps; ~5,300 draws a frame.
- **Main visible problem: skin, walls and furniture render black.** Diagnosed with captures
  of the G-buffer: the normals target is complete (geometry and pixel shaders run), the
  albedo target is black for those materials. They pick their textures from the view
  heap by a computed index; the layer uploads those through a queue
  (`crates/victory-gpu/src/draw.rs`, `heap_textures`) that stops when the card is full.
  On the 6 GB card it is past its budget from the world's first frame (6.6-6.9 GB used of
  5.2 GB) and the PC is down to ~0.5-2 GB of commit; ~18,000 slots wait. The older fork's
  best run had the same black albedo. **It is a memory problem, not a shader problem.**
- Next step being arranged: a test on a machine with 32 GB RAM and a 16 GB+ NVIDIA GPU, to
  confirm textures appear when memory allows (a test build was zipped for that).

## How to run (PowerShell, from the repo root)

```powershell
cargo build --release -p victory
# warm the shader cache with the settings of the run's GPU (NVIDIA: 32 lanes, 999999)
$env:VICTORY_HOST_WAVE=32; $env:VICTORY_VIEW_LIMIT=999999
target\release\victory.exe translate "<game folder>" --warm all
Remove-Item Env:VICTORY_HOST_WAVE, Env:VICTORY_VIEW_LIMIT
target\release\victory.exe run "<game folder>\<title exe>" --execute > <somewhere outside the repo>\run.log 2>&1
```

- The cache key includes the translator's source and build kind: after any translator
  change, warm again. The diagnostics build (`cargo build --profile diag -p victory
  --features diagnostics`) has its own cache: warm it with that binary.
- Frame and render-target captures (diagnostics build): `VICTORY_CAPTURE=<dir>`,
  `VICTORY_CAPTURE_WORLD=1`, `VICTORY_CAPTURE_WORLD_ALSO=60,150,300`,
  `VICTORY_DUMP_TARGETS=1`; `VICTORY_KEYS=45:Enter,60:Enter,...` presses keys.
- Tests: `cargo test -p victory-shader --lib` (GPU tests run on the local GPU),
  `cargo test -p victory-gpu --lib` (`warp_budget_saturates_*` is flaky on WARP).
- Benchmark: `cargo test -p victory-shader --lib bench_paired_against_dual_bank --
  --ignored --nocapture --test-threads=1`.

## Open problems, in order

1. **Textures held back for lack of GPU memory** (above). Ideas: GPU feedback of sampled
   slots (tried, reverted, see git history of this session); letting textures not sampled
   for a long time go; using less memory per frame (the "windows" copies, 3.2 GB).
2. **Frame rate**: copying memory back from the card and memory pressure dominate
   (`docs/fork-findings-6gb-laptop.md`, "Memory on a 16 GB machine").
3. **Approximations on by default**: 15 compute programs use the approximate half-wave
   form, 36 are marked approximate for a lane write. CLAUDE.md wants them opt-in; the user
   has not decided.
4. **Dual-bank refusals** (20 programs): >124 VGPRs, DPP compares, program-counter
   instructions, branches too far.
5. **Ray-tracing structure builder**: runs, but a stage stores out of bounds (off by
   default; `VICTORY_BVH_BUILDER=1`).
6. **Graphics**: no differential tests yet for the listed 64-lane graphics programs.
7. **Series S**: the UI plane is not composited.
