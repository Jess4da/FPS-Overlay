# FPS Overlay (Rust) — Steam-like + FPS-Only — Design

## Intent
New Rust app (free redesign; the C++ code is a reference, not a structure to copy). Only requirement on looks: the bar output must match the original's **Steam-like** layout (`src/main.cpp:4270-4483`). Adds a second layout, **FPS-Only** (a lone FPS number in a corner, like NVIDIA's FPS overlay). Unlike the original (always topmost, global hotkeys), the overlay exists **only while a game is the foreground window**, and its hotkeys work **only in-game**. **Second main goal: as lightweight as possible** — tiny single exe, near-zero cost when no game is running, minimal cost while gaming (the overlay must never be the thing that costs the game frames).

Reference look (`screenshots/steamlike_layout.png`):
```
FPS 180 | CPU 15% | 5545 MHz | GPU 25% 54°C | 2505 MHz | VRAM 25% 2.0/8G | RAM 80% 12.8/16G
RocketLeague.exe (Rocket League)
```

## Goals
- FPS of the foreground process via ETW present events (no injection, anti-cheat safe), same providers and counting rules as the original.
- Metrics from native Windows APIs only — no driver, no .NET, no LibreHardwareMonitor/PawnIO.
- Auto-show when a game is foreground; auto-hide otherwise.
- Toggle/exit hotkeys that act only while a game is foreground and never swallow the key from the game.
- Process label line `game.exe (File Description)` (Steam-like only).
- Two layouts, picked by `layout` in config: `steam` (default) and `fps-only`. In `fps-only`, CPU/GPU/RAM metrics are never polled and PDH/NVML/DXGI are never loaded.
- `config.ini` next to the exe.
- Lightweight budgets (verified in final smoke, release build):
  - Single self-contained `.exe` (static CRT, no DLLs shipped), **≤ 500 KB**.
  - Idle (no game foreground): **0 timers, 0 metric polling, 0 redraws**; CPU ≈ 0% in Task Manager; private bytes **≤ 5 MB**.
  - In game: CPU **< 0.5%** of one core averaged; private bytes **≤ 10 MB** (NVML may add its own; measured separately).
  - Redraw at most once per second (plus on show/move); no per-frame rendering, no swapchain, no GPU device.

## Non-goals
Vertical/horizontal layouts, settings UI, tray icon, i18n/RTL, update check, sparklines, CTRL+drag, CPU temperature, CPU clock, power/fan readings, multi-GPU selection, exclusive-fullscreen support (borderless/windowed only, same as original).

## Anti-goals
- Overlay visible over desktop, browser, or other non-game apps.
- Overlay stealing focus or mouse input from the game.
- Hotkeys registered system-wide (`RegisterHotKey`) or consumed before the game sees them.

## Constraints
- Windows 10 1809+ x64. Rust stable (1.99). **Single dependency: `windows` crate** (zero-cost bindings, minimal feature set). No ETW wrapper crate, no GUI crate, no serde, no logging crate.
- Release profile: `opt-level = "s"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`; `.cargo/config.toml` sets `-C target-feature=+crt-static` for `x86_64-pc-windows-msvc`.
- ETW kernel/system providers need admin: the exe relaunches itself elevated (`ShellExecuteW` verb `runas`) when not elevated; no manifest/build script.
- Per-monitor DPI aware v2 via `SetProcessDpiAwarenessContext` at startup.
- Heavy resources are lazy: PDH query, NVML are created on first game qualification and kept; DXGI factory is used once for adapter info then released.
- **ETW providers are enabled only while a candidate is foreground.** Not candidate → `EnableTraceEx2(..., EVENT_CONTROL_CODE_DISABLE_PROVIDER)` for all providers (session stays open but receives nothing). Candidate → enable DXGI + D3D9; enable DxgKrnl **only as fallback** if no DXGI/D3D9 event for that pid within 1.5 s (Vulkan/OpenGL games). DxgKrnl is the high-volume provider; this keeps it off for DX9–12 games and off entirely when idle.
- Process runs at `BELOW_NORMAL_PRIORITY_CLASS`; ETW callback thread stays at normal priority so buffers don't back up.
- On becoming idle (game left foreground): free per-game scratch and call `SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX)` to trim the working set.
- No heap allocation in the ETW callback (atomics + fixed counters only).
- *(ponytail: PDH `GPU Engine(*)` enumerates per-process instances and is the costliest 1 Hz call; measured in smoke. If it exceeds budget, replace with `D3DKMTQueryStatistics` per-adapter node running time.)*

## Decided (user)
- In-game = both auto-show/hide and in-game-only hotkeys.
- Sensors = native, no driver.
- Extras = process-name line, `config.ini`. (Sparklines and CTRL+drag dropped.)

