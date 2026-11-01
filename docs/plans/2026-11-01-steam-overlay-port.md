# FPS Overlay (Rust) — Steam-like + FPS-Only — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In-game-only overlay with two layouts — Steam-style one-line stats bar (default) and NVIDIA-style FPS-Only counter — with the smallest possible CPU/RAM/disk footprint.

**Architecture:** Single binary crate, one dependency (`windows`). Pure logic (config, layout runs, FPS windowing, game gate, alpha compose) in small unit-tested modules; Win32 edges are thin. Main thread is fully event-driven: `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` wakes it; a `SetTimer` exists only while a candidate/game is foreground. ETW `ProcessTrace` runs on one thread; providers are enabled only while a candidate is foreground.

**Tech Stack:** Rust stable 1.99, `windows` crate (minimal features). GDI for drawing. Raw ETW (`StartTraceW`/`EnableTraceEx2`/`ProcessTrace`).

**Spec:** `docs/specs/2026-11-01-steam-overlay-port-design.md`

## Global Constraints
- Windows 10 1809+ x64 only. `#![windows_subsystem = "windows"]`.
- Exactly one runtime dependency: `windows`. No dev-deps.
- Release profile: `opt-level = "s"`, `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`. `.cargo/config.toml`: `[target.x86_64-pc-windows-msvc] rustflags = ["-C", "target-feature=+crt-static"]`.
- No Direct2D/DirectWrite/D3D device, no swapchain, no per-frame work, no polling while idle, no heap allocation in the ETW callback.
- No driver, no .NET, no `RegisterHotKey`, no keyboard hooks.
- Budgets (spec Goals): exe ≤ 500 KB; idle CPU ≈ 0%, private bytes ≤ 5 MB; in-game CPU < 0.5% of a core, private bytes ≤ 10 MB.
- All exact values (GUIDs, event IDs, colors, thresholds, paddings, formats, config keys/defaults/ranges) come from the spec verbatim.
- Every Win32 metric failure yields `None`, never a panic.

## Review Focus
1. Foreground flips game↔desktop rapidly → counts from old PID must not leak into new PID's sample. Task 3 `target_change_discards_window`.
2. Game pauses/loads → overlay stays, FPS `---` after 2 s. Task 4 `sticky_after_qualify`, `stale_fps_is_none`.
3. Vulkan/OpenGL game (no DXGI/D3D9 events) → DxgKrnl fallback must turn on within 1.5 s and off when leaving. Task 4 `kernel_fallback_after_1500ms`, `leaving_disables_providers`.
4. Malformed `config.ini` → per-key default, no crash. Task 1 `out_of_range_falls_back`.
5. Stale ETW session after a crash → next launch starts. Task 3 Step 5, Task 7 smoke 6.

---

## File Structure
```
Cargo.toml, .cargo/config.toml
src/main.rs      // elevation, message loop, WinEvent hook, timer, hotkeys, wiring (Task 7)
src/config.rs    // Config + parse (Task 1)
src/layout.rs    // Stats -> Vec<Line> (Task 2)
src/fps.rs       // FpsCounter pure accumulator (Task 3)
src/etw.rs       // raw ETW session + provider enable/disable (Task 3)
src/gate.rs      // GameGate pure state machine (Task 4)
src/process.rs   // pid -> exe name / label (Task 4)
src/metrics.rs   // CPU, RAM, PDH GPU, DXGI, NVML (Task 5)
src/render.rs    // GDI layered window + alpha compose (Task 6)
```
Task 1 first (crate, all `windows` features, empty modules). Tasks 2–6 in parallel (no shared files). Task 7 last.

---

### Task 1: Crate scaffold + config

**Review:** per-task — persisted format

**Files:** Create `Cargo.toml`, `.cargo/config.toml`, `src/main.rs` (stub `fn main(){}` + `mod` lines), empty `src/{layout,fps,etw,gate,process,metrics,render}.rs`, `src/config.rs`.

