#![cfg_attr(not(test), windows_subsystem = "windows")]

mod config;
mod etw;
mod fps;
mod gate;
mod layout;
mod metrics;
mod process;
mod render;

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::{Acquire, Relaxed, Release}};

use windows::core::{BOOL, PCWSTR, w};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{HMONITOR, MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::Security::{TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, OpenProcessToken,
    SetPriorityClass, SetProcessWorkingSetSize,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, EVENT_SYSTEM_FOREGROUND,
    GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, KillTimer, MB_ICONERROR, MB_OK,
    MSG, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SW_SHOWNORMAL,
    SetTimer, TranslateMessage, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_ENDSESSION,
    WM_CLOSE, WM_QUERYENDSESSION, WM_TIMER, WNDCLASSW, WINDOW_EX_STYLE, WS_OVERLAPPED,
};

use crate::config::{Config, Layout};
use crate::etw::{Providers, SHARED};
use crate::gate::{GameGate, GateOut, Timer};
use crate::layout::Stats;

const TIMER_ID: usize = 1;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}
/// Message window; the console-control handler runs on its own thread, so it
/// posts to this window instead of re-entering our state.
static MAIN_HWND: AtomicUsize = AtomicUsize::new(0);
/// Win32 code of a fatal provider failure, shown after the message loop ends.
static FATAL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

struct App {
    cfg: Config,
    gate: GameGate,
    overlay: render::Overlay,
    etw: etw::Etw,
    metrics: metrics::Metrics,
    hwnd: HWND,
    timer: Timer,
    visible: bool,
    shown: bool,
    fg_pid: u32,
    fg_hwnd: HWND,
    target: u32,
    anchor_mon: HMONITOR,
    anchor_dpi: u32,
    stats: Stats,
    frame: Option<layout::Frame>,
    label: (u32, Option<String>),
    last_poll: u64,
    keys: (bool, bool),
    keys_armed: bool,
    freq: i64,
    t0: i64,
}

impl App {
    fn qpc(&self) -> i64 {
        let mut q = 0i64;
        // SAFETY: out-param only.
        unsafe {
            let _ = QueryPerformanceCounter(&mut q);
        }
        q
    }

    fn now(&self) -> u64 {
        ms_between(self.freq, self.qpc(), self.t0)
    }

    /// New foreground window (or `HWND::default()`), from the hook or startup.
    fn foreground(&mut self, hwnd: HWND) {
        let mut pid = 0u32;
        if !hwnd.0.is_null() {
            // SAFETY: out-param only.
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        }
        let exe = (pid != 0).then(|| process::exe_name(pid)).flatten();
        // Skip our own process exactly like the ETW target does.
        let pid = (pid != 0 && pid != std::process::id()).then_some(pid);
        self.fg_pid = pid.unwrap_or(0);
        self.fg_hwnd = hwnd;
        let out = self.gate.on_foreground(pid, exe.as_deref(), &self.cfg, self.now());
        self.apply(out);
    }

    fn apply(&mut self, out: GateOut) {
        let target = if out.providers == Providers::Off { 0 } else { self.fg_pid };
        if target != self.target {
            self.target = target;
            SHARED.target_pid.store(target, Release);
        }
        // Idempotent: a no-op unless the provider set actually changed.
        if let Err(e) = self.etw.set_providers(out.providers) {
            // Only a DXGI enable failure gets here (D3D9/DxgKrnl are tolerated
            // inside set_providers). The spec wants a message box and exit, but
            // showing it from inside a tick would pump messages under the App
            // borrow, so record it and let main show it after the loop.
            FATAL.store(e.code().0 as u32, Relaxed);
            // SAFETY: posts to this thread's queue, breaking GetMessageW.
            unsafe { PostQuitMessage(0) };
            return;
        }

        if out.timer != self.timer {
            // SAFETY: our own timer on our own window.
            unsafe {
                if out.timer == Timer::Off {
                    let _ = KillTimer(Some(self.hwnd), TIMER_ID);
                } else {
                    SetTimer(Some(self.hwnd), TIMER_ID, interval(out.timer), None);
                }
            }
            let left_game = self.timer != Timer::Off && out.timer == Timer::Off;
            self.timer = out.timer;
            if left_game {
                self.become_idle();
            }
        }

        self.visible = out.visible;
        if !out.visible {
            self.hide();
        }
    }

