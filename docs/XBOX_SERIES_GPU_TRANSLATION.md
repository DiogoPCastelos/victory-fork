# Xbox Series X/S → PC GPU compatibility layer
## Technical handover for Claude — RTX 3060 Laptop GPU target

**Status:** Proposed architecture, not an implemented or benchmarked emulator.  
**Target:** Xbox Series X/S graphics and compute shaders on a Windows or Linux PC with an NVIDIA GeForce RTX 3060 Laptop GPU.  
**Central problem:** Preserve guest Wave64 semantics on host Wave32 hardware without treating interacting halves as independent batches.  
**Scope:** GPU shader translation and relevant integration points, not a complete console emulator.  
**Version:** 1.0 | 25 November 2026

## 1. Executive decision

Build a capability-aware, stage-aware shader recompiler around a guest-wave semantic intermediate representation (IR). Use native host execution whenever proven equivalent. For genuinely Wave64-dependent compute shaders on NVIDIA, implement two exact lowering paths: (A) two 32-lane subgroups with workgroup shared memory and explicit synchronization, and (B) one 32-lane subgroup holding two logical lane states per physical invocation (dual-bank virtualization). Start with a CPU reference model and a simple, constrained shared-memory GPU proof of concept; then implement dual-bank lowering and benchmark both. Select the actual backend per shader, rather than assuming one always wins.

Do not launch two independent 32-lane batches and expect cross-wave values to be available. Do not request a 64-lane hardware subgroup on the RTX 3060. Do not silently run unsupported Wave64 graphics shaders as Wave32: this can yield incorrect images or memory effects. In stages where no faithful lowering is available, return an explicit diagnostic.

## 2. Hardware and assumptions

| Property | Xbox Series X/S (guest) | RTX 3060 Laptop GPU (host) |
|---|---|---|
| Architecture | Custom AMD RDNA 2-class GPU | NVIDIA Ampere, compute capability 8.6 |
| Wave/subgroup width | RDNA 2 ISA defines Wave32 and Wave64; actual guest shader mode must be identified | Hardware warp size 32; expect 32-lane graphics/compute subgroups and query the driver |
| Lane communication | Guest-specific cross-lane instructions, EXEC mask, shared LDS | Native subgroup shuffles, ballots, arithmetic where advertised; workgroup shared memory in appropriate stages |
| Memory | Console memory layout and resource model differ from PC | Standard 6 GB GDDR6 dedicated VRAM, laptop-specific clocks, power and thermal limits |
| Shader input | Unknown; could be an available intermediate form or guest native machine code | Vulkan SPIR-V or D3D12-compatible DXIL as chosen by project |

The public RDNA 2 ISA is a starting point, not proof that every encoding, resource descriptor or behavior on the custom console exactly matches a retail desktop RDNA 2 GPU. Identify guest wave mode from actual accessible shader metadata, compilation mode, or validated decoding. If unknown, do not guess. Xbox Series X and Series S share the architectural family but have different resource budgets and title configurations. This handover does not assume which model or title is the first target.

Wave size is not the number of cores, compute units, or total simultaneously resident threads. NVIDIA's RT cores and Tensor cores do not turn a 32-lane warp into a Wave64. Two warps on the same SM cannot directly use a normal warp shuffle to read one another's registers. NVIDIA independent thread scheduling also means implicit warp lockstep cannot be used as a synchronization contract.

The actual laptop OS, CPU, RAM, power limit, cooling, driver, API support and codebase are unknown. Benchmark on the plugged-in machine using a stable power profile; do not predict FPS from TFLOPS or warp width alone.

## 3. Ranked options for this gaming target

These are qualitative engineering priorities, not measured performance results. Some entries are policies or optimizations rather than mutually exclusive backends.

| Rank | Method | Expected speed when applicable | Correctness / coverage | Recommendation |
|---:|---|---|---|---|
| 1 | Adaptive hybrid recompiler | Highest overall potential | High if each lowering is verified; not universal | Primary architecture |
| 2 | Native Wave32 / proven width-independent lowering | Highest individual path potential on RTX | Exact only where guest semantics are preserved | Default fast path |
| 3 | Dual-bank virtual Wave64 | Good potential; more instructions/registers | Strong for suitably remapped compute; not a generic graphics solution | Optimize for frequent cross-lane compute operations |
| 4 | Paired Wave32 + shared memory | Moderate to good; barriers and LDS-equivalent traffic | Strong for eligible workgroup-based compute | First GPU reference path and production fallback for selected shaders |
| 5 | Interleaved virtual lane mapping | Depends on access patterns | Exact with complete lane identity remapping | Optional specialization of rank 3 |
| 6 | Synchronized multi-pass/state spilling | Usually expensive | Only for explicitly supported compute transformations | Rare fallback, not naive batch execution |
| 7 | CPU reference interpreter | Unsuitable for normal game rendering | High within implemented ISA subset | Required correctness oracle and debugging tool |