**Interfaces — Produces:**
```rust
#[derive(Clone, Copy, Debug, PartialEq)] pub enum Corner { TopLeft, TopRight, BottomLeft, BottomRight }
#[derive(Clone, Copy, Debug, PartialEq)] pub enum Layout { Steam, FpsOnly }
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub layout: Layout, pub fps_only_background: bool,
    pub position: Corner, pub scale: f32 /*0.5..=2.0*/, pub background_opacity: f32 /*0..=1*/,
    pub show_fps: bool, pub show_cpu: bool, pub show_gpu: bool, pub show_gpu_clock: bool,
    pub show_vram: bool, pub show_ram: bool, pub show_process: bool, pub fahrenheit: bool,
    pub min_fps: f32, pub ignore: Vec<String> /*lowercased*/, pub toggle_key: u8, pub exit_key: u8,
}
impl Default for Config { /* spec defaults */ }
pub fn parse(ini: &str) -> Config;   // pure; only [Overlay]; unknown keys ignored
pub fn load() -> Config;             // <exe_dir>/config.ini via std::fs; writes DEFAULT_INI if missing
pub const DEFAULT_INI: &str;
```

- [ ] **Step 1:** `cargo init --bin`; add only `windows` with features: `Win32_Foundation, Win32_UI_WindowsAndMessaging, Win32_UI_Accessibility, Win32_UI_Input_KeyboardAndMouse, Win32_UI_HiDpi, Win32_UI_Shell, Win32_Graphics_Gdi, Win32_Graphics_Dxgi, Win32_System_Threading, Win32_System_SystemInformation, Win32_System_Performance, Win32_System_LibraryLoader, Win32_System_Diagnostics_Etw, Win32_System_ProcessStatus, Win32_Storage_FileSystem, Win32_Security, Win32_System_Console`. Release profile + `.cargo/config.toml` per Global Constraints. `cargo build --release` passes.
- [ ] **Step 2: Failing tests** in `config.rs`:
  - `empty_is_default`: `parse("") == Config::default()`; `layout == Steam`, `!fps_only_background`, `toggle_key == 45`, `exit_key == 35`, `min_fps == 20.0`, `ignore` contains `"explorer.exe"`.
  - `parses_layout`: `layout=fps-only\nfps_only_background=1` → `FpsOnly`, `fps_only_background`; `layout=bogus` → `Steam`.
  - `parses_all_keys`: `[Overlay]\nposition=bottom-right\nscale=150\nbackground_opacity=50\nshow_ram=0\nfahrenheit=1\nmin_fps=30\nignore=Foo.EXE, bar.exe\ntoggle_key=112` → `BottomRight`, `1.5`, `0.5`, `!show_ram`, `fahrenheit`, `30.0`, `["foo.exe","bar.exe"]`, `112`.
  - `out_of_range_falls_back`: `scale=999`, `background_opacity=-1`, `min_fps=abc`, `toggle_key=0`, `position=middle` → each default.
  - `other_section_ignored`: `[Other]\nscale=150` → `scale == 1.0`.
  - `default_ini_roundtrips`: `parse(DEFAULT_INI) == Config::default()`.
- [ ] **Step 3:** `cargo test config` → FAIL. **Step 4:** implement hand parser. **Step 5:** PASS. Commit `feat: crate scaffold and config.ini parsing`.

---

### Task 2: Layout model

**Files:** `src/layout.rs`

**Interfaces — Consumes:** `config::Config`. **Produces:**
```rust
#[derive(Clone, Copy, Debug, PartialEq)] pub struct Rgb(pub u8, pub u8, pub u8);   // spec floats * 255, rounded
#[derive(Clone, Debug, PartialEq)] pub struct Run { pub text: String, pub color: Rgb, pub gap_before: f32 }
#[derive(Clone, Debug, PartialEq)] pub struct Line { pub runs: Vec<Run>, pub size: f32 /*1.0 or 0.78*/ }
/// What the renderer draws. Base font px at scale 1 = `font_px` (17 steam, 22 fps-only).
#[derive(Clone, Debug, PartialEq)] pub struct Frame { pub lines: Vec<Line>, pub font_px: f32, pub semibold: bool,
    pub padding: (f32, f32), pub background: bool, pub shadow: bool }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats { pub fps: Option<f32>, pub cpu: Option<f32>, pub gpu: Option<f32>, pub gpu_temp_c: Option<f32>,
    pub gpu_mhz: Option<f32>, pub vram_used_gb: Option<f32>, pub vram_total_gb: Option<f32>,
    pub ram_used_gb: f32, pub ram_total_gb: f32, pub label: Option<String> }
pub fn build(s: &Stats, c: &Config) -> Frame;
```
Steam rules: `font_px 17`, `semibold false`, `padding (10,6)`, `background true`, `shadow false`; label→value `gap_before = 3.0`, within-group `4.0`, separator run `" | "` (0.55,0.55,0.58) gap 0 only between drawn groups; order/formats/colors/thresholds per spec "Layout: Steam-like". Line 2 (size 0.78, (0.42,0.52,0.42)) only when `show_process && label.is_some()`.
FPS-Only rules: one line, one run (FPS value only, tier color / `---` gray), `font_px 22`, `semibold true`, `padding (4,2)`, `background = c.fps_only_background`, `shadow true`. `Frame` equality is what Task 7 uses to skip redraws.

