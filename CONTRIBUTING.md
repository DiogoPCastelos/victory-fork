# Contributing

The most useful thing you can do is **run it and open an issue** with what happened. The
layer behaves very differently depending on the machine (above all GPU memory and RAM), so
every report needs the same four things:

1. **Your full specs.**
2. **`run.log`**, uploaded.
3. **Screenshots in game,** of the picture and of the render layers.
4. **What you did and what you saw.**

Open one issue per run. Use the **Run report** template; it asks for all of this.

**Never upload the game's files** or anything copied out of them (archives, executables,
shader dumps). `run.log` and screenshots are fine.

## 1. Your full specs

All of these, please. Most of the problems are memory-related, so the numbers matter.

- **GPU:** model, dedicated memory (GB), driver version.
- **CPU:** model.
- **RAM:** total (GB). **Page file:** size, or "system managed".
- **Windows:** version and build (`winver`).
- **The fork's commit** you built (`git rev-parse --short HEAD`), and whether it was the
  release or the diagnostics build.
- **Launch options:** any `VICTORY_*` variables you set, and the exact command line.
- **Cache:** whether you warmed it before running (README, step 2), and with which values.

The GPU and memory numbers are in Task Manager → Performance (GPU: "Dedicated GPU memory";
Memory: total and "Committed").

## 2. `run.log`

Run with the output saved to a file:

```powershell
target\release\victory.exe run "<game folder>\<the game's .exe>" --execute > run.log 2>&1
```

Play until the problem shows (or the world loads), close the window, and upload `run.log`
to the issue. Zip it if it is large. If it crashed, also upload `crashes.log` from
`%LOCALAPPDATA%\victory\data`.

## 3. Screenshots: the picture and the render layers

A normal screenshot of the problem (Win + Shift + S), plus **the render layers** of the
same frames. The game draws each frame in layers before lighting them: base colour,
normals, material, depth and so on. They show which stage fails. In one example, faces
were black in the base colour layer but whole in the normals: that pointed at missing
textures, not missing geometry or broken shaders.

To save the layers, build and run the diagnostics build with capture on:

```powershell
cargo build --profile diag -p victory --features diagnostics
# its shader cache is separate: warm it with this binary (README, step 2, with target\diag)
$env:VICTORY_CAPTURE = "C:\victory-captures"        # any folder OUTSIDE the repo and the game
$env:VICTORY_CAPTURE_WORLD = 1                      # a few frames once the world shows
$env:VICTORY_CAPTURE_WORLD_ALSO = "60,150,300"      # and these many frames into the world
$env:VICTORY_DUMP_TARGETS = 1                       # every render layer of those frames
target\diag\victory.exe run "<game folder>\<the game's .exe>" --execute > run.log 2>&1
```

In the capture folder:
- `frame-<n>.png` is the picture;
- `frame-<n>-<index>-<address>-<width>x<height>-f<format>.png` are its layers.

Upload, for one or two frames in the world:
- the picture `frame-<n>.png`;
- every layer at the game's internal size (the largest ones, e.g. `2560x1440`). At least
  the base colour (`f27`) and normals (`f28`) layers.

Say which frame number they are.

Captures are slow (each saved frame is read back from the GPU), so turn them off for
normal runs:

```powershell
Remove-Item Env:VICTORY_CAPTURE, Env:VICTORY_CAPTURE_WORLD, Env:VICTORY_CAPTURE_WORLD_ALSO, Env:VICTORY_DUMP_TARGETS
```

## 4. What you did and what you saw

- How far it got (menus, cutscene, world) and how long it ran.
- What looks wrong (black faces, missing walls, flicker, wrong colours...), and where.
- The frame rate shown in the window title.
- Whether it crashed or froze, and when.

## Code contributions

Pull requests are welcome. Please:

- read [docs/CLAUDE.md](docs/CLAUDE.md) (the rules for the shader translation work) and
  [docs/HANDOVER.md](docs/HANDOVER.md) (where things stand) first;
- run `cargo test -p victory-shader --lib` and `cargo test -p victory-gpu --lib`, and say
  which GPU the GPU tests ran on (they print "NOT RUN" without one);
- report only what you ran and measured;
- never put game files, game-derived data, shader names or screenshots in the repository
  (`docs/asset-policy.md`; `cargo xtask install-hooks` installs a check).