## Data sources
| Field | Source |
|---|---|
| FPS | ETW realtime session `FPSOverlayRs_ETW`, raw `StartTraceW` / `EnableTraceEx2` / `OpenTraceW` / `ProcessTrace` (`EVENT_RECORD_CALLBACK`, `PROCESS_TRACE_MODE_REAL_TIME \| PROCESS_TRACE_MODE_EVENT_RECORD`), small buffers (`BufferSize` 64 KB, `MinimumBuffers` 4, `FlushTimer` 1 s). Callback reads only `EventHeader.ProcessId`, `EventDescriptor.Id`, `ProviderId` — no TDH decoding. Providers: DXGI `{CA11C036-0102-4A2D-A6AD-F03CFED5D3C9}` event 42; D3D9 `{783ACA0A-790E-4D7F-8451-AA850511C6B9}` event 1; DxgKrnl `{802EC45A-1E99-4B83-9920-87C98277BA9D}` keywords `0x8000000 \| 0x1`, events `0xB8, 0xA8, 0xA6`. DXGI and D3D9 enabled with an `EVENT_FILTER_TYPE_EVENT_ID` filter so only the listed IDs are delivered. Count only events whose PID == target PID. Per 1 s window: if d3d9>0 use d3d9, else if dxgi>0 use dxgi, else use kernel only if kernel/elapsed ≥ 20. *(ponytail: DxgKrnl is kernel-mode, can't be PID-filtered at source; callback discard is the ceiling.)* |
| CPU % | `GetSystemTimes` delta: `(1 - idle/(kernel+user)) * 100` |
| RAM | `GlobalMemoryStatusEx` → used/total GiB |
| GPU % | PDH `\GPU Engine(*engtype_3D)\Utilization Percentage`, summed across instances **whose name contains the chosen adapter's LUID** (`luid_0x{HighPart:08X}_0x{LowPart:08X}`), clamped 0–100 |
| VRAM used | PDH `\GPU Adapter Memory(*)\Dedicated Usage` instance matching the chosen adapter's LUID (bytes) |
| VRAM total | Chosen adapter = DXGI `IDXGIFactory1::EnumAdapters1` entry with largest `DedicatedVideoMemory`, skipping `DXGI_ADAPTER_FLAG_SOFTWARE`; total = its `DedicatedVideoMemory`. All GPU fields describe this one adapter. |
| GPU temp / core MHz | Only when chosen adapter `VendorId == 0x10DE`: NVML via `LoadLibraryW("nvml.dll")` + `GetProcAddress` (`nvmlInit_v2`, `nvmlDeviceGetHandleByIndex_v2(0)`, `nvmlDeviceGetTemperature` sensor 0, `nvmlDeviceGetClockInfo` clock 0 = graphics). Otherwise / on failure → `None`, fields omitted. *(Builder's call: NVML index 0 assumed to be the chosen adapter; multi-NVIDIA rigs may mismatch.)* |
| CPU MHz | PDH `\Processor Information(_Total)\Processor Frequency` × `% Processor Performance` / 100 (live boost clock, as Task Manager), own query opened on first poll. Failure → `None`, group omitted. |
| Process label | `QueryFullProcessImageNameW` + `GetFileVersionInfoW`/`VerQueryValueW` `FileDescription` → `"name.exe (Description)"`, or `"name.exe"` when no description |

## Game gate *(builder's call — resolves FPS dropouts and exit-key reachability)*
Driven by `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, …, WINEVENT_OUTOFCONTEXT)` — no polling while idle. On foreground change: `pid = GetWindowThreadProcessId(hwnd)`; set ETW target PID (skip own PID). If the pid is a **candidate**, start one `SetTimer` at 250 ms ("probe"); stop it when the pid changes or stops being a candidate. Once **is game**, the timer runs at 100 ms (hotkeys + 1 Hz stats/redraw). Not candidate → kill timer, hide overlay.
- **Candidate** ⇔ pid ≠ own pid ∧ exe name (case-insensitive) not in `ignore`.
- **Is game** ⇔ candidate ∧ this pid has produced at least one FPS sample ≥ `min_fps` since it became foreground (**sticky** for that foreground period; cleared on PID change). Loading screens / pauses therefore keep the overlay up.
- FPS sample older than 2 s displays as `---` (no frozen values).
- Overlay visible ⇔ is game ∧ `user_visible` (toggled by hotkey, default true). On PID change: FPS → `None`, sticky flag cleared, overlay hides until qualified again.
- UWP/`ApplicationFrameHost.exe` windows: out of scope (Win32 games only, same as original).

## Startup failure *(builder's call)*
Not elevated → relaunch elevated and exit; UAC declined → exit silently. ETW session or DXGI provider fails to start → `MessageBoxW` with the Win32 error code, then exit (the overlay can't work without FPS). D3D9/DxgKrnl provider failure is non-fatal.

## Hotkeys
Polled with `GetAsyncKeyState` on the in-game 100 ms timer (edge-triggered: act on up→down transition), **only while is game** — so no keyboard polling when idle. Non-consuming by construction. `toggle_key` flips `user_visible`; `exit_key` quits the process (cleanly stops the ETW session). The process also exits on `WM_ENDSESSION` / Ctrl+C.

## Window & rendering
- `WS_POPUP`, ex-style `WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW`. Sized to content, not the whole screen.
- **GDI only** (no Direct2D/DirectWrite/D3D — avoids loading d2d1/dwrite/d3d11 and their working set). One 32-bit top-down DIB section in a memory DC, sized to content; `CreateFontW("Segoe UI", ANTIALIASED_QUALITY)` cached per size; `GetTextExtentPoint32W` for measuring; `ExtTextOutW` for drawing.
- Alpha: draw the bar twice into two DIBs — (a) colored runs on black, (b) the same runs in white on black = coverage mask. Per pixel: `cov = mask.r/255`, `a = cov + bg_a·(1−cov)`, premultiplied rgb = colored pixel rgb (black bg contributes nothing). Push with `UpdateLayeredWindow` (`ULW_ALPHA`, `AC_SRC_ALPHA`). *(ponytail: grayscale AA, not ClearType/DirectWrite quality; upgrade to D2D if text looks rough.)*
- Redraw only when the built line text/colors change (≤1 Hz) or visibility/monitor changes; skip `UpdateLayeredWindow` otherwise.
- Placement: corner of the **monitor containing the foreground window** (`MonitorFromWindow` → `rcMonitor`), margin 0, per `position`.

## Layout: Steam-like (exact values from original)
- Font Segoe UI, 17 px × scale × DPI/96. Padding (10, 6) × scale. Label→value gap 3 px × scale; between parts within a group 4 px × scale. Separator text `" | "`.
- Background black, alpha = `background_opacity`/100.
- Colors (RGB 0–1):
  - label FPS (0.92,0.52,0.58); label CPU (0.94,0.88,0.58); label GPU (0.52,0.90,0.70); separator (0.55,0.55,0.58); no-data `---` (0.50,0.50,0.55).
  - FPS value: ≥60 (0.18,0.94,0.45); ≥30 (1,0.85,0.15); else (1,0.25,0.25).
  - load (CPU%, GPU%, whole VRAM/RAM text): >90 (1,0.3,0.3); >70 (1,0.85,0.15); else (0.70,0.70,0.75).
  - temp (°C): >85 (1,0.3,0.3); >70 (1,0.85,0.15); else (0.70,0.70,0.75). Thresholds always in °C even when displayed in °F.
  - MHz values (CPU and GPU) (0.70,0.70,0.75). Process line (0.42,0.52,0.42) at 0.78 × font size.
- Groups in order, each enabled by config, separated by `" | "` only between drawn groups:
  1. `FPS` `180` (`%.0f`; `---` when `None`)
  2. `CPU` `15%`
  3. `5545 MHz` CPU clock (own group, only when Some)
  4. `GPU` `25%` ` 54°C` (temp omitted when `None`; `N/A` when GPU% `None`)
  5. `2505 MHz` GPU clock (own group, only when Some)
  6. `VRAM 25% 2.0/8G` (`VRAM %.0f%% %.1f/%.0fG`; omitted when either value `None`)
  7. `RAM 80% 12.8/16G` (`RAM %.0f%% %.1f/%.0fG`)
- Fahrenheit: `°F`, value `c*9/5+32`.

## config.ini (`[Overlay]`, read once at start, parsed by hand from the file bytes)
| Key | Default | Range |
|---|---|---|
| layout | steam | steam, fps-only |
| fps_only_background | 0 | 0/1 (fps-only only: draw black background at `background_opacity`) |
| position | top-left | top-left, top-right, bottom-left, bottom-right |
| scale | 100 | 50–200 (%) |
| background_opacity | 100 | 0–100 |
| show_fps, show_cpu, show_cpu_clock, show_gpu, show_gpu_clock, show_vram, show_ram, show_process | 1 | 0/1 |
| fahrenheit | 0 | 0/1 |
| min_fps | 20 | 1–1000 |
| ignore | explorer.exe,dwm.exe,chrome.exe,msedge.exe,firefox.exe,discord.exe | comma list |
| toggle_key | 45 (VK_INSERT) | 1–254 |
| exit_key | 35 (VK_END) | 1–254 |

Missing file → defaults (and write a default file). Out-of-range or unparsable value → default for that key.

## Layout: FPS-Only *(builder's call on look, modeled on NVIDIA's FPS counter)*
- One run: the FPS number only (`%.0f`, `---` when `None`). No label, no process line.
- Font Segoe UI **semibold** (weight 600), 22 px × scale × DPI/96. Padding (4, 2) × scale.
- Color: same FPS tiers as Steam-like (≥60 green, ≥30 yellow, else red; `---` gray).
- Always a 1 px black drop shadow at (+1,+1) so it reads on any scene. Background rect only when `fps_only_background=1` (black, alpha `background_opacity`).
- Same corner placement, game gate, hotkeys as Steam-like.

## Left to the builder
Module internals, error-message wording, GDI object lifetime, PDH query handle management, how ETW stale-session (`ERROR_ALREADY_EXISTS`) is cleared (stop then restart).
