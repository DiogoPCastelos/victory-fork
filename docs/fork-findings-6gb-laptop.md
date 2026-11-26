# Findings on a 6 GB laptop GPU (2026-11-24 and 25)

What was run, read and changed while bringing the title up on a machine well
below the fork's test machine, and what is still open. Written for whoever
picks this up next; every number below comes from this machine's logs and
captures (none of which are in the repository: see
[asset-policy.md](asset-policy.md)).

| | Fork's test machine | This machine |
|---|---|---|
| GPU | RTX 4070, 12 GB | RTX 3060 Laptop, 6 GB (32-lane waves only, like every NVIDIA GPU) |
| CPU | i7-14700KF | i7-11800H, 8 cores |
| RAM | 32 GB | 16 GB (about 1 GB free during a world run) |
| Intel iGPU | none | present; the host device is made on the NVIDIA GPU (checked by its memory counters) |

## How to run it

```powershell
cargo build --release -p victory
# warm the shader cache as this GPU's runs read it (NVIDIA: 32-lane waves,
# a view heap capped at 1,000,000 views, as a run's log says)
$env:VICTORY_HOST_WAVE=32; $env:VICTORY_VIEW_LIMIT=999999
target\release\victory.exe translate <the title's shader archive> --warm all
target\release\victory.exe translate <game dir>\XboxBvhBuilder.dll --warm all
Remove-Item Env:VICTORY_HOST_WAVE, Env:VICTORY_VIEW_LIMIT
target\release\victory.exe run <game dir>\<the title's exe> --execute
```