    fn hide(&mut self) {
        if self.shown {
            self.overlay.hide();
            self.shown = false;
            self.frame = None;
        }
    }

    /// No candidate foreground: free per-game scratch and trim the working set.
    fn become_idle(&mut self) {
        self.overlay.release_buffers();
        // SAFETY: own process handle; SIZE_MAX/-1 asks Windows to trim.
        unsafe {
            let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
        }
        self.stats = Stats::default();
        self.label = (0, None);
        self.last_poll = 0;
        self.keys_armed = false;
    }

    fn tick(&mut self) {
        let qpc = self.qpc();
        let now = ms_between(self.freq, qpc, self.t0);
        // Self-heal: a missed or coalesced WinEvent would otherwise leave the gate
        // watching a process that is no longer (or no longer only) foreground.
        // SAFETY: read-only query.
        let fg = unsafe { GetForegroundWindow() };
        if !fg.0.is_null() && fg != self.fg_hwnd {
            self.foreground(fg);
        }
        // Load the timestamp first: it is the commit flag for the other fields.
        let at = SHARED.sample_qpc.load(Acquire);
        let sample = (at != 0).then(|| {
            (
                SHARED.sample_pid.load(Relaxed),
                f32::from_bits(SHARED.fps_bits.load(Relaxed)),
                ms_between(self.freq, qpc, at),
            )
        });
        let saw_user_mode = self.target != 0 && SHARED.saw_pid.load(Relaxed) == self.target;
        let out = self.gate.on_tick(sample, saw_user_mode, &self.cfg, now);
        self.apply(out);

        if !self.gate.is_game() {
            self.keys_armed = false;
            return;
        }
        if self.hotkeys() {
            return;
        }
        if !self.visible {
            return;
        }

        // Steam layout only: one poll per second, label cached per pid. FpsOnly
        // never reaches here, so PDH/DXGI/NVML are never even loaded.
        if self.cfg.layout == Layout::Steam && now.saturating_sub(self.last_poll) >= 1000 {
            self.last_poll = now;
            let s = self.metrics.poll();
            self.stats.cpu = s.cpu;
            self.stats.gpu = s.gpu;
            self.stats.gpu_temp_c = s.gpu_temp_c;
            self.stats.gpu_mhz = s.gpu_mhz;
            self.stats.vram_used_gb = s.vram_used_gb;
            self.stats.vram_total_gb = s.vram_total_gb;
            self.stats.ram_used_gb = s.ram_used_gb;
            self.stats.ram_total_gb = s.ram_total_gb;
            if self.label.0 != self.fg_pid {
                self.label = (self.fg_pid, process::label(self.fg_pid));
            }
            self.stats.label = self.label.1.clone();
        }
        self.stats.fps = out.fps;

        let anchor = self.fg_hwnd;
        let (mon, dpi) = monitor_of(anchor);
        let frame = layout::build(&self.stats, &self.cfg);
        let want = needs_draw(
            self.shown,
            mon != self.anchor_mon || dpi != self.anchor_dpi,
            self.frame.as_ref() != Some(&frame),
        );
        let r = if want { self.overlay.draw(&frame, anchor) } else { Ok(()) };
        if want && r.is_ok() {
            self.frame = Some(frame);
            self.anchor_mon = mon;
            self.anchor_dpi = dpi;
            self.shown = true;
        }
    }