- [ ] **Step 1: Failing tests:**
  - `full_bar_text`: fps 180, cpu 15, gpu 25, temp 54, mhz 2505, vram 2.0/8, ram 12.8/16, label `"RocketLeague.exe (Rocket League)"` → line-1 concatenated text `"FPS180 | CPU15% | GPU25% 54°C | 2505 MHz | VRAM 25% 2.0/8G | RAM 80% 12.8/16G"`; 2 lines.
  - `fps_colors`: 60→`Rgb(46,240,115)`, 59→`Rgb(255,217,38)`, 29→`Rgb(255,64,64)`, None→`"---"` `Rgb(128,128,140)`.
  - `load_colors`: cpu 91→`Rgb(255,77,77)`, 71→`Rgb(255,217,38)`, 70→`Rgb(179,179,191)`.
  - `fahrenheit`: temp 54, fahrenheit → contains `"129°F"`; temp 86 °C red regardless of unit.
  - `gpu_none_renders_na`: gpu/temp/mhz/vram None → contains `"GPU"`, `"N/A"`; no `"MHz"`, no `"VRAM"`.
  - `disabled_groups_no_dangling_separator`: only `show_ram` → `"RAM 80% 12.8/16G"`.
  - `fps_only_single_run`: layout FpsOnly, full stats → 1 line, 1 run, text `"180"`, color `Rgb(46,240,115)`, `font_px 22.0`, `semibold`, `shadow`, `!background`; with `fps_only_background` → `background`.
  - `fps_only_none`: fps None → run text `"---"`.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** PASS. **Step 5:** Commit `feat: steam-like layout model`.

---

### Task 3: FPS counter + raw ETW session

**Review:** per-task — concurrency

**Files:** `src/fps.rs`, `src/etw.rs`

**Interfaces — Produces:**
```rust
// fps.rs — pure, time = QPC ticks (i64) + freq passed in new()
pub enum Source { D3d9, Dxgi, Kernel }
pub struct FpsCounter { /* target pid, window start, 3 u32 counts; no Vec */ }
impl FpsCounter {
    pub fn new(qpc_freq: i64) -> Self;
    pub fn on_present(&mut self, pid: u32, target: u32, src: Source, now: i64) -> Option<f32>; // target change resets window
    pub fn saw_user_mode(&self) -> bool;   // any D3d9/Dxgi event for current target since reset
}
// etw.rs
pub struct Shared {
    pub target_pid: AtomicU32,
    pub fps_bits: AtomicU32,        // f32 bits of last sample
    pub sample_pid: AtomicU32,
    pub sample_qpc: AtomicI64,      // 0 = none
    pub saw_user_mode: AtomicBool,
}
pub enum Providers { Off, UserMode /*DXGI+D3D9*/, WithKernel /*+DxgKrnl*/ }
pub struct Etw { /* TRACEHANDLE session, consumer thread JoinHandle */ }
impl Etw {
    pub fn start(shared: &'static Shared) -> windows::core::Result<Etw>; // session open, all providers OFF
    pub fn set_providers(&mut self, p: Providers) -> windows::core::Result<()>; // idempotent
}
impl Drop for Etw { /* ControlTraceW STOP, join */ }
```
`Shared` is a `static` (no `Arc`). Callback is `extern "system" fn(*mut EVENT_RECORD)` with `FpsCounter` in a `static mut`-free `thread_local!`/`UnsafeCell` owned by the consumer thread; writes results only via atomics. Selection rule per spec.

