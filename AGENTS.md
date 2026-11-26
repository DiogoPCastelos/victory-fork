# Codex project instructions — Xbox Series GPU compatibility

## Project objective
Develop a shader compatibility layer for Xbox Series X/S (custom AMD RDNA 2-class GPU) targeting a PC with an NVIDIA GeForce RTX 3060 Laptop GPU (Ampere, 32-lane warps, normally 6 GB GDDR6). Preserve guest Wave32/Wave64 semantics, including communication between guest lanes 31 and 32, while maintaining real-time game-rendering performance where feasible.

Read `docs/XBOX_SERIES_GPU_TRANSLATION.md` in full, then `docs/wave-audit.md` (what this repository already has against it), before proposing implementation changes. It contains the ranking, semantic IR, lowering pseudocode, caveats, test cases, milestones and public references. It is a proposal, not evidence of an existing implementation.

## Mandatory design rules
- Inspect repository and existing GPU/frontend/backend architecture before adding modules. Reuse the current API and build/test conventions; Vulkan is a reasonable default only for a greenfield compute prototype. Do not use CUDA as the primary graphics renderer.
- Determine guest shader stage and wave size from evidence. Do not assume all Xbox Series shaders are Wave64. Query actual PC device capabilities and ensure the discrete NVIDIA GPU is selected.
- Prefer native Wave32 or width-independent execution where proven equivalent; use dual-bank virtual Wave64 and paired Wave32 shared-memory lowering only where legal. For future hosts, support native Wave64 if reported and appropriate.
- Never split dependent logical waves into independent batches. Never issue unsupported Wave64 requests on the RTX 3060.
- Preserve old source values before committing destination banks. Distinguish logical guest EXEC/validity from physical subgroup participation. Handle guest scalar state, lane identities, divergence, side effects, memory scopes and inactive-source semantics explicitly.
- Never place a workgroup barrier inside control flow that is not uniform for all participating physical invocations. Do not use cross-warp spin waits. Initial paired-wave tests: one logical guest wave per workgroup; no unsupported group remapping.
- Do not apply compute virtualization blindly to fragment/vertex shaders. Preserve derivatives, quads, helper invocations, outputs and rasterizer coverage; report unsupported cross-half graphics operations explicitly.
- Favor SSA, bounded caches, modest VRAM footprint, register liveness analysis and static lane-pattern optimization. Measure both Wave64 compute backends before selecting performance defaults. Do not invent FPS, compatibility claims or completed tests.
- Use authorized shader inputs/test fixtures. Do not require proprietary SDK material or protected binaries to run synthetic validation tests.

## First task after reading
1. Inventory existing shader decoder, IR, shader stage/wave metadata, host graphics API, subgroup usage, and relevant test files. Identify what is present vs missing with exact repository paths and symbols. If no repository is available, state that and create only a proposed integration map.
2. Provide a short plan identifying the minimum implementable changes and blockers, including unknown guest shader formats or unsupported stages. Do not rewrite unrelated systems.
3. Add or verify a GPU capability probe and the CPU wave reference model. Add deterministic and randomized lane-operation tests from the handover, including cross-boundary shuffle, in-place XOR-32, ballot, prefix, masks and divergence.
4. Implement or verify native Wave32 lowering. Establish a constrained paired-wave shared-memory compute prototype, then contiguous dual-bank Wave64. Keep both behind a capability-checked selector and provide explicit diagnostics.
5. Run the available tests/build commands. Report actual results, files changed, tests not run, and remaining limitations. GPU tests must be reported as unrun if the RTX 3060 or another suitable device is unavailable. Do not claim a game works based only on synthetic tests.

## Definition of initial success
A minimal compute test running on a verified 32-lane host correctly transfers guest lane 31's value to lane 32 and vice versa, matches the CPU reference for the supported operation subset, and never issues a barrier with illegal participation. A Wave32 shader that does not need emulation remains on the native path. Unsupported graphics shaders receive clear diagnostics, not silent approximations. Game compatibility is a later milestone requiring actual authorized title data and broader subsystem integration.

## Running the game
Run it with ray tracing on: the static world's ray tracing runs by default, and `VICTORY_BVH_BUILDER=1` adds the builder of the dynamic structures (characters, moving objects). Warm the cache first with the same build (README, step 2).

```powershell
$env:VICTORY_BVH_BUILDER = 1
target\diag\victory.exe run "<game folder>\<the game's .exe>" --execute > "<a folder outside the repo>\run.log" 2>&1
```

If a run crashes or runs out of memory soon after the world appears, report it and run once more without `VICTORY_BVH_BUILDER`. Never hold the ray-tracing walkers (`VICTORY_BVH_HOLD`) in a normal run: they light the world.
