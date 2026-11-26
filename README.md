# VIctory: dual-bank fork

A fork of **[hyphennn's VIctory fork](https://github.com/fxceless/victory-fork)**
(`fxceless/victory-fork`), which is itself a fork of
**[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak: a compatibility layer
that runs an Xbox Series title natively on Windows (Direct3D 12) from files you supply.

**No game files, code or assets are in this repository.** You need your own copy of the
game. Not affiliated with or endorsed by the game's developer or publisher.

## The approach

The console's GPU runs most shaders in waves of 64 lanes; NVIDIA GPUs only run waves of 32.
The usual way across is to split each 64-lane wave into two 32-lane halves and make them
talk through shared memory with barriers, or approximate.

This fork adds a **dual-bank** path instead: the console's 64-lane program is rewritten so
that **each GPU thread carries two console lanes** (lane p and lane p + 32). A whole console
wave then fits in one 32-lane NVIDIA warp. Nothing is split, nothing has to wait for the
other half, and nothing is approximated. The console's EXEC mask and control flow are kept
as they are, and every vector instruction runs once per bank.

- Every lane operation is checked against a CPU model of the console's 64-lane wave, on the
  GPU: lane 31↔32, all 4,096 shuffle pairs, ballots, prefix counts, branches, loops.
- On the title's shaders it covers 45 of the 65 compute programs that had no exact
  translation on a 32-lane GPU.
- It is 1.6–6× faster than the split-in-halves path where that path needs barriers.

Found a problem or ran it on your machine? See [CONTRIBUTING.md](CONTRIBUTING.md): open an issue with your full specs, `run.log` and screenshots.

Details, numbers and what is still open: [docs/wave-audit.md](docs/wave-audit.md) and
[docs/HANDOVER.md](docs/HANDOVER.md).

**Where it stands:** reaches the world and runs at about 1–2 fps on 16 GB RAM with a 6 GB
NVIDIA GPU. Skin, walls and furniture come out black there because the GPU runs out of
memory and their textures are held back. More memory (32 GB RAM, 16 GB+ GPU) should show
them; that is what is being tested next.

## How to run it

You need:
- Windows 10/11 x64, a CPU with AVX2;
- a Direct3D 12 GPU with shader model 6.6 and mesh shaders;
- [Rust](https://rustup.rs) (the version is pinned in `rust-toolchain.toml`);
- your own copy of the game, outside this repository;
- plenty of memory: several GB of free Windows commit (see Task Manager → Performance →
  Memory → Committed). If it crashes with Windows error 1455, close other programs or make
  the page file larger.

**1. Build**

```powershell
cargo build --release -p victory
```

**2. Warm the shader cache** (optional, strongly recommended: about 5 minutes, once per
build; otherwise the first launch spends a long time translating ~26,000 shaders). The two
values must match the GPU you will run on. For any NVIDIA GPU:

```powershell
$env:VICTORY_HOST_WAVE = 32
$env:VICTORY_VIEW_LIMIT = 999999
target\release\victory.exe translate "<game folder>" --warm all
Remove-Item Env:VICTORY_HOST_WAVE, Env:VICTORY_VIEW_LIMIT
```

**3. Run**

```powershell
target\release\victory.exe run "<game folder>\<the game's .exe>" --execute > run.log 2>&1
```

Press Enter through the menus. The first launch also builds the GPU's pipelines (minutes in
the menus); later launches reuse them. The window title shows the frame rate.

- The game's files are only read, never written.
- The layer's cache is in `%LOCALAPPDATA%\victory\cache` (safe to delete).
- Saves, settings and `crashes.log` are in `%LOCALAPPDATA%\victory\data`.

**Controls:** an XInput pad, or the keyboard while the game's window has focus:

| Keys | Pad |
|------|-----|
| W A S D / I J K L | left / right stick |
| arrows | D-pad |
| Enter, Backspace, X, Y | A, B, X, Y |
| Q / E, Z / C | shoulders, triggers |
| F / H | stick clicks |
| Esc, Tab | Menu, View |

**If something goes wrong,** look in `run.log` for:
- `fps,`: frame rate and draws per frame;
- `textures picked by index, the queue`: textures waiting;
- `video memory as the system counts it`: GPU memory;
- `ERROR`.

Captures of frames and render targets need the diagnostics build
(`cargo build --profile diag -p victory --features diagnostics`; see
[docs/HANDOVER.md](docs/HANDOVER.md)).

## Reporting a run

Ran it? Please open an issue, whether it worked or not, following
**[CONTRIBUTING.md](CONTRIBUTING.md)** (the issue form asks for the same):
- your full specs: GPU and its dedicated memory, driver, CPU, RAM, page file, Windows build;
- your `run.log`, uploaded;
- screenshots in game: the picture and its render layers (base colour, normals, and the
  rest), captured with the diagnostics build as CONTRIBUTING.md shows.

## Credits

- **[VIctory](https://github.com/Marvisak/VIctory)** by Marvisak and its contributors: the
  base of everything (loader, runtime services, Direct3D 12 back end, shader recompiler).
- **[hyphennn's fork](https://github.com/fxceless/victory-fork)** (`fxceless/victory-fork`):
  the fork this one builds on, with its frame-rate and rendering work, and the changes it
  carries from others:
  - **myokex** ([victory-amd](https://github.com/myokex/victory-amd)): the memory-page
    guard, adapting VIctory pull request #115 by Spexivus, its correction by joroyt, and
    part of VIctory #1122 by tightfall;
  - **Joshua Odom**: the read-beside fix.

  Its README has the full history.
- **[XCL](https://github.com/ivall/xcl)** (GPL-2.0-only) was used as a reference only;
  nothing was copied.
- This fork's changes were developed with [Claude Code](https://claude.com/claude-code).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