- [ ] **Step 1: Failing tests** (`fps.rs`, freq = 1000):
  - `dxgi_120_over_1s`: 120 Dxgi events over [0,1000] → `Some(≈120)` when `now >= 1000`.
  - `d3d9_wins_over_dxgi`: 60 D3d9 + 120 Dxgi → ≈60.
  - `kernel_only_below_20_dropped`: 10 Kernel / 1 s → `None`.
  - `kernel_only_above_20_counts`: 30 Kernel → ≈30.
  - `other_pid_ignored`: events for pid 2, target 1 → no sample.
  - `target_change_discards_window`: 50 events pid 1 (target 1), then target 2 at 500, 30 events pid 2 until 1500 → ≈30 at 1500.
  - `saw_user_mode_flag`: only Kernel → false; one Dxgi → true; target change → false.
- [ ] **Step 2:** FAIL. **Step 3:** implement `FpsCounter`. **Step 4:** PASS.
- [ ] **Step 5:** Implement `etw.rs`: `EVENT_TRACE_PROPERTIES` + name buffer, `LogFileMode = EVENT_TRACE_REAL_TIME_MODE`, `BufferSize = 64`, `MinimumBuffers = 4`, `MaximumBuffers = 8`, `FlushTimer = 1`, `Wnode.ClientContext = 1` (QPC). `ERROR_ALREADY_EXISTS` → `ControlTraceW(STOP)` by name, retry once. `OpenTraceW` + `ProcessTrace` on one thread. `set_providers` uses `EnableTraceEx2` with `ENABLE_TRACE_PARAMETERS` carrying `EVENT_FILTER_TYPE_EVENT_ID` (DXGI id 42; D3D9 id 1), DxgKrnl keywords `0x8000000|0x1`, level 4; disable with `EVENT_CONTROL_CODE_DISABLE_PROVIDER`. DXGI enable failure → `Err`; D3D9/DxgKrnl failures logged-to-nothing and tolerated.
- [ ] **Step 6:** Throwaway `examples/etw_probe.rs` (elevated): enable `UserMode` for a PID arg of a running D3D app, print fps for 5 s; switch to `Off`, confirm callback count stops. Delete example. Commit `feat: raw ETW present counting`.

---

### Task 4: Game gate + process label

**Files:** `src/gate.rs`, `src/process.rs`

**Interfaces — Consumes:** `config::Config`, `etw::Providers`. **Produces:**
```rust
// gate.rs — pure; time in ms (u64)
pub enum Timer { Off, Probe /*250 ms*/, Game /*100 ms*/ }
pub struct GateOut { pub timer: Timer, pub providers: Providers, pub visible: bool, pub fps: Option<f32> }
pub struct GameGate { /* pid, candidate, qualified, user_visible, fg_since_ms */ }
impl GameGate {
    pub fn new() -> Self;
    pub fn on_foreground(&mut self, pid: Option<u32>, exe: Option<&str>, cfg: &Config, now_ms: u64) -> GateOut;
    /// sample: (pid, fps, age_ms); saw_user_mode from Shared
    pub fn on_tick(&mut self, sample: Option<(u32, f32, u64)>, saw_user_mode: bool, cfg: &Config, now_ms: u64) -> GateOut;
    pub fn toggle(&mut self) -> GateOut;
    pub fn is_game(&self) -> bool;
}
// process.rs
pub fn exe_name(pid: u32) -> Option<String>;  // lowercased file name
pub fn label(pid: u32) -> Option<String>;     // "Name.exe (Description)" | "Name.exe"
```
Rules: per spec "Game gate" + resource rules. Not candidate → `{Off, Providers::Off, visible false}`. Candidate, not qualified → `{Probe, UserMode}`; after 1500 ms with `!saw_user_mode` → `WithKernel`. Qualified → `Game` timer, providers unchanged. Stale (> 2000 ms) → `fps None`.

- [ ] **Step 1: Failing tests:** `ignored_exe_is_idle` (explorer.exe → timer Off, providers Off); `candidate_probes_user_mode`; `kernel_fallback_after_1500ms` (no user-mode at 1499 → UserMode, at 1500 → WithKernel); `qualifies_at_min_fps` (20 → Game timer, visible); `below_min_not_game` (19); `sticky_after_qualify` (no sample later → still visible); `stale_fps_is_none` (age 2500 → fps None, visible); `sample_other_pid_ignored`; `leaving_disables_providers` (qualified, then fg ignored exe → Off/Off/hidden); `toggle_hides` (visible false, is_game true).
- [ ] **Step 2:** FAIL. **Step 3:** implement gate; `process.rs` via `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`, `QueryFullProcessImageNameW`, `GetFileVersionInfoSizeW`/`GetFileVersionInfoW`/`VerQueryValueW` (first translation, then `FileDescription`).
- [ ] **Step 4:** PASS + `own_process_label` (`label(std::process::id())` starts with test exe name). Commit `feat: game gate and process label`.