Native Wave64 is a preferred path for compatible future host GPUs when device and stage capabilities match. It is excluded from the RTX-specific ranking because it is not expected to be available. Confirm driver capabilities rather than hardcoding vendor names as capability tests. Independent sequential batching is invalid for dependent lanes; it may only be used for proven independent regions or a carefully synchronized instruction/phase interpreter that preserves all dependencies.

### Stage-dependent selection

| Guest shader case | Preferred lowering | Alternative |
|---|---|---|
| Guest Wave32, supported semantics | Native host Wave32 | Exact operation-specific emulation |
| Guest Wave64, no observable dependence on grouping | Native host invocations after proof | Exact Wave64 compute paths |
| Guest Wave64, all lane interactions confined to known subgroups of 32 or smaller | Native operations with logical IDs/masks and correct guest uniform behavior, if proven | Dual-bank or shared memory |
| Guest Wave64, arbitrary cross-half communication in compute | Benchmark dual-bank vs paired shared memory | Reference/debug path |
| Guest Wave64, group barriers or complex group topology | Paired shared memory if a legal mapping and uniform barrier schedule exist | Dual-bank with full group remapping if verified |
| Pixel/vertex/other conventional graphics stage with cross-half dependencies | Stage-specific exact lowering, if one is developed | Explicit unsupported diagnostic; no general shared-memory fallback |
| Mesh/task/ray stages | Separate stage-specific analysis and capability checks | Explicit unsupported diagnostic |

Do not infer width independence solely from a lack of shuffle instructions. Wave-level scalar operations, votes, EXEC-based branches, lane IDs, quad behavior and reductions can expose wave width.

## 4. Recommended architecture

```text
Guest shader bytes / metadata (provided by existing project)
            |
            v
  Decode + validate instruction semantics and guest wave mode
            |
            v
  Semantic IR: typed values, logical lane IDs, masks, wave scope,
               scalar/vector distinction, control flow, memory effects
            |
            +--> CPU reference evaluator (test oracle)
            |
            v
  Analysis: uniformity, width sensitivity, cross-lane domain,
            workgroup topology, barriers, derivatives, register liveness
            |
            v
  Backend selector (host capabilities + stage + correctness + cost)
       |         |              |               |
       v         v              v               v
    Native    Dual-bank      Paired waves    Unsupported /
    Wave32    Wave64         + shared        stage-specific
       |         |              |               lowering
       +---------+--------------+
                 v
       SPIR-V (Vulkan) or DXIL (D3D12)
                 |
                 v
       Pipeline cache + GPU dispatch/draw + telemetry
```

Reuse the current project's graphics API and IR if present. If choosing from scratch, Vulkan is a reasonable first compute research backend for explicit subgroup capability queries and SPIR-V lowering. D3D12 is also valid, particularly if the project already translates to DXIL. CUDA can prototype and benchmark shuffle behavior, but it is not the normal rendering backend for translated vertex/pixel shaders. Do not introduce a second full graphics backend just to test a hypothesis.

### Suggested semantic IR

```cpp
// Design sketch; not drop-in C++.
enum class WaveMode { Unknown, W32, W64 };
enum class Stage { Compute, Vertex, Fragment, Mesh, Task, Ray, Other };
enum class Lowering { Native32, Native64, Virtual64, Paired64, Unsupported };
struct Mask64 { uint32_t lo, hi; };
struct ShaderFacts {
    WaveMode mode;
    Stage stage;
    bool width_independent_proven;
    bool requires_guest_wave_identity;
    bool cross_half_exchange;
    bool needs_group_barriers;
    bool uses_derivatives_or_helpers;
    bool scalar_control_or_exec_sensitive;
    bool virtual_mapping_legal;
    bool paired_mapping_legal;
    unsigned estimated_live_vector_words;
};
```