    /// Edge-triggered, non-consuming hotkeys; only called while `is_game`.
    /// Returns true when the exit key asked the process to quit.
    fn hotkeys(&mut self) -> bool {
        let down = |vk: u8| (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0;
        let cur = (down(self.cfg.toggle_key), down(self.cfg.exit_key));
        if !self.keys_armed {
            // A key already held when the overlay appears must not act.
            self.keys = cur;
            self.keys_armed = true;
            return false;
        }
        let (toggle, exit) = edges(self.keys, cur);
        self.keys = cur;
        if toggle {
            let out = self.gate.toggle();
            self.apply(out);
        }
        if exit {
            // SAFETY: posts to this thread's queue, breaking GetMessageW.
            unsafe { PostQuitMessage(0) };
        }
        exit
    }
}

fn interval(timer: Timer) -> u32 {
    match timer {
        Timer::Off => 0,
        Timer::Probe => 250,
        Timer::Game => 100,
        Timer::SlowProbe => 1000,
    }
}

/// Monitor and effective DPI of `hwnd`, for placement-change detection.
fn monitor_of(hwnd: HWND) -> (HMONITOR, u32) {
    // SAFETY: read-only queries; a null anchor maps to the primary monitor.
    unsafe {
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        (mon, if x == 0 { 96 } else { x })
    }
}

/// A visible overlay must be pushed when it is not on screen yet (even if the
/// content is unchanged — a hidden window needs the show), when the placement
/// anchor moved, or when the content changed.
fn needs_draw(shown: bool, anchor_moved: bool, content_changed: bool) -> bool {
    !shown || anchor_moved || content_changed
}

/// Up→down edges for `(toggle, exit)`: a key held across ticks fires once.
fn edges(prev: (bool, bool), cur: (bool, bool)) -> (bool, bool) {
    (cur.0 && !prev.0, cur.1 && !prev.1)
}

/// Milliseconds from `then` to `now` on a QPC clock of `freq` ticks/s.
fn ms_between(freq: i64, now: i64, then: i64) -> u64 {
    if freq <= 0 || then <= 0 {
        return 0;
    }
    let delta = now.saturating_sub(then).max(0) as u128;
    (delta * 1000 / freq as u128) as u64
}

fn with_app(f: impl FnOnce(&mut App)) {
    APP.with(|slot| {
        if let Some(app) = slot.borrow_mut().as_mut() {
            f(app);
        }
    });
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn show_error(text: &str) {
    let text = wide(text);
    let title = wide("FPS Overlay");
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

fn elevated() -> bool {
    let mut token = HANDLE::default();
    let mut value = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: token is closed below; value/len are valid out-params.
    unsafe {
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let ok = windows::Win32::Security::GetTokenInformation(
            token,
            windows::Win32::Security::TokenElevation,
            Some(&mut value as *mut _ as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && value.TokenIsElevated != 0
    }
}

/// Relaunch self elevated. Declined UAC (or any failure) is silent, per spec.
fn relaunch_elevated() {
    let Ok(exe) = std::env::current_exe() else { return };
    let file = wide(&exe.to_string_lossy());
    let dir = exe.parent().map(|d| wide(&d.to_string_lossy()));
    // SAFETY: buffers are NUL-terminated and outlive the call.
    unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            PCWSTR(file.as_ptr()),
            None,
            dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            SW_SHOWNORMAL,
        );
    }
}

fn main() {
    // SAFETY: fixed startup calls; every failure is non-fatal.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if !elevated() {
            relaunch_elevated();
            return;
        }
        let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
    }

    let cfg = config::load();
    let etw = match etw::Etw::start(&SHARED) {
        Ok(e) => e,
        Err(e) => {
            show_error(&format!("ETW session failed: 0x{:08X}", e.code().0 as u32));
            return;
        }
    };
    let overlay = match render::Overlay::new(&cfg) {
        Ok(o) => o,
        Err(e) => {
            show_error(&format!("Overlay window failed: 0x{:08X}", e.code().0 as u32));
            return;
        }
    };
    let Some(hwnd) = main_window() else {
        show_error("Overlay message window failed to create");
        return;
    };
    MAIN_HWND.store(hwnd.0 as usize, Relaxed);

    let mut freq = 0i64;
    let mut t0 = 0i64;
    // SAFETY: out-params only.
    unsafe {
        let _ = QueryPerformanceFrequency(&mut freq);
        let _ = QueryPerformanceCounter(&mut t0);
    }
    let app = App {
        cfg,
        gate: GameGate::new(),
        overlay,
        etw,
        metrics: metrics::Metrics::new(),
        hwnd,
        timer: Timer::Off,
        visible: false,
        shown: false,
        fg_pid: 0,
        fg_hwnd: HWND::default(),
        target: 0,
        anchor_mon: HMONITOR::default(),
        anchor_dpi: 0,
        stats: Stats::default(),
        frame: None,
        label: (0, None),
        last_poll: 0,
        keys: (false, false),
        keys_armed: false,
        freq,
        t0,
    };
    APP.with(|slot| *slot.borrow_mut() = Some(app));

    // SAFETY: hooks/handlers live for the process lifetime; the loop pumps them.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(ctrl_handler), true);
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        with_app(|app| app.foreground(GetForegroundWindow()));

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Explicit drop: stops the ETW session and frees the overlay before exit.
    APP.with(|slot| drop(slot.borrow_mut().take()));

    let code = FATAL.load(Relaxed);
    if code != 0 {
        show_error(&format!("DXGI provider failed: 0x{code:08X}"));
    }
}

fn main_window() -> Option<HWND> {
    const CLASS: PCWSTR = w!("FpsOverlayRsMain");
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    // SAFETY: class registration is kept alive by the window; ours is one instance.
    unsafe {
        if !REGISTERED.swap(true, Relaxed) {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(main_wndproc),
                lpszClassName: CLASS,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                REGISTERED.store(false, Relaxed);
                return None;
            }
        }
        // A real (never shown) top-level window: message-only windows miss
        // broadcast messages such as WM_ENDSESSION.
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!(""),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )
        .ok()
    }
}

