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

Run `fps-overlay.exe`. A `config.ini` is written next to the exe on first start;
edit it and restart the app. Keys are read once at startup.

## Hotkeys

Active **only in game**, polled (never registered), so the game still receives
the key.

| Action | Default key |
|---|---|
| Show/hide the overlay | `Insert` (45) |
| Exit | `End` (35) |

A tray icon ("FPS Overlay") is always present: click or right-click it →
**Exit FPS Overlay** to quit from anywhere.

## Layouts

- `layout=steam` (default): one line `FPS 180 | CPU 15% | GPU 25% 54°C | 2505 MHz | VRAM 25% 2.0/8G | RAM 80% 12.8/16G`
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
| `show_fps`, `show_cpu`, `show_gpu`, `show_gpu_clock`, `show_vram`, `show_ram`, `show_process` | `1` | `0`/`1` |
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

Single static-CRT exe, one dependency (`windows`). `pdh.dll`, `dxgi.dll` and
`version.dll` are delay-loaded, so the FPS-only layout never maps them.

## Footprint

Release build measured on this machine:

- `fps-overlay.exe`: 324,096 bytes (316.5 KiB), no DLLs to ship.
- Import table: system DLLs only (`kernel32`, `user32`, `gdi32`, `shell32`,
  `advapi32`, `oleaut32`, `ntdll`).
