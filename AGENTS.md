# Repository Guidelines

## Project Overview

Windows-only, in-game FPS overlay written in Rust. It shows a Steam-like stats bar (`FPS | CPU | GPU | MHz | VRAM | RAM`, plus a process line) or a lone NVIDIA-style FPS counter, **only while a game is the foreground window**. FPS comes from ETW present events, with no injection and no hooks, so it is anti-cheat safe. It is a port of the `steam` layout from a C++/ImGui app (`D:\Coding\OtherCode\fps-overlay`), redesigned from scratch.

The second main goal is to be **as lightweight as possible**. Budgets (spec "Goals"):
- exe ≤ 500 KB
- idle: 0 timers, 0 polling, ≤ 5 MB private bytes
- in game: < 0.5% of one core, ≤ 10 MB (NVML excluded)
- at most one redraw per second

Treat these as hard constraints when you change anything.

## Architecture & Data Flow

There are exactly **two threads**:
- **UI thread:** owns everything, including the message loop, `App`, the renderer, metrics and the settings window.
- **ETW consumer thread:** `src/etw.rs` runs `ProcessTrace`. Its callback writes **atomics only**: no allocation, no locks.

```
WinEvent EVENT_SYSTEM_FOREGROUND ─► App::foreground ─► GameGate::on_foreground ─► App::apply
WM_TIMER ─► App::tick ─► read SHARED sample ─► GameGate::on_tick ─► App::apply
                       └─ if game && visible: hotkeys → (Steam only) metrics.poll 1 Hz
                                              → layout::build → needs_draw? → Overlay::draw
App::apply(GateOut): SHARED.target_pid, etw.set_providers, SetTimer/KillTimer, hide/idle trim
Tray WM_TRAY: double-click → settings::open → Save → config::save + App::reconfigure (live)
```

- **ETW → UI contract (`etw::SHARED`):** the callback stores `fps_bits` and `sample_pid` Relaxed, then `sample_qpc` **Release**, last. Readers load `sample_qpc` **Acquire** first (0 means no sample). The UI thread writes only `target_pid`. `saw_pid` is per-pid evidence that a user-mode (DXGI/D3D9) event was seen.
- **Gate (`src/gate.rs`):** pure state machine returning `GateOut { timer, providers, visible, fps }`.
  - **Candidate:** pid ≠ own and the exe is not in `ignore`.
  - **Game:** a candidate with a fresh sample ≥ `min_fps`. Qualification is sticky per foreground period.
  - **Timers:** `Probe` 250 ms → `Game` 100 ms → `SlowProbe` 1000 ms after 10 s without qualifying.
  - **Providers:** `Off`, `UserMode` (DXGI+D3D9), or `WithKernel`. DxgKrnl is added only after 1.5 s with no user-mode events (Vulkan/OpenGL fallback).
- **FPS counting (`src/fps.rs`):** 1 s window, priority D3D9 > DXGI > kernel. Kernel-only counts only if ≥ 20 fps. Windows older than 2 s are dropped.
- **Rendering (`src/render.rs`):** GDI only. Two DIBs, one with colour and one as a white-on-black coverage mask. `compose` produces premultiplied BGRA for `UpdateLayeredWindow(ULW_ALPHA)`. The window is `WS_EX_TOPMOST|LAYERED|TRANSPARENT|NOACTIVATE|TOOLWINDOW`: click-through and never focused.
- **Self-heal:** each tick compares `GetForegroundWindow()` with `fg_hwnd` and re-runs `foreground()`, because WinEvents can be missed or coalesced.

## Key Directories

| Path | Purpose |
|---|---|
| `src/` | The whole crate: flat modules, no `lib.rs`, declared in `src/main.rs:3-11` |
| `assets/icon.ico` | App/tray icon. Embedded as resource id **1** by `build.rs` |
| `docs/specs/` | Binding design: GUIDs, event IDs, colours, thresholds, config keys |
| `docs/plans/` | Historical implementation plan (done). Keep for reference; don't update it |
| `target/release/` | `fps-overlay.exe`. `config.ini` is written next to the exe at runtime |

## Development Commands

```powershell
cargo build --release                          # target\release\fps-overlay.exe
cargo test                                     # all unit tests (70)
cargo test gate::tests                         # one module (config, fps, layout, render, metrics, main, settings, process)
cargo test poll_smoke -- --nocapture           # real PDH/DXGI/NVML poll, prints Sample + timing (~1.1 s)
cargo clippy --release --all-targets -- -D warnings
cargo fmt                                      # CI runs `cargo fmt --check`; format before committing

# Release: bump `version` in Cargo.toml, commit, then push a matching semver tag
git tag v0.2.0; git push origin v0.2.0
```