Warming (upstream's `translate --warm`) translates every shader container
into the cache a run reads; the whole pack (26,503 containers) takes about
2.5 minutes here. The cache is keyed on the translator's source, so a build
that does not touch the translator starts warm. The key also holds the build
kind (`translation_fingerprint`: the `diagnostics` feature and debug
assertions), so a diagnostics build (`cargo build --profile diag -p victory
--features diagnostics`) has a cache of its own: warm it with the binary that
runs. A new key also means the driver's pipelines are made again at the next
run's start (about 1,100 here, minutes of menus while they are made). The translation has to be
the one a run on this GPU makes: its wave width and view-heap limit, which
a run's log gives ("host wave lanes ... max=32"; "host view heaps hold
1000000 views", whose last slot, 999999, is the limit). This branch had
a warm-up of its own that made the host device first to get them; upstream's
replaced it. Driver pipelines cannot be made ahead of a run (they need the
render-target formats and other state the title passes when it creates
them); every pipeline a run uses is kept and made again at the start of the
next one.

## The runs

About twenty runs, Series X profile unless said otherwise; logs and captures
stayed outside the tree.

| Run | What it showed |
|---|---|
| First, cold caches | Menus at 30 fps; the opening cutscene as a blue screen with its sound: about 1,300 pipelines still being made, 800-4,000 draws a frame dropped for them, 0.2-1 fps. Ended after 8.5 minutes by the title itself: its object pools were full (instances 45,000 of 45,000, buildings 22,272 of 22,500, 3,000-6,000 streaming requests queued). Its crash reporter tried to upload a minidump; the network block refused it. |
| Second, warm pipelines | Same, ended the same way after 8 minutes. |
| Third, all pipelines made | 0.2-0.7 fps, 0 pipelines left to make but still 500-1,000 draws a frame dropped (see "What is dropped"); a frame took 1-6 s, most of it uploading textures. |
| Series S profile | Black from the start: the title presents two planes and only the first was shown (below). In the world, the host device was removed within seconds (below). |
| Captures (`VICTORY_CAPTURE`, `VICTORY_DUMP_TARGETS`) | The world's geometry is complete in the G-buffer's normals; walls and skin black in its albedo (below). |
| Texture experiments | Bigger budgets and the shared-memory spill (below): faces and walls get their colour. |
| Ray-tracing builder on | See "Ray tracing". |

## What is dropped, and why

The frame line's "dropped N without a host pipeline" counts dispatches too.
In a captured world frame 664 of the drops were the ray-tracing structure
builder's stages (off by default), a few were compute dispatches of the five
programs this GPU refuses (64-lane waves, below), and none were the world's
draws. So once the pipeline cache is warm, missing objects are not dropped
draws.

## Black walls, faces and clothes

The G-buffer of a world frame has every wall, door, arm and face in its
normals target, while its albedo target is black for skin and building
materials and coloured for clothes and hair. The materials that are black
pick their textures from the view heap by a computed index; the coloured ones
read theirs through descriptor tables. Textures picked by index are uploaded
by a queue (`draw.rs`, `heap_textures`) that stopped for two reasons on this
machine:

1. Throughput: 192 MB and 250 ms a pass. The world's first minutes had
   11,000-27,000 slots waiting, the oldest written 2,000-3,000 frames
   earlier.
2. Room: the queue stops once the card's memory in use reaches nine tenths of
   the budget Windows gives the process. On 6 GB that is reached in the
   world's first frames.

Measured with captures of the same shots:

| Setting | Albedo |
|---|---|
| Defaults (then) | walls, skin, uniforms black |
| `VICTORY_TEXTURE_BUDGET_MB=3000` alone | still black: the queue's own limits held it |
| plus 2 GB and 4 s a pass | walls, beds, rails, skin of most characters |
| plus the shared-memory spill | the main character's face, brows and lips; the concrete walls |

Upstream reached the same queue from another side at the same time: textures
uploaded before the title had filled them kept zeros (now looked at again
after their upload), and a catch-up budget (three times the bytes and time
while 1,024 or more slots are due). This branch keeps upstream's queue and
adds the spill: past the card's share, textures picked by index go on into
the PC memory the system shares with the card, up to half of that budget
(`VICTORY_TEXTURE_SPILL=0` turns it off). A texture there is read over the
bus: slower, not missing.

Still open: the queue does not know which slots the frame samples. The fix
that removes the problem instead of shortening it is GPU feedback: each
translated program that picks a texture by index sets the slot's bit in a
buffer, read back the next frame, and those slots go first.

## Light in shadow

Anything the sun does not reach directly looks like dusk: the title's bounce
light and its lamp and sun shadows are ray traced, and the ray-tracing
structure builder is off (below). The static world's structures ship
pre-built, and the trace programs that walk them do light it: a 2026-11-26
capture with them held back (`VICTORY_BVH_HOLD=1`) shows the dorm almost
black, and lit without the switch. What the builder would add (skinned
characters, moving objects) is missing from the structures the rays walk;
how much of the picture that changes has not been looked at. The depth bounds test
(upstream, #82) lit the first room by its lamp on the fork's machine.

## Ray tracing

The builder runs all or none: one builder program without a host pipeline
keeps the whole builder off (`pipeline.rs`, `Builder`). On this GPU three of
the 53 were missing, all three for the same reason: block-wide prefix sums
over a 512-thread group of 64-lane console waves. Each wave writes its total
to shared memory, the group meets at `s_barrier`, and each lane then sums the
totals of the waves before its own in a loop whose exit depends on its wave's
number, broadcasting the running sum with `v_readfirstlane`. Run as two
32-lane host waves per console wave, a first-lane read inside a branch only
some lanes take was refused (the halves cannot meet there).

The running sum is the same in every lane of the console wave: it is a scalar
plus a shared-memory read at a scalar address. `lane_uniform` now proves that
for shared-memory reads whose memory has settled (`lds_settled`: no write
follows the read on any path, and every write before it reaches it only
through an `s_barrier`), so both halves take their own first lane, which
holds the same value. Upstream had added `VICTORY_FIRSTLANE_OWN=1` for these
same scans as an opt-in shortcut; the proof makes it automatic where it
holds. All 52 builder programs of the title's DLL now translate, pass
`dxil.dll` and make pipelines on the RTX 3060 (49 before).

A test runs the same shape on the GPU (`wave64_tests.rs`, a scan over the eight
console waves of a 512-thread group, written for the test): on the RTX 3060,
as two 32-lane halves per console wave, every thread gets its waves'
predecessors' sum. Making it pass also fixed an over-cautious check in
`lane_uniform`: it took the destination's old value, which the IR records as
read, for one of the operand's registers, so an instruction whose
destination was not one of its sources was never uniform.

In a run (`VICTORY_BVH_BUILDER=1`), for the first time in this fork:
"the ray-tracing structure builder runs: all of its programs have host
pipelines pipelines=53". It is not usable yet:

- Builder on: 13 s after the builder's first frame the title faulted on
  pointers that held float bit patterns (`0x4084003e99`) and all-ones, as in
  the fork's run 98: a builder stage stores where it should not, over the
  title's own objects.
- Builder on with `VICTORY_BVH_QUARANTINE=1` (its stores stay in the layer's
  copies): the title's memory stays whole, and 7 s later the layer could not
  make its 4 MB host copies of title memory any more (4,889 refusals in 6 s,
  out of memory), then a command list failed to close. A stage that computes
  wild addresses makes the layer copy memory it was never meant to touch.

Both point at a builder stage computing wrong addresses. Not the three
scans: the GPU test above checks their form. The next step is to check each
of the builder's stages on the GPU against its expected output (the fork's
`bvh.rs` reference covers the ray test, not the builder), starting from the
stage whose dispatch first names memory outside the structures' buffers.
The builder stays off by default, so none of this touches a normal run.

## Series S profile

- The host device was removed seconds into the world: the title makes a
  1920x1080 R9G9B9E5 texture with render-target and unordered-access use,
  which the console GPU allows and the PC does not (it only samples that
  format). A stand-in format fixes it (`target_format`). This branch first
  used R11G11B10_FLOAT, as XCL does for the same texture; upstream's
  RGBA16F, which keeps every bit of R9G9B9E5's precision, replaced it.
- The screen stayed black because the title presents two planes: its 3D
  plane (1920x1080) and its UI plane (3840x2160), and the layer showed the
  first only. The plane description is 72 bytes, not the 24 the layer read
  (the second plane's count read as garbage). The layout is now right and
  logged when it changes; composing the planes is still to do.

## Shaders

Pack-wide, as for this GPU (`VICTORY_HOST_WAVE=32 victory translate
<the shader archive> --validate`): every one of the 26,084 programs is lowered, and
254 containers do not translate on the run's path.

Fixed while this was written, each checked with `--validate --pipelines` on
the RTX 3060: the deferred material pixel programs (a first interpolation
half read where no second half is), the puddle programs and a shallow-water
reduction (descriptors read only for an image's size or a buffer's record
count), two water programs (double-precision whole-wave sums), and the
programs whose only cross-lane code is `v_permlane16`/`x16`. This branch had
fixes of its own for the first two; upstream's (pairs evaluated as their
pair on a retry, `unused_descriptor_kind`) replaced them, and all those
programs still translate. What this branch keeps is the console-lane order
for `v_permlane16`/`x16` programs (upstream lowers the instructions but did
not ask for the console's lane order there).

Still failing, by cause. Pack census of 2026-11-26 on `main` after the
far-jump commits (`translate --list --validate --pipelines`, 32-lane host,
view limit 999,999): 26,599 containers, 26,151 programs lowered, and every
emitted program makes a pipeline on the RTX 3060. Of the 270 containers
that do not translate, the opening binds none (four runs to the world logged
no shader that does not translate):

| Programs | Cause |
|---|---|
| 195 geometry | an address in an SGPR that is no root parameter (no geometry-shader path) |
| 40 (37 pixel, 3 mesh) | a sampler chosen by control flow, one of them constant |
| 29 | no DXIL part (SM5 or older; a DLL the title does not load) |
| 5 compute | a descriptor read only as data |
| 1 pixel | an opaque lane mask |

Translated but refused by `dxil.dll`'s validator (the driver takes them all):
216 programs, 111 vertex/pixel and 105 mesh. 71 mesh programs output more
than 32 KB; 72 have an input or output signature not as the module expects;
about 70 pack elements of different interpolation modes into one row; 3
pixel programs have an out-of-bounds coordinate that is not undef.

(The table this replaces, from 2026-11-25, listed 5 compute programs still
needing 64-lane waves, 2 the driver refused and the cascade-shadow program;
none of those appear in this census.)

The census also lists 1,373 programs (1,041 compute, 309 pixel, 20 mesh, 3
vertex) whose per-lane branches are not the console's (`lane_exact.rs`):
their translation is unchanged, the list says where to look. An attempt to
route such compute programs to an exact form first changed 1,024 wave32
programs (337 of them bound in the opening) and was followed by a black world
in the game; it was dropped.

## GPU time in the world (2026-11-26)

On the RTX 3060, with `VICTORY_CARD_METER=1 VICTORY_CARD_METER_PIPELINES=1`,
one 256-frame window in the world: 376 ms of the card's time a frame.

- The most expensive programs run as native Wave32, not as an emulated
  64-lane wave: the top one is a skinning program feeding the ray-tracing
  builder, 51 ms a frame in 17 dispatches of 2.9 ms. The dual-bank program a
  tester's breakdown pointed at costs 1-2 ms.
- About 70 ms a frame are six skinning programs whose only reader in the
  captured frames is the builder, which is off. About 40 ms are the programs
  that walk the structures; those light the static world and must stay.
- A/B, same switches, first 41 world windows each: nothing skipped, median
  frame 818 ms; the six skinning programs skipped, 649 ms, the same lighting
  in the captures; skinning and walking both skipped, 651 ms and the dorm
  almost black. One pair of runs, game clock and shots differ; the draws a
  frame were 5,655 against 6,318.

The "memory wait" in a tester's logs (5060 Ti, three runs) is the
submitting thread waiting for the card at the next point it must: the wait
attributed to title memory read and the one to title memory changed under
shader writes add up to about 300 ms a frame with and without the second copy
queue, against 290-315 ms the frame spends waiting for the card. The copy
queue moves the wait from one reason to the other.

## Intro video stutter

The opening video drops to 3 fps while the pipeline workers make the cached
pipelines (8 workers on an 8-core CPU, below normal priority, but every core
busy) and holds 24-30 fps once they are done. A start-up cost only.

## Stability

- One GPU hang in about twenty runs (the driver reset itself); the runs on
  either side with the same shader changes did not hang.
- One access violation inside the NVIDIA driver's own code, in a run with
  the builder switch on but the builder off, on a machine with about 1 GB of
  RAM free. A driver failing an allocation crashes this way; the pipeline
  workers now wait while the system's commit is nearly used up (below).
- With the builder running, see "Ray tracing".
- 2026-11-26: twice, in runs with `VICTORY_CARD_METER_PIPELINES=1`, an access
  violation inside the NVIDIA driver (`nvwgf2umx.dll+0x422e03`, a read at
  0x36) on entering the world, from `ResourceBarrier` in a list's state
  prelude (`host.rs`, the barriers after the copies between depth textures
  and their colour twins). Same address and stack both times; a run without
  the meter reached the world. Not explained yet; a run with
  `VICTORY_D3D12_DEBUG=1` should name the barrier.
- 2026-11-26: one run lost the host device ("device removed") in the world
  with the PC at 437 MB free of 16 GB (process 22 GB private); two others ended
  on out-of-memory errors from the host (a list or a compare that could not
  be recorded), one of them with the builder on. Memory, not a shader, on
  this machine.
- Later launches stopped at the title's start screen until a key was
  pressed (earlier ones had someone at the keyboard): unattended runs need
  `VICTORY_KEYS` (diagnostics builds), e.g. Enter every 15-30 s from 45 s.
- When the keys let the opening cinematic play to its end instead of
  skipping it, the title faulted in its own code (a write to address 0) at
  the cinematic's lead-out, 4-8 minutes in, twice in two such runs with two
  different settings; runs whose keys skipped it reached the world. Not
  looked into yet.

## Memory on a 16 GB machine (with upstream's frame-rate work)

The same machine, later, with this branch on top of upstream's frame-rate
work (card copies of title buffers at 1 MiB and up, draw windows, the relay,
copy queues; all their defaults). Runs crashed with E_OUTOFMEMORY while card
copies were brought up to date, once with an access violation in the NVIDIA
driver. Measured in 10-minute world runs of a diagnostics build, the same
key script, one game at a time, with a sampler of the process's private
bytes, the system's commit and the card's memory every 5 s:

| | This branch | The branch without upstream's frame-rate work |
|---|---|---|
| private bytes in the world | 20-22 GB | 20-21 GB |
| commit left in the world (limit 40.7 GB) | 0.4-2 GB | 0.7-1.7 GB |
| windows outside buffers (host copies) | 3.2 GB | 3.2 GB |
| host textures | 5.3 GB | 5.5 GB |
| card copies of title buffers, at the end | 59, 0.6 GB | 23, 0.4 GB |
| the card | 6.4-6.6 GB of a 5.2 GB budget, 4.6-6.7 GB of shared memory | the same |
| world frames (1,000+ draws), mean | 0.57 fps; 1,058 ms preparing, 514 ms GPU, 1,201 ms copying back 65 MB | 0.76 fps; 722, 505, 973 ms for 70 MB |

So upstream's work adds about 0.5 GB here and the older branch is at the
same edge: from the world's first frame both have 1-2 GB of commit left.
Whether a run survives depends on what else holds commit (about 13 GB with
nothing running; a shader translate or validate beside the game takes
0.5-4 GB more) and on the page file growing in time (here 24 GB, at most
32 GB). Where the process's commit goes (the layer's memory map line): the
title's heaps 4.6 GB and its page pool 6.2 GB (a section, outside private
bytes); buffers shared with the card about 7.5 GB; the windows 2-3 GB; other
allocations 5.8 GB, among them four driver regions of 1 GB (2.7 GB
committed).