Use explicit IR operations such as `GuestLaneId`, `GuestWaveId`, `GuestExec`, `Ballot`, `Shuffle`, `ReadFirstLane`, `Reduce`, `Prefix`, `QuadExchange`, `SetExec`, `ScalarBranch`, `GroupBarrier`, `MemoryFence`, `Export` and guest-specific instructions. Each operation must carry its operand types, active-lane semantics, memory effects, legal stages, and behavior for invalid source lanes. Different instructions can have different rules; a generic shuffle is not a substitute for decoding AMD-specific DPP, DS or lane instructions correctly.

Prefer SSA values over dynamic register arrays in generated shaders, which can spill into memory. Keep a separate reference model of architectural registers for tests. Preserve source snapshot semantics for instructions whose destination aliases the source.

## 5. Native path: avoid emulation whenever safe

Ordinary per-lane arithmetic and many resource accesses can be lowered to normal host invocations. A 64-lane guest shader with provably independent invocations does not necessarily require virtualizing all 64 lanes. Nevertheless, guest lane IDs, group IDs, subgroup-wide effects, branch predicates, memory ordering and scalar values must still be preserved wherever observable.

For a native Wave32 path on a guest Wave64 shader, demonstrate all relevant operations are invariant under the width change or explicitly reconstruct their semantics. For example, a quad-local exchange might lower to a native quad instruction if the guest and host quad membership and ordering match. A 64-lane `any`, ballot, prefix sum, first-active-lane read, or EXEC-zero branch does not generally pass that test.

Native Wave64 on other hosts requires capability, valid pipeline stage, compatible invocation layout and supported operations. Requesting a subgroup size does not itself emulate any guest ISA operations.

## 6. Dual-bank virtual Wave64 (one host subgroup)

### 6.1 Contiguous mapping

A host lane `p` owns two guest lanes: `p` and `p+32`. Each live guest vector value has two per-invocation SSA values or register banks. Guest scalar state is logically one value per wave; a host compiler may replicate it in physical registers, but the semantic value is uniform across the logical wave.

```text
Host lane:  0      1      ...  30      31
Bank 0:     0      1      ...  30      31   (guest lanes)
Bank 1:     32     33     ...  62      63
```

`owner(g) = g & 31`, `bank(g) = g >> 5`. For example guest lane 31 reading guest lane 32 obtains bank 1 from host lane 0. All 64 logical values are accessible within one physical subgroup.

### 6.2 Arbitrary lane shuffle

The following is shader-IR pseudocode. `shuffle32` must lower to a supported subgroup shuffle, executed in a valid converged context, with every requested physical source invocation participating. Logical activity and physical participation are separate. Handle invalid guest sources according to the exact instruction, before consuming the result.

```cpp
uint fetch64(uint old0, uint old1, uint target /* validated 0..63 */) {
    uint p = target & 31u;
    uint a = shuffle32(old0, p);
    uint b = shuffle32(old1, p);
    return (target & 32u) ? b : a;
}
// All 32 host lanes execute the same collective instruction instances.
uint r0 = fetch64(old0, old1, safe_target0);
uint r1 = fetch64(old0, old1, safe_target1);
new0 = guest_write_allowed0 ? apply_source_policy(r0) : old_dest0;
new1 = guest_write_allowed1 ? apply_source_policy(r1) : old_dest1;
```

A general pair of arbitrary guest source indices may require four native shuffles per 32-bit component; known bank/owner, uniform sources or common indices can reduce the cost. For XOR-32 with contiguous mapping, swap the two local banks: zero cross-lane shuffles. For XOR-1, shuffle within each bank. For a broadcast with one source index shared by all destinations, fetch once and reuse. Do not move a per-destination bank selection before a shuffle unless the selection is proven correct for every source; otherwise the source chooses the wrong bank.

Always read original operands from both banks before updating either destination. Example: a guest XOR-32 operation in which the destination equals its source must exchange the two old values, not copy the first new value twice.

### 6.3 Mask and collective handling

Store guest EXEC, validity and relevant compare/predicate state explicitly (e.g. `uvec2`). A host subgroup ballot of bank 0 predicates creates `lo`; a ballot of bank 1 creates `hi`. Include the required guest eligibility conditions, not just the host active mask. Keep physical threads participating in communication even when both their guest lanes are inactive, where necessary. Never shift a 32-bit integer by 32 to build a mask. A helper for bit access should select the word and use `g & 31` as the shift count.