- **Running the exe** triggers UAC; it relaunches itself elevated via `runas`. Declining exits silently.
- **Stuck ETW session after a crash:** `logman query -ets | findstr FPSOverlay`, then `logman stop FPSOverlayRs_ETW -ets` (both elevated). The app also clears a stale session on start.
- **Checking size and imports:** `dumpbin /dependents target\release\fps-overlay.exe` should list system DLLs only, with `pdh.dll`, `dxgi.dll`, `version.dll` and `dwmapi.dll` under *delay load*.

## Code Conventions & Common Patterns

- **Pure core, thin Win32 shell.** Put logic in pure functions with injected time and unit-test those. Examples:
  - `GameGate::on_tick(.., now_ms)` and `FpsCounter::new(qpc_freq)` / `on_present(.., now)`
  - `layout::build(&Stats, &Config) -> Frame` and `render::compose`
  - `config::parse` / `to_ini`, `settings::from_fields`
  - `needs_draw`, `edges` and `ms_between` in `main.rs`

  New Win32 behaviour should extract its decision logic the same way.
- **State:** `thread_local! APP: RefCell<Option<App>>`, accessed through `with_app(|app| ..)`.
  - **Never nest `with_app`.** A double borrow panics, and the release build aborts on panic.
  - Anything that pumps messages (MessageBox, opening a window, running a callback) must run *after* the borrow ends. Copy the data out first (see `open_settings`, `settings::save`), or defer it (`FATAL` + `PostQuitMessage`, shown after the loop).
  - `WM_ENDSESSION` uses `try_borrow_mut`.
- **Cross-thread:** other threads only `PostMessageW(MAIN_HWND, ..)`, as `ctrl_handler` does. Raw pointers crossing threads are passed as `usize`.
- **Errors:**
  - Non-critical Win32 results use `let _ = ..`.
  - Metric and process failures become `None`, never an error string and never a panic.
  - Fatal errors happen only at startup: `show_error` and then return.
  - The release profile is `panic = "abort"`, so `unwrap`/`expect` only on internal invariants.
- **Unsafe:** keep each `unsafe` block small and put a `// SAFETY:` line above it naming the invariant. Callbacks are `unsafe extern "system" fn`.
- **Lightweight invariants:** don't break these.
  - The FPS-only layout must never call `metrics.poll()` or `process::label`, so the delay-loaded `pdh`/`dxgi`/`version`/`nvml` DLLs are never mapped.
  - `Metrics::new()` stays cheap.
  - No timer while not a candidate.
  - Redraw only when `needs_draw(..)` is true (the `Frame` compares unequal).
  - On idle, call `release_buffers` and `SetProcessWorkingSetSize(-1,-1)`.
  - Hotkeys are polled with `GetAsyncKeyState`, edge-triggered, and armed only after the overlay appears. Never use `RegisterHotKey` or keyboard hooks.
- **Comments:** `//!` module headers state responsibility, and `///` explains *why*. `ponytail:` marks a deliberate simplification and names its limit and upgrade path. `// Regression:` labels a test that pins a past bug.
- **Naming:** standard Rust casing. Short Win32-style locals are fine (`wp`, `lp`, `px`, `s`). Window classes are `FpsOverlay` (the overlay), `FpsOverlayRsMain` (the hidden main window) and `FpsOverlaySettings`.
- **Win32 idioms in use:**
  - Register each window class once with `static REGISTERED: AtomicBool` and `swap`.
  - Top-down DIBs use a negative `biHeight`.
  - Re-select `dc_old` before `DeleteObject` on a cached font.
  - The mask DIB must be filled with `MASK_EMPTY = 0`, or the background alpha is counted twice.
- **Settings window (`src/settings.rs`):** raw `BS_*`/`CB_*`/`BM_*` constants are used instead of the `Controls` feature. `CHECKS` order is tied to `Fields::checks` and `ID_CHECKS + i`, so change all three together.

## Important Files