---

### Task 5: Native metrics (lazy)

**Files:** `src/metrics.rs`

**Interfaces — Produces:**
```rust
pub struct Metrics { /* prev GetSystemTimes; Option<PdhState>; adapter LUID + vram total + vendor; Option<Nvml> */ }
pub struct Sample { pub cpu: Option<f32>, pub gpu: Option<f32>, pub gpu_temp_c: Option<f32>, pub gpu_mhz: Option<f32>,
    pub vram_used_gb: Option<f32>, pub vram_total_gb: Option<f32>, pub ram_used_gb: f32, pub ram_total_gb: f32 }
impl Metrics {
    pub fn new() -> Self;               // cheap: no PDH, no NVML, no DXGI yet
    pub fn poll(&mut self) -> Sample;   // first call lazily: DXGI adapter pick (factory dropped), PDH open, NVML load if VendorId 0x10DE
}
pub fn luid_tag(high: i32, low: u32) -> String;  // "luid_0x{high:08X}_0x{low:08X}"
pub fn cpu_percent(idle: u64, kernel: u64, user: u64) -> f32;
```
PDH: `PdhOpenQueryW`, `PdhAddEnglishCounterW` wildcards, `PdhCollectQueryData`, `PdhGetFormattedCounterArrayW(PDH_FMT_DOUBLE)` into a reused `Vec<u8>` buffer (grow only), filter by `luid_tag`. NVML via `LoadLibraryW`/`GetProcAddress`. Metrics state lives for process lifetime once created (spec: kept).

- [ ] **Step 1: Failing tests:** `luid_tag_format` (`luid_tag(0,0x1234) == "luid_0x00000000_0x00001234"`); `cpu_percent_math` (`(25,80,20) → 75.0`, `(0,0,0) → 0.0`); `poll_smoke` (two polls 1 s apart; `ram_total_gb > 0`; second `cpu` in 0..=100); `new_is_cheap` (`Metrics::new()` loads no `pdh.dll`: `GetModuleHandleW("pdh.dll")` is null after `new`).
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** PASS; `cargo test poll_smoke -- --nocapture`, compare GPU %/VRAM with Task Manager; time one `poll()` (print µs). Commit `feat: lazy native metrics`.

---

### Task 6: GDI layered renderer

**Files:** `src/render.rs`

**Interfaces — Consumes:** `layout::{Line, Run, Rgb}`, `config::{Config, Corner}`. **Produces:**
```rust
pub fn compose(color: &[u32], mask: &[u32], bg_alpha: u8, out: &mut [u32]); // pure, BGRA premultiplied
pub struct Overlay { /* hwnd, mem DC, 2 DIBs (reused, regrown only when larger), cached HFONTs per px size */ }
impl Overlay {
    pub fn new(cfg: &Config) -> windows::core::Result<Self>;  // hidden window, spec styles
    pub fn draw(&mut self, frame: &Frame, anchor: HWND) -> windows::core::Result<()>;
    pub fn hide(&self);
    pub fn release_buffers(&mut self);  // drop DIBs + fonts when idle
}
```
`compose` per spec: `cov = mask.r/255; a = cov + bg_a·(1−cov)`; out rgb = color rgb (already premultiplied by cov because drawn on black); out a = `a·255`. Measure with `GetTextExtentPoint32W`, draw with `SetTextColor` + `ExtTextOutW`, font `CreateFontW(-px, …, ANTIALIASED_QUALITY, "Segoe UI")`, px = `frame.font_px × line.size × scale × dpi/96`; padding `frame.padding × scale × dpi/96`. DPI via `GetDpiForMonitor`. Place at corner of `MonitorFromWindow(anchor).rcMonitor`; `UpdateLayeredWindow(ULW_ALPHA)`; `ShowWindow(SW_SHOWNOACTIVATE)`.