The copying is slow because the memory is not there: the process's working
set is 0.5-3 GB of 22 GB and the PC has under 1 GB free, so 65-77 MB copied
back a frame take 0.7-1.2 s (60-100 MB/s). The GPU's time per frame is the
same in all three runs (about 505 ms). On this machine memory is the frame
rate too.

What was looked at and what it gave:

- `VICTORY_TEXTURE_SPILL_COMMIT_MB` never fires in the world: the card is
  past its share and the shared memory holds more than half of its budget
  from the first frames, so the spill's own rule refuses first. The spill
  does not use the commit up.
- The thousands of drops a frame on the frame line are the ray-tracing
  builder's stages (off by default: 610-2,570 a frame), not draws; the
  rest is about one dispatch a frame for each of a few refused compute
  pipelines. The pipeline workers' pause at low commit cost 2-3 a frame.
- `VICTORY_WINDOW_KB=1024 VICTORY_WINDOW_TAILS=1` (only 2.0-2.1 GB of the
  windows' 3.2 GB was ever copied) and `VICTORY_CARD_BUFFERS_MB=512`: not
  measured in the world. Their runs played the opening cinematic and met
  the title's fault above, or were still in it after 10 minutes.
- Not tried: `VICTORY_INPLACE=1` would drop the windows' copies but locks
  the title's pages into RAM (up to 4 GB), which this machine does not
  have free.

What keeps a run alive here, by what was measured: a larger page file (the
commit limit is the edge; the page file's peak use since boot was 7.9 GB, so
the room costs disk, not RAM), and no translate, validate or build beside a
game run. What would make it faster is less memory touched per frame, the
windows first.

## XCL

[XCL](https://github.com/xcl-dev/xcl) runs the same kind of title another way:
every console lane runs every instruction (EXEC only masks writes), the
title's graphics memory sits in up to four committed windows that D3D12 opens
as heaps (shaders reach it by address), and translated programs go through
HLSL and DXC. It is GPL-2.0-only and this project GPL-3.0-or-later: ideas
only, written anew. Its notes on the same title (menus reached, not the
world) gave these, now in this branch:

- Device methods 57 and 67 are `Deserialize{Graphics,Compute}PipelineStateX`:
  the console driver's pipeline cache. The title makes the pipeline from its
  description when they fail, so failing is right; the layer's log called
  every call a lost pipeline (tens of thousands a run). Now one line.
- Compute is waited for, graphics is not: a dispatch whose pipeline is still
  being made after the frame's wait now waits for it instead of being
  dropped (`VICTORY_COMPUTE_WAIT=0` drops it as before). A dropped dispatch
  leaves what it computes unwritten for later passes and frames.
- Pipeline workers wait while less than 2 GB of the system's commit is left
  (`VICTORY_PIPELINE_COMMIT_MB`); XCL found the title dying in the driver's
  compiler under memory pressure and makes its largest pipelines one at a
  time with 3 GB free. A pipeline a title thread waits for never waits.

Considered and not taken:

- XCL's shared memory windows would end the copies of title buffers, but not
  the texture copies (XCL converts textures too), and they commit their
  windows whole, which this 16 GB machine cannot afford. This fork's own
  first stage of the same idea is `VICTORY_INPLACE`.
- XCL's split of 64-lane programs into two 32-lane halves that meet through
  shared-memory counters works because its control flow is the same in both
  halves; this fork's per-lane branches are faster but let the halves part,
  which is why the five programs above still need 64-lane waves. Porting it
  means a second way of lowering such programs.

## Switches added on this branch

| Switch | Default | What it does |
|---|---|---|
| `VICTORY_TEXTURE_SPILL` | on | textures picked by index may go on into shared memory |
| `VICTORY_COMPUTE_WAIT` | on | a dispatch waits for its pipeline |
| `VICTORY_PIPELINE_COMMIT_MB` | 2048 | pipeline workers wait below this much free commit |
| `VICTORY_SHADER_CACHE_FAILURES` | on | a translation that fails is kept in the shader cache under its key, so later runs and `--warm` do not translate it again |
| `VICTORY_BVH_STAGES` | unset | diagnostics: only the first N ray-tracing builder programs run, in the order of their first dispatch (each logged), to find a bad stage by halving |

## Next

1. GPU feedback for textures picked by index (which slots the frame samples).
2. Find the ray-tracing builder stage that stores outside the structures'
   buffers (above): halve with `VICTORY_BVH_STAGES` and
   `VICTORY_BVH_QUARANTINE=1`, watching the log's count of pages that are
   not the title's memory. Then the dynamic structures end to end.
3. While the builder is off, skip the six skinning programs that only feed
   it, once something proves nothing else reads their buffer (about 20% of
   the world frame in one A/B pair).
4. The driver's access violation at the state prelude's barrier (above).
5. Signature rows of mixed interpolation modes (about 70 programs) and mesh outputs over 32 KB (71).
6. Compose the Series S UI plane over the 3D plane.