```cpp
Mask64 ballot64(bool pred0, bool pred1, bool eligible0, bool eligible1) {
    return { ballot32(pred0 && eligible0),
             ballot32(pred1 && eligible1) };
}
uint count64(Mask64 b) { return popcount(b.lo) + popcount(b.hi); }
// first-set: use lo if nonzero, otherwise 32 + firstSet(hi).
// If both zero, use the specific guest operation's empty-mask policy.
```

For associative integer reductions, reduce each bank with a neutral element for ineligible lanes and combine the partials if guest arithmetic semantics permit it. For a guest exclusive prefix sum over contiguous lanes, compute two bank-local prefixes and add the total of bank 0 to each eligible bank 1 result. Preserve the exact guest arithmetic type, overflow rules and floating-point ordering/rounding wherever defined. Host floating-point tree order or fused operations may differ; do not promise bitwise equivalence unless required semantics are reproduced and tested.

### 6.4 Divergence, scalar execution and side effects

Guest EXEC updates must control logical execution without accidentally removing the host lane needed to supply a later shuffle. Lower vector operations using explicit predication, with guest mask stacks or an appropriate CFG transformation. If the guest scalar PC is shared by the logical wave, emulate it once logically; avoid issuing a scalar store twice per bank or once per physical invocation. Handle VCC/SCC and width-specific EXEC rules according to the decoded ISA.

Do not speculatively execute masked loads, stores, atomics, exports or other effects just to make control flow uniform. Predicate effects, use valid dummy operands only where legal, then reconverge all required physical participants before subgroup collectives. A naive host branch around a collective, based on the bank's active flag, is incorrect. Check SPIR-V reconvergence guarantees and device support; CUDA-style masks do not automatically translate to Vulkan/HLSL.

### 6.5 Workgroup remapping (compute)

For a guest group with N logical invocations, a virtual path can use `32 * ceil(N / 64)` physical invocations, one full physical subgroup per logical wave, with padding for missing guest lanes. This is only a starting formula: validate API limits, 3D guest local-size mapping, full subgroup properties and guest local-index reconstruction. Keep every physical workgroup participant alive across required group barriers, including padding. Reconstruct both guest local and global IDs and use guest sizes for resource addressing and shared allocation. Dispatch group counts normally stay the same if one host group implements one guest group.

Use a verified `(subgroup_id, subgroup_local_id)` mapping or another explicitly established mapping; do not assume that native subgroup order is always identical to linear local invocation order across APIs. Host shader local dimensions can change while guest-observable IDs must not. A padded lane never performs guest side effects. Workgroup LDS addressing, memory scopes, barriers, shared atomics and guest group size need separate validation. Some shaders are not safely remappable and must use another path.

### 6.6 Cost on RTX 3060 Laptop

Two logical vector states increase register liveness, ALU instruction count and potentially spills. This does not imply an exact 2x slowdown. Fewer physical invocations, better communication locality and fewer barriers can help; lower occupancy can hurt. Ampere SM 8.6 has a 64K 32-bit register file per SM and a maximum of 48 concurrent warps per SM [3]; these are architecture limits, not an occupancy prediction for an actual generated shader. Track spills, register allocation, resident warps, and per-dispatch time. Avoid keeping both banks live when static liveness analysis permits reuse.

## 7. Paired Wave32 with workgroup shared memory

Keep a separate physical invocation for each guest lane. Explicitly pair two host subgroups in one host workgroup, with verified logical IDs and mapping. Use workgroup memory (Vulkan `Workgroup` storage or HLSL `groupshared`) for values crossing the 32-lane boundary. Each guest logical wave gets isolated scratch storage. One 32-bit value per logical lane requires 256 bytes per pair; multiple operands and wave pairs multiply the allocation, plus any guest LDS requirements.

```cpp
// Conceptual compute shader; all physical workgroup invocations must
// execute BOTH barrier instances in the same legal dynamic order.
uint pair = logicalWaveId;
uint lane = logicalLaneId; // 0..63, verified mapping
scratch[pair * 64 + lane] = valid_source_storage ? source : safe_dummy;
workgroup_memory_and_execution_barrier();
uint fetched = scratch[pair * 64 + safe_target];
workgroup_memory_and_execution_barrier();
if (guest_can_write) dest = apply_guest_source_policy(fetched);
```