- [ ] **Step 1: Failing tests** for `compose`: `transparent_bg_no_text` (mask 0, bg 0 → out 0); `opaque_bg_no_text` (bg 255 → alpha 255, rgb 0); `full_text_pixel` (mask white, color (200,100,50), bg 0 → `0xFFC86432` BGRA-packed as `0xFF_C8_64_32`); `half_cov_half_bg` (mask 128, bg 128 → alpha ≈ 192); `shadow_pixel` (mask white, color black (shadow), bg 0 → `0xFF000000`). Shadow = draw each run first in black at (+1,+1) into both color and mask DIBs, then the text; only when `frame.shadow`. Background rect only when `frame.background` (else bg_alpha 0). `FW_SEMIBOLD` when `frame.semibold`.
- [ ] **Step 2:** FAIL. **Step 3:** implement `compose` then `Overlay`. **Step 4:** PASS.
- [ ] **Step 5:** Throwaway `examples/render_demo.rs`: spec reference stats → `layout::build` for both layouts (or hand-made `Frame`s if Task 2 not merged), draw each at each corner 2 s. Screenshot Steam vs `D:/Coding/OtherCode/fps-overlay/screenshots/steamlike_layout.png`; FPS-Only readable over bright and dark wallpaper; confirm click-through + no focus steal. Delete example. Commit `feat: GDI layered overlay renderer`.

---

### Task 7: Wiring, elevation, hotkeys, resource smoke

**Files:** `src/main.rs`, `README.md` (usage, config keys, borderless + admin requirement, measured footprint).

**Interfaces — Consumes:** all above.

- [ ] **Step 1:** `main`: `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`; elevation check (`TokenElevation`) → `ShellExecuteW("runas")` and return; `SetPriorityClass(BELOW_NORMAL_PRIORITY_CLASS)`; `config::load()`; `Etw::start` (Err → `MessageBoxW` with code, return); `Overlay::new`; `Metrics::new`; `SetConsoleCtrlHandler` posts `WM_QUIT`.
- [ ] **Step 2:** `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, cb, 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS)`; handle current foreground once at startup. Blocking `GetMessageW` loop (no `PeekMessage`). Hook cb: pid + exe → `gate.on_foreground` → apply `GateOut`.
- [ ] **Step 3:** Apply `GateOut`: store `target_pid`; `etw.set_providers` only when changed; `SetTimer`/`KillTimer` only when timer kind changed; hidden→ `overlay.hide()`; on transition to `Timer::Off`: `overlay.release_buffers()` + `SetProcessWorkingSetSize(-1,-1)`.
- [ ] **Step 4:** `WM_TIMER`: read `Shared` atomics → `gate.on_tick`; if `is_game`: edge-detect `GetAsyncKeyState(toggle/exit) & 0x8000` (toggle → `gate.toggle()`, exit → `PostQuitMessage`); **Steam layout only:** every 1000 ms `metrics.poll()` and label cached per pid (FpsOnly never calls `poll()` nor queries the label, so PDH/DXGI/NVML never load); `layout::build`; `overlay.draw` only if `visible` and (`frame != last_frame` or monitor/visibility changed).
- [ ] **Step 5: Smoke (elevated, release build)** — record observed values:
  1. Desktop focused → no overlay; Process Explorer: 0 timers firing (CPU cycles delta ~0 over 30 s), private bytes ≤ 5 MB.
  2. Borderless DX game focused → bar within ~1 s, FPS ±5% of in-game counter; `logman query FPSOverlayRs_ETW -ets` shows DXGI/D3D9 only, no DxgKrnl.
  3. Vulkan/OpenGL app → DxgKrnl enabled after ~1.5 s, FPS shown.
  4. In-game 60 s: CPU < 0.5% avg, private bytes ≤ 10 MB.
  5. Alt-tab to browser → bar hides, `logman` shows providers disabled; back → reappears.
  6. Insert in game hides/shows; Insert on desktop no effect; key still reaches game. End exits; session gone from `logman query -ets`.
  7. Kill via Task Manager, relaunch → starts (stale session path).
  8. `config.ini` `position=bottom-right`, `show_gpu_clock=0`, `fahrenheit=1` → reflected after restart.
  8b. `layout=fps-only` → only FPS number, drop shadow, no background; Process Explorer DLL view shows **no** `pdh.dll`/`nvml.dll`/`dxgi.dll` loaded; in-game CPU lower than Steam layout.
  9. `target/release/*.exe` size ≤ 500 KB; `dumpbin /dependents` shows only system DLLs.
- [ ] **Step 6:** `cargo clippy -- -D warnings`, `cargo test`, `cargo build --release` clean. Record footprint numbers in README. Commit `feat: event-driven overlay with in-game gate`.