unsafe extern "system" fn main_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            with_app(|app| app.tick());
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_CLOSE => {
            // SAFETY: posts to the message loop's thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_ENDSESSION => {
            // wParam FALSE means the shutdown was cancelled: keep running.
            if wp.0 != 0 {
                // Windows may end the process as soon as we return, so drop the
                // state here: Etw::drop stops the session instead of orphaning it.
                APP.with(|slot| {
                    if let Ok(mut slot) = slot.try_borrow_mut() {
                        drop(slot.take());
                    }
                });
                // SAFETY: posts to the message loop's thread.
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    with_app(|app| app.foreground(hwnd));
}

unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> BOOL {
    // The handler runs on its own thread: ask the UI window to quit, which
    // breaks GetMessageW and lets the event loop drop the ETW session cleanly.
    // SAFETY: the handle outlives the window (checked non-null before use).
    unsafe {
        let hwnd = MAIN_HWND.load(Relaxed);
        if hwnd != 0 {
            let _ = PostMessageW(Some(HWND(hwnd as *mut c_void)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    BOOL(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_intervals() {
        assert_eq!(interval(Timer::Probe), 250);
        assert_eq!(interval(Timer::Game), 100);
        assert_eq!(interval(Timer::Off), 0);
    }

    #[test]
    fn hidden_overlay_always_redraws() {
        // Regression: hidden → visible must push even when the frame is unchanged.
        assert!(needs_draw(false, false, false));
        assert!(needs_draw(false, false, true));
        assert!(!needs_draw(true, false, false));
        assert!(needs_draw(true, true, false));
        assert!(needs_draw(true, false, true));
    }

    #[test]
    fn held_key_fires_once() {
        assert_eq!(edges((false, false), (true, false)), (true, false));
        assert_eq!(edges((true, false), (true, false)), (false, false));
        assert_eq!(edges((true, false), (false, false)), (false, false));
        assert_eq!(edges((false, true), (false, true)), (false, false));
        // Arming with the key already down suppresses the edge.
        assert_eq!(edges((true, true), (true, true)), (false, false));
    }

    #[test]
    fn qpc_to_ms() {
        assert_eq!(ms_between(1000, 1500, 1000), 500);
        assert_eq!(ms_between(1000, 1000, 1500), 0); // clock never runs backwards
        assert_eq!(ms_between(10_000_000, 5_000_000, 2_000_000), 300);
        assert_eq!(ms_between(10_000_000, 5_000_000, 0), 0); // 0 = "no sample"
        assert_eq!(ms_between(0, 10, 0), 0);
    }
}