Vulkan: emit an appropriate `OpControlBarrier` with workgroup execution and memory scope and semantics for Workgroup memory. GLSL compute `barrier()` or HLSL `GroupMemoryBarrierWithGroupSync()` may express this specific pattern when used legally. Guest fences involving other memory classes require their own translation. The second barrier prevents early overwrite while another invocation is still reading. Ping-pong scratch can change the synchronization schedule but does not remove the need to prove lifetime safety.

The major limitation is barrier scope: a workgroup barrier involves the entire physical group, not merely two selected warps. Other logical waves may follow different guest scalar control flow or reach different wave collectives. Inserting a barrier into such divergent flow is invalid or can hang. Never fix this by spinning on a flag between warps or relying on scheduler progress. Restrict the initial implementation to one logical wave per group and statically uniform collective control flow; later support multiple logical waves only with a proven common barrier schedule or a different lowering. Do not split a guest workgroup into unrelated host workgroups just to obtain this property if the guest group shares LDS or barriers.

Optimize after correctness: within-half operations can use native shuffles; cross-half operations use scratch. The decision to execute the workgroup barrier must be uniform across the whole group; a per-lane `if (target crosses half)` around a barrier is not valid. Reduce scratch traffic using bank-local partial collectives where appropriate. This method maintains the original one-invocation-per-lane model more naturally than dual-bank virtualization, which is why it is a good first constrained GPU reference implementation.

## 8. Interleaved mapping: optional optimization

Alternative virtual mapping: `owner(g) = g >> 1`, `bank(g) = g & 1`. Host lane p owns guest lanes `2*p` and `2*p+1`. XOR-1 becomes a local bank swap. XOR-32 becomes a cross-host shuffle with owner XOR-16. Contiguous mapping instead makes XOR-32 local, while retaining sequential memory accesses in each bank. Choose based on actual shader access patterns, not just the word "neighbor".

Interleaving changes the mapping for ballots, prefixes, lane IDs, memory coalescing, and quad layout. A ballot of even/odd banks must spread bits into even/odd positions of the logical 64-bit mask rather than simply concatenate the two words. Prefix sums require guest order, not all evens followed by all odds. Keep contiguous mapping as the initial implementation, and treat interleaving as an optional compiler specialization validated against the same reference model.

## 9. Graphics shader limitations (critical for games)

Compute virtualization alone does not make an Xbox title render correctly. Conventional vertex and fragment stages do not give the shader a controllable 64-invocation workgroup with workgroup shared storage and barriers. A host fragment invocation normally corresponds to rasterizer-generated coverage, interpolation, sample and quad state. Two independent guest fragments cannot simply be mapped onto one host fragment and exported as if both were independently rasterized.

- First support width-independent and genuinely Wave32 vertex/fragment shaders using normal graphics pipelines. Preserve guest resource bindings, shader exports and built-ins.
- For quad operations, verify logical quad membership and component ordering. Fragment derivatives, implicit texture gradients/LOD, helper invocations, sample shading, discard/demote, depth/stencil and output coverage require separate correctness checks. Never replace derivatives with arbitrary neighboring-wave shuffle values.
- A guest whole-wave vote/reduction/shuffle that crosses physical fragment subgroups is not generally implementable through ordinary Vulkan/D3D12 subgroup operations. No generic cross-warp graphics barrier exists. Do not add SSBO polling, spinlocks, or assume different fragment subgroups will be scheduled together. Such a design can deadlock and fail to reconstruct guest wave membership.
- More advanced approaches (explicit primitive/pixel scheduling, compute-based rasterization, or shader-specific algorithm rewriting) are research projects. They may require additional passes, large temporary buffers, and reproducing fixed-function graphics behavior. Do not label them as implemented or universal fallbacks. They are a poor default for a 6 GB laptop GPU.
- Keep unsupported shaders visible in a per-title report with opcode, stage, shader hash, guest wave mode and reason. Prefer an honest failure over a silent approximation. Optional developer-only approximations must be explicitly enabled and clearly flagged, never the default correctness path.

Mesh/task shaders may expose a workgroup, but their payloads, output allocation and synchronization need dedicated mapping. Ray tracing has its own invocation and traversal semantics; do not assume compute lowering applies. Graphics-stage coverage, not only compute performance, will likely determine whether the wave translation subsystem is sufficient for a particular title.

## 10. Runtime capability checks

