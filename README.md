# FPS Overlay

Steam-like FPS/CPU/GPU/RAM bar, plus a minimal FPS-only counter, drawn over the
game you are playing. The overlay appears only while a game is the foreground
window and gets its FPS from ETW present events — no injection, no hooks.

## Requirements

- Windows 10 1809+ x64.
- **Administrator**: the kernel ETW provider needs it. The exe relaunches itself
  through UAC when it is not elevated; declining the prompt exits silently.
- **Borderless or windowed games.** Exclusive fullscreen is not supported (same
  as the original).
- Stock `nvml.dll` (NVIDIA driver) is used, when present, for GPU temperature and
  clock. Everything else is native Windows APIs.

Run `fps-overlay.exe`. A `config.ini` is written next to the exe on first start.
Change settings from the tray (applied immediately), or edit the file and restart.

## Hotkeys

Active **only in game**, polled (never registered), so the game still receives
the key.

| Action | Default key |
|---|---|
| Show/hide the overlay | `Insert` (45) |
| Exit | `End` (35) |

A tray icon ("FPS Overlay") is always present:

- **Double-click** it to open **Settings**: every `config.ini` option in one
  window. **Save** validates, writes `config.ini` and applies immediately (no
  restart); **Cancel** discards.
- **Right-click** it for **Settings…**, **Start with Windows** (checkmark toggle;
  also a checkbox in Settings) and **Exit FPS Overlay**. Start with Windows
  creates a Task Scheduler logon task named `FPS Overlay` that runs the exe at
  highest privileges, so there's no UAC prompt at login. Untick it to delete the
  task. If you move the exe, untick and tick again.
- **Ignoring an app** the overlay shouldn't show for: after it appears, right-click
  the tray → **Don't show for `name.exe`** (saved and applied at once). Or in
  Settings, **Add…** next to the ignore list picks from running apps with a window.

## Layouts

- `layout=steam` (default): one line `FPS 180 | CPU 15% | 5545 MHz | GPU 25% 54°C | 2505 MHz | VRAM 25% 2.0/8G | RAM 80% 12.8/16G`
  plus a process line `RocketLeague.exe (Rocket League)`.
- `layout=fps-only`: a single FPS number with a drop shadow, no background by
  default. CPU/GPU/RAM are never polled in this layout (PDH, DXGI and NVML are
  never loaded).

Corner and scaling follow `position` and `scale` on the monitor that holds the
game window.

## config.ini (`[Overlay]`)

| Key | Default | Range |
|---|---|---|
| `layout` | `steam` | `steam`, `fps-only` |
| `fps_only_background` | `0` | `0`/`1` (fills a black background behind the FPS-only number) |
| `position` | `top-left` | `top-left`, `top-right`, `bottom-left`, `bottom-right` |
| `scale` | `100` | 50–200 (%) |
| `background_opacity` | `100` | 0–100 (%) |
| `show_fps`, `show_cpu`, `show_cpu_clock`, `show_gpu`, `show_gpu_clock`, `show_vram`, `show_ram`, `show_process` | `1` | `0`/`1` |
| `fahrenheit` | `0` | `0`/`1` |
| `min_fps` | `20` | 1–1000; a foreground app below this is not treated as a game |
| `ignore` | `explorer.exe,dwm.exe,chrome.exe,msedge.exe,firefox.exe,discord.exe` | comma-separated exe names |
| `toggle_key` | `45` (Insert) | 1–254 (virtual-key code) |
| `exit_key` | `35` (End) | 1–254 (virtual-key code) |

Missing file → defaults are written. An out-of-range or unparsable value falls
back to the default for that key.

## Build

```
cargo build --release
```

Single static-CRT exe, one dependency (`windows`). `pdh.dll`, `dxgi.dll`,
`version.dll` and `dwmapi.dll` are delay-loaded, so the FPS-only layout never
maps them.

## CI and releases

- `.github/workflows/ci.yml`: every push to `main` and every PR runs
  `cargo fmt --check`, clippy `-D warnings`, tests and a release build; the exe
  is uploaded as a workflow artifact.
- `.github/workflows/release.yml`: pushing a semver tag `vMAJOR.MINOR.PATCH`
  (optionally `-pre`, published as a pre-release) builds and publishes a GitHub
  release with the exe and its SHA-256. The tag must match `version` in
  `Cargo.toml`.

```
# bump version in Cargo.toml first, commit, then:
git tag v0.2.0 && git push origin v0.2.0
```

## Footprint

Release build measured on this machine:

- `fps-overlay.exe`: ~347 KB, no DLLs to ship.
- Import table: system DLLs only (`kernel32`, `user32`, `gdi32`, `shell32`,
  `advapi32`, `oleaut32`, `ntdll`).