| File | Role |
|---|---|
| `src/main.rs` | Entry point: DPI and elevation, `App`, `apply`/`tick`/`reconfigure`, tray, message loop, `main_wndproc` |
| `src/etw.rs` | Raw ETW session `FPSOverlayRs_ETW`, provider enable/disable, consumer thread, `SHARED` |
| `src/fps.rs` | `FpsCounter`: per-window FPS from present events |
| `src/gate.rs` | `GameGate`: candidate/game/backoff logic, timers and providers |
| `src/layout.rs` | `Stats → Frame` (runs, colours, gaps) for both layouts. Exact values come from the spec |
| `src/render.rs` | Layered GDI overlay, `compose`, placement, DPI |
| `src/metrics.rs` | CPU (`GetSystemTimes`), RAM, GPU %/VRAM (PDH, filtered by adapter LUID), VRAM total (DXGI), temp/MHz (NVML, NVIDIA only) |
| `src/process.rs` | pid → lowercase exe name, `"Name.exe (FileDescription)"` label |
| `src/config.rs` | `Config`, `DEFAULT_INI`, hand-written `parse`/`to_ini`/`load`/`save`. Bad values fall back per key |
| `src/settings.rs` | Settings window (tray double-click): validates with `from_fields`, saves, applies live |
| `build.rs` | `/DELAYLOAD` for pdh/dxgi/version/dwmapi, the Common Controls v6 manifest, and the icon via SDK `rc.exe` (if `rc.exe` isn't found, the build still succeeds with the stock icon) |
| `.cargo/config.toml` | `+crt-static` for `x86_64-pc-windows-msvc` only. Linker args belong in `build.rs` (`rustc-link-arg-bins`) so they don't apply to the build script |
| `.github/workflows/ci.yml` | Push to `main` / PR: `fmt --check`, clippy `-D warnings`, tests, release build, exe uploaded as an artifact (windows-latest) |
| `.github/workflows/release.yml` | Tag `vX.Y.Z[-pre]`: checks the tag is semver and equals `Cargo.toml` `version`, builds, publishes a GitHub Release with `fps-overlay-vX.Y.Z-x86_64-windows.zip` (only `fps-overlay.exe` at its root, so it extracts over an install) + `.sha256` (`-pre` tags become pre-releases) |

## Runtime/Tooling Preferences

- **Target:** `x86_64-pc-windows-msvc` on Windows 10 1809+. Needs the MSVC toolchain, plus the Windows SDK for `rc.exe` (only for the icon). Edition 2024; no pinned toolchain.
- **Exactly one dependency:** `windows = 0.62.2`. To use a new API namespace, add the feature to that same array in `Cargo.toml`.
  - Don't add crates: no `rc`/`winres`, `serde`, INI, logging or GUI crates, and no dev-dependencies.
  - Don't add Direct2D, DirectWrite or a D3D device to the overlay.
- **Release profile:** `opt-level="s"`, `lto="fat"`, `codegen-units=1`, `panic="abort"`, `strip=true`. There are no symbols, and the release build has no console (`windows_subsystem`), so use a debug build to diagnose.
- **Runs elevated** (ETW needs it). Explorer runs unelevated and Windows blocks its messages to an admin process, so every Explorer→app message needs `ChangeWindowMessageFilterEx(.., MSGFLT_ALLOW)`. Today that covers `WM_TRAY` and `TaskbarCreated`. For the same reason, `FindWindow` from a non-elevated tool can't see this app's windows.
- **Formatting:** rustfmt defaults; there is no `rustfmt.toml` or `clippy.toml`. Clippy `-D warnings` must be clean.

## Testing & QA

- **Unit tests:** built-in `#[test]` in an inline `#[cfg(test)] mod tests` at the end of every module; there is no `tests/` dir. Name tests as behaviour statements with their units, e.g. `kernel_fallback_after_1500ms`, `half_bg_no_text_is_half_alpha`.
  - Pin boundaries, precedence, state transitions and round-trips; `parse(to_ini(c)) == c`, for example.
  - Don't test wiring, defaults or source text.
- **Coverage gaps:** `etw.rs` has no unit tests. Window and tray handling, the message loop, the settings window itself (only `from_fields` is tested), on-screen drawing (only `compose` is tested), and `App::tick` integration are all smoke-only.
- **Manual smoke** (elevated release build, with a borderless DX game or a stand-in such as a Godot test project run with `--rendering-driver d3d12|vulkan`):
  1. Desktop focused: no overlay, ~0% CPU, ≤ 5 MB. `logman query FPSOverlayRs_ETW -ets` shows no providers.
  2. Game focused: the bar appears within ~1 s and FPS is within ±5% of the game's own counter. DX games use DXGI/D3D9 only; Vulkan/GL adds DxgKrnl after ~1.5 s.
  3. Alt-tab: the overlay hides and the providers go off. Return: it reappears.
  4. Hotkeys: Insert toggles only in game, and the game still receives the key. End exits and the session is gone.
  5. Footprint: 30–60 s in game stays within budget. `layout=fps-only` loads no `pdh`/`dxgi`/`nvml`.
  6. Tray: double-click opens Settings, Save applies live, Cancel leaves `config.ini` unchanged, right-click → Exit.
- **Docs to keep in sync:**
  - Any change to behaviour or config: update `README.md` (config table, hotkeys, layouts, footprint numbers).
  - Binding-rule changes: update `docs/specs/…-design.md`.
  - Known drift in the spec: tray, Settings window and the 10 s backoff are listed as non-goals or missing, but are implemented.