Vulkan: query `VkPhysicalDeviceSubgroupProperties` (or corresponding Vulkan 1.1 properties) for default size, supported stages and operations. For Vulkan 1.3 or `VK_EXT_subgroup_size_control`, query feature support and `VkPhysicalDeviceSubgroupSizeControlProperties`, including min/max sizes and required subgroup size stages. Use a valid full-subgroup pipeline configuration when the virtual backend relies on 32 participating lanes. Check local size limits, subgroup count limits, workgroup shared memory, extended subgroup types, and required reconvergence capabilities. Use explicit device selection if the laptop also exposes an integrated GPU.

D3D12: query `D3D12_FEATURE_D3D12_OPTIONS1` for `WaveOps`, `WaveLaneCountMin`, `WaveLaneCountMax`; query shader-model support separately. Use `[WaveSize(32)]` only on supported shader stages and versions; the SM 6.6 facility was introduced for compute. Confirm stage-specific wave operations, helper behavior, and a legal full-wave mapping rather than assuming all HLSL invocations are active. Do not request `[WaveSize(64)]` on a driver that supports only 32.

A simple startup report should contain GPU identity, API/driver version, subgroup size/range, subgroup operations and stages, full subgroup capability, shared-memory and workgroup limits, available device-local memory budget, and selected defaults. Keep capabilities part of the shader/pipeline cache compatibility key.

### Example backend selector (illustrative)

```cpp
if (guest.mode == Unknown && !guest.width_independent_proven)
    return Unsupported("unknown guest wave semantics");
if (guest.mode == W32 && native32_equivalent_and_supported(guest, caps))
    return Native32;
if (guest.mode == W64 && native64_equivalent_and_supported(guest, caps))
    return Native64; // not expected on RTX 3060
if (proven_width_independent_or_exact_local_lowering(guest, caps))
    return Native32; // or normal invocations without subgroup ops
if (guest.mode == W64 && guest.stage == Compute) {
    // Each predicate includes verified topology, synchronization, opcode
    // coverage, source-lane semantics and host capability checks.
    bool v = virtual64_legal(guest, caps);
    bool p = paired64_legal(guest, caps);
    if (v && p) return choose_by_profile_or_conservative_cost(guest);
    if (v) return Virtual64;
    if (p) return Paired64;
}
return Unsupported("no verified stage-specific lowering");
```

The native path must not accidentally change shader-observable grouping or uniform/scalar side effects. Unknown instructions, wave modes and mapping constraints must be represented as unknown, not defaulted to safe.

## 11. RTX 3060 Laptop performance and memory strategy

1. Keep native fast paths for width-independent shaders, especially common graphics stages. Avoid CPU round trips during draws or dispatches.
2. Specialize known lane patterns (same bank, XOR-32 swap, uniform broadcast, quad exchange); use general fetch only when necessary. Track live bank values and eliminate dead operations. Preserve exact semantics while doing so.
3. Measure register spills and occupancy rather than optimizing only instruction count. The shared-memory backend can outperform dual-bank if the latter spills heavily; dual-bank may win in frequent cross-half shuffle kernels with modest live state.
4. Cache translated IR and compiled shaders/pipelines with keys that include guest shader hash, wave mode, stage, backend, mapping, specialization, relevant device/driver features and translator version. Bound in-memory caches; persist suitable caches to disk, invalidate safely and avoid unbounded shader compilation queues.
5. Track VRAM budget and residency rather than assuming all 6 GB is available. Reuse scratch buffers, avoid repeated copies, and account for textures, render targets, guest memory mirrors, pipeline resources and other applications. Vulkan memory budget extension or DXGI budget queries can assist where available.
6. Test at the original guest configuration first for correctness; offer a separate resolution-scaling option later if the renderer can preserve guest resource semantics. Start performance evaluation with modest workloads (e.g. 720p/1080p if the title allows it), not a promise of 4K/60 FPS. Record laptop power and thermal conditions.
7. Use GPU timestamp queries and, when available, NVIDIA Nsight Graphics for actual graphics/compute pipeline profiling. CUDA microbenchmarks are useful for intuition but do not replace profiling the real graphics API backend.

## 12. Validation plan

### CPU oracle

Build a reference model with 64 logical lane slots per vector register, guest scalar state, explicit validity/EXEC masks and decoded opcode semantics. Step one logical guest instruction at a time using source snapshots and model its side effects. Keep this deliberately simple and independent of GPU lowering. Start with a specified synthetic semantic subset, then extend based on verified guest ISA. For undefined behavior, compare only required/defined results; do not invent guarantees.

### Deterministic test suite

Use input value `0x1000 + guest_lane` unless otherwise specified. All lanes valid and active unless stated. Examples below are synthetic operation contracts, not claims that every AMD opcode has identical rules.

| Test | Expected result / property |
|---|---|
| Identity | lane g receives 0x1000+g |
| Boundary read 31→32 | lane 31 receives 0x1020 |
| Boundary read 32→31 | lane 32 receives 0x101F |
| XOR-32 in-place | lane g receives 0x1000+(g XOR 32), using old values |
| XOR-1 in-place | lane g receives 0x1000+(g XOR 1) |
| Arbitrary shuffle | Every legal (source,destination) pair matches oracle; 4096 pairs |
| Dynamic mixed banks | Distinct source indices per destination, both banks, no accidental source-side select |
| Ballot | Active set {0,31,32,63} yields lo=0x80000001, hi=0x80000001 |
| First-active lane | hi-only mask with bit 5 set returns guest lane 37 |
| EXEC empty | No masked vector side effect; use opcode-specific scalar/collective behavior |
| Integer sum of lane IDs | 0+...+63 = 2016 |
| Exclusive prefix of ones | lane g receives g (all active) |
| Partial group | 1, 31, 32, 33, 63, 64, 65, 96, 128 guest invocations where legal; no padded effects |
| Aliasing and repeated operations | No early overwrite, stale scratch, or cross-wave scratch interference |
| Divergence | Bank 0/1 differ, nested EXEC masks, loops, reconvergence and EXEC restoration |
| Scalar side effects | Correct logical issue count even when replicated across physical lanes |
| Group LDS and barriers | Verify guest local IDs, memory addresses, fence semantics and legal barrier scheduling |
| Graphics quads | Known 2x2 gradients, texture LOD, helper/discard and sample behavior where supported |
| Unsupported graphics | Explicit actionable error for unsupported cross-half operation |

Add randomized masks, source/destination aliasing, cross-half access patterns, 3D groups, arithmetic limits, valid and invalid lane indices, divergent CFGs, and floating-point special values under the correct opcode semantics. Compare CPU oracle, native path where equivalent, virtual path, and paired path. Run GPU validation layers and device-loss/watchdog checks; validation layers alone cannot prove subgroup semantics or absence of all deadlocks.

### Benchmarks

Use the same semantics and inputs on both GPU paths. Record median and tail GPU dispatch times, register count/spills when tooling exposes them, shared-memory bytes, inserted barriers, host subgroup operations, compilation time, cache hit rates, VRAM usage and correctness mismatches. Cover shuffle-heavy, ALU-heavy, memory-heavy, divergence-heavy, and reduction/prefix shaders. For games, additionally record frame time distribution, graphics/compute bottlenecks, shader/pipeline misses and unsupported stages/opcodes. No benchmark numbers are available yet; fill in measured results instead of estimating FPS.

## 13. Milestones and acceptance criteria

| Phase | Deliverable | Completion gate |
|---|---|---|
| 0 | Repository audit and GPU capability probe | Input shader format, API, stage, wave-mode evidence and device capabilities documented |
| 1 | Wave semantic IR and CPU reference | Deterministic synthetic tests pass, undefined cases explicitly represented |
| 2 | Native Wave32 path and width analysis | Verified native results; unsafe Wave64 cases rejected |
| 3 | Paired compute prototype | One logical wave per group; valid barriers, cross-half tests pass on real GPU |
| 4 | Contiguous dual-bank compute | Correct shuffles, masks, scalar control, IDs, divergence, side effects and partial groups for supported subset |
| 5 | Adaptive selection and profiling | Both compute backends compared on RTX 3060; selector never chooses an illegal path |
| 6 | Graphics-stage integration | Native graphics and verified quad operations pass tests; unsupported cases reported |
| 7 | Title-specific profiling and expansion | Authorized test shaders/captures demonstrate actual stage/opcode coverage, correctness and measured frame times |

Do not defer all validation until game boot. Each milestone should be independently testable. If the repository already implements a milestone, assess existing coverage before duplicating work.

## 14. Proposed repository organization (adapt, do not force)

```text
src/gpu/shader/
    decode/                  # existing decoder or guest shader frontend
    ir/                      # typed semantic IR and CFG
    analysis/                # wave-width, topology, effects and liveness
    reference/               # CPU semantic oracle
    lowering/native_wave/
    lowering/virtual_wave64/
    lowering/paired_wave64/
    backend/                 # existing SPIR-V or DXIL emitter
    capabilities/            # runtime capability discovery
    cache/                   # bounded translation/pipeline cache
    diagnostics/             # unsupported-op and title coverage reports
 tests/gpu/wave/
    unit/                    # CPU and IR tests
    differential/            # CPU vs GPU tests
    graphics/                # stage-specific correctness
    benchmarks/              # controlled performance workloads
 docs/gpu/
```

Do not replace the existing build system or introduce needless dependencies. Keep backend-specific lowering isolated from guest semantics. This permits future AMD Wave64 support without requiring a separate frontend.

## 15. Other Xbox compatibility dependencies

Wave translation is only one subsystem. Running Xbox Series games also requires compatible executable/runtime and OS interfaces, command streams and queue synchronization, resource formats/tiling and descriptors, guest GPU virtual addressing and memory coherence, shader input decoding, pipeline state, render/depth exports and other fixed-function features, and potentially ray tracing, audio, input, storage and platform services. The appropriate translation boundary depends on what the project can legally and technically access. Prefer authorized development shaders, homebrew or other test materials you have permission to use. This design does not provide any means of obtaining proprietary binaries, credentials, keys or bypassing protection.

A correct GPU wave subsystem does not establish that a commercial title will boot, render or achieve playable performance. The guest shader format and current project state were not provided, so actual ISA decoding, API integration and compatibility remain open.

## 16. Public technical references

Use these to verify API and architecture facts. They do not constitute a console-specific SDK or proof of custom Xbox shader behavior.

1. Microsoft Xbox Wire, Xbox Series X architecture overview: https://news.xbox.com/en-us/2020/03/16/xbox-series-x-tech/
2. AMD RDNA 2 Shader ISA reference (PDF): https://www.amd.com/content/dam/amd/en/documents/radeon-tech-docs/instruction-set-architectures/rdna2-shader-instruction-set-architecture.pdf
3. NVIDIA Ampere Tuning Guide (SM 8.6 resource limits): https://docs.nvidia.com/cuda/ampere-tuning-guide/index.html
4. NVIDIA CUDA Programming Guide, advanced kernel programming and warp model: https://docs.nvidia.com/cuda/cuda-programming-guide/03-advanced/advanced-kernel-programming.html
5. NVIDIA RTX 30-series laptop comparison: https://www.nvidia.com/en-eu/geforce/laptops/compare/30-series/
6. Khronos Vulkan Guide, subgroups: https://docs.vulkan.org/guide/latest/subgroups.html
7. Vulkan subgroup size control properties: https://docs.vulkan.org/refpages/latest/refpages/source/VkPhysicalDeviceSubgroupSizeControlProperties.html
8. Vulkan shader execution, subgroup and derivative semantics: https://docs.vulkan.org/spec/latest/chapters/shaders.html
9. Vulkan subgroup uniform control flow: https://docs.vulkan.org/guide/latest/extensions/VK_KHR_shader_subgroup_uniform_control_flow.html
10. Microsoft D3D12 wave capabilities: https://learn.microsoft.com/en-us/windows/win32/api/d3d12/ns-d3d12-d3d12_feature_data_d3d12_options1
11. Microsoft HLSL WaveSize specification: https://microsoft.github.io/DirectX-Specs/d3d/HLSL_SM_6_6_WaveSize.html
12. Microsoft HLSL wave intrinsics and helper lane discussion: https://github.com/microsoft/DirectXShaderCompiler/wiki/Wave-Intrinsics
13. Vulkan shader interface built-ins and full subgroup mapping: https://docs.vulkan.org/spec/latest/chapters/interfaces.html

## 17. Direction for Claude

Read the companion `CLAUDE.md`. Audit the existing project before implementing anything. Treat the above as a design proposal with correctness gates rather than an instruction to add every backend immediately. The initial priority is a reliable Wave32 native path plus a constrained Wave64 compute test; the long-term objective is an adaptive recompiler with explicit unsupported-graphics handling. Do not claim performance or game compatibility until tests and actual target-hardware profiling support it.
