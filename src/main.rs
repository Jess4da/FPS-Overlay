#![cfg_attr(not(test), windows_subsystem = "windows")]

mod audio;
mod autostart;
mod config;
mod etw;
mod fps;
mod gate;
mod layout;
mod metrics;
mod mute;
mod process;
mod render;
mod settings;
mod update;

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{
    AtomicBool, AtomicU32, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT,
    RPC_E_CHANGED_MODE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow,
};
use windows::Win32::Security::{TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, CreateMutexW, GetCurrentProcess, OpenProcessToken,
    SetPriorityClass, SetProcessWorkingSetSize,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFY_ICON_MESSAGE, NOTIFYICONDATAW,
    Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_DBLCLKS, ChangeWindowMessageFilterEx, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetClientRect,
    GetCursorPos, GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowThreadProcessId,
    HICON, IDI_APPLICATION, IDYES, IMAGE_ICON, IsDialogMessageW, KillTimer, LR_DEFAULTCOLOR,
    LoadIconW, LoadImageW, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO, MF_CHECKED,
    MF_SEPARATOR, MF_STRING, MSG, MSGFLT_ALLOW, MessageBoxW, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SW_SHOWNORMAL,
    SetForegroundWindow, SetMenuDefaultItem, SetTimer, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WM_APP, WM_CLOSE, WM_ENDSESSION, WM_LBUTTONDBLCLK, WM_QUERYENDSESSION,
    WM_RBUTTONUP, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSW, WS_OVERLAPPED, WTS_SESSION_UNLOCK,
};
use windows::core::{BOOL, PCWSTR, w};

const WM_TRAY: u32 = WM_APP + 1;
/// Update check result: lParam is a `Box<String>` tag leaked as `usize`.
const WM_UPDATE: u32 = WM_APP + 2;
/// Audio session or endpoint changed; wParam is `audio::NOTIFY_*`.
pub(crate) const WM_AUDIO: u32 = WM_APP + 3;
/// Set when an update was installed; the loop exit then starts the new exe.
static RESTART: AtomicBool = AtomicBool::new(false);
const ID_EXIT: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_AUTOSTART: usize = 3;
const ID_IGNORE: usize = 4;
const ID_MUTE: usize = 5;
/// Explorer broadcasts this after it restarts; the tray icon must be re-added.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

/// Embedded icon (resource id 1, see build.rs) at the tray's small-icon size; stock icon fallback.
fn tray_icon() -> HICON {
    // SAFETY: own module handle; resource id 1 written by build.rs.
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        let (w, h) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        match LoadImageW(
            Some(module.into()),
            PCWSTR(1 as _),
            IMAGE_ICON,
            w,
            h,
            LR_DEFAULTCOLOR,
        ) {
            Ok(h) => HICON(h.0),
            Err(_) => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

fn tray(hwnd: HWND, op: NOTIFY_ICON_MESSAGE) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: tray_icon(),
        ..Default::default()
    };
    for (d, s) in nid.szTip.iter_mut().zip("FPS Overlay".encode_utf16()) {
        *d = s;
    }
    // SAFETY: nid is fully initialised for this call.
    let _ = unsafe { Shell_NotifyIconW(op, &nid) };
}

fn tray_menu(hwnd: HWND) {
    // SAFETY: menu is created, shown modally and destroyed in this scope.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Settings…"));
        let _ = SetMenuDefaultItem(menu, ID_SETTINGS as u32, 0);
        let mut game = None;
        let mut mute = None;
        with_app(|app| {
            game = app
                .last_game
                .clone()
                .filter(|g| !app.cfg.ignore.contains(g));
            mute = app
                .last_fg_exe
                .clone()
                .map(|e| (app.cfg.mute.contains(&e), e));
        });
        let item = game.as_ref().map(|g| wide(&format!("Don't show for {g}")));
        if let Some(t) = &item {
            let _ = AppendMenuW(menu, MF_STRING, ID_IGNORE, PCWSTR(t.as_ptr()));
        }
        let mute_item = mute
            .as_ref()
            .map(|(listed, e)| wide(&mute_menu_label(e, *listed)));
        if let Some(t) = &mute_item {
            let _ = AppendMenuW(menu, MF_STRING, ID_MUTE, PCWSTR(t.as_ptr()));
        }
        let auto = autostart::enabled();
        let flags = if auto {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let _ = AppendMenuW(menu, flags, ID_AUTOSTART, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit FPS Overlay"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            ID_EXIT => {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            ID_SETTINGS => open_settings(),
            ID_IGNORE => {
                if let Some(g) = game {
                    ignore_exe(hwnd, &g);
                }
            }
            ID_MUTE => {
                // Act on what the label offered, not on state that may have changed.
                if let Some((listed, e)) = mute {
                    set_muted(hwnd, &e, !listed);
                }
            }
            ID_AUTOSTART => {
                if let Err(e) = autostart::set(!auto) {
                    let t = wide(&format!("Could not change Start with Windows:\n{e}"));
                    MessageBoxW(
                        Some(hwnd),
                        PCWSTR(t.as_ptr()),
                        w!("FPS Overlay"),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
            _ => {}
        }
    }
}

/// Tray "Don't show for X": append to `ignore`, save, apply live.
/// ponytail: an already-open Settings window keeps its old ignore text; saving it
/// drops the new entry. Refresh its edit box here if that bites.
fn ignore_exe(hwnd: HWND, exe: &str) {
    edit_config(hwnd, |cfg| config::add_to_list(&mut cfg.ignore, exe));
}

/// Tray "Mute X in background" (`mute = true`) / "Stop muting X" (`false`):
/// add or remove `exe` in `mute`; a no-op if it is already in that state.
/// Same open-Settings ponytail as `ignore_exe`.
fn set_muted(hwnd: HWND, exe: &str, mute: bool) {
    edit_config(hwnd, |cfg| {
        if mute {
            config::add_to_list(&mut cfg.mute, exe)
        } else {
            let n = cfg.mute.len();
            cfg.mute.retain(|e| e != exe);
            cfg.mute.len() != n
        }
    });
}

/// Clone the config out, change it, save, apply live. `change` returns false for
/// a no-op. The error box runs with no APP borrow held.
fn edit_config(hwnd: HWND, change: impl FnOnce(&mut Config) -> bool) {
    let mut cfg = None;
    with_app(|app| cfg = Some(app.cfg.clone()));
    let Some(mut cfg) = cfg else { return };
    if !change(&mut cfg) {
        return;
    }
    if let Err(e) = config::save(&cfg) {
        let t = wide(&format!("Could not write config.ini:\n{e}"));
        // SAFETY: NUL-terminated buffer outlives the modal call; no APP borrow held.
        unsafe {
            MessageBoxW(
                Some(hwnd),
                PCWSTR(t.as_ptr()),
                w!("FPS Overlay"),
                MB_OK | MB_ICONERROR,
            )
        };
        return;
    }
    with_app(|app| app.reconfigure(cfg));
}

fn mute_menu_label(exe: &str, listed: bool) -> String {
    if listed {
        format!("Stop muting {exe}")
    } else {
        format!("Mute {exe} in background")
    }
}

/// Spec: the audio machinery exists while there is a list to apply or a mute
/// from an earlier run (or a failed unmute) still to undo.
fn mute_wanted(list_empty: bool, persisted_empty: bool) -> bool {
    !list_empty || !persisted_empty
}

/// Foreground exe offered in the tray. The tray click itself focuses the
/// taskbar (explorer.exe), so that never replaces the app the user meant.
/// ponytail: name match; a real File Explorer window can't be offered either.
fn tray_candidate(exe: &str) -> bool {
    exe != "explorer.exe"
}

fn open_settings() {
    // Clone out first: the window's callbacks re-enter APP, so no borrow may be held.
    let mut cfg = None;
    with_app(|app| cfg = Some(app.cfg.clone()));
    if let Some(cfg) = cfg {
        settings::open(&cfg, |new| with_app(|app| app.reconfigure(new)));
    }
}

use crate::config::{Config, Layout};
use crate::etw::{Providers, SHARED};
use crate::gate::{GameGate, GateOut, Timer};
use crate::layout::Stats;

const TIMER_ID: usize = 1;
/// One-shot mute debounce after a foreground change (absorbs alt-tab flicker).
const MUTE_TIMER_ID: usize = 2;
const MUTE_DEBOUNCE_MS: u32 = 150;
/// CoInitializeEx returned S_OK/S_FALSE on the UI thread: CoUninitialize at exit.
static COM_INIT: AtomicBool = AtomicBool::new(false);

/// Present only while muting is wanted (see `mute_wanted`); lives on the UI thread.
struct MuteState {
    mixer: audio::WasapiMixer,
    muter: mute::Muter,
    /// Last contents written to `muted.txt`.
    saved: BTreeSet<String>,
}

impl MuteState {
    /// Write `muted.txt` when the persisted set changed. On a write failure the
    /// next change retries.
    fn save(&mut self) {
        let p = self.muter.persisted();
        if p != self.saved && config::save_owned(&p).is_ok() {
            self.saved = p;
        }
    }
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}
/// Message window; the console-control handler runs on its own thread, so it
/// posts to this window instead of re-entering our state.
pub(crate) static MAIN_HWND: AtomicUsize = AtomicUsize::new(0);
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
    /// Exe of the last process the overlay showed for, offered in the tray menu.
    last_game: Option<String>,
    /// Exe of the foreground window; `None` for our own process or no window.
    fg_exe: Option<String>,
    /// Last `fg_exe` that passed `tray_candidate`, offered in the tray menu.
    last_fg_exe: Option<String>,
    mute: Option<MuteState>,
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
        self.fg_exe = pid.and(exe.clone());
        if let Some(e) = self.fg_exe.as_ref().filter(|e| tray_candidate(e)) {
            self.last_fg_exe = Some(e.clone());
        }
        // Retry a failed start (e.g. audio service not up yet at logon); free when `mute` is empty.
        if self.mute.is_none() && !self.cfg.mute.is_empty() {
            self.sync_mute();
        }
        if self.mute.is_some() {
            // SAFETY: our own one-shot timer on our own window; re-arming resets it.
            unsafe { SetTimer(Some(self.hwnd), MUTE_TIMER_ID, MUTE_DEBOUNCE_MS, None) };
        }
        let out = self
            .gate
            .on_foreground(pid, exe.as_deref(), &self.cfg, self.now());
        self.apply(out);
    }

    fn apply(&mut self, out: GateOut) {
        // Self-heal before touching providers: enabling on a dead session would
        // be reported as fatal. A failed restart retries on the next call.
        let live = self.etw.is_alive() || self.restart_etw();
        let target = if out.providers == Providers::Off {
            0
        } else {
            self.fg_pid
        };
        if target != self.target {
            self.target = target;
            SHARED.target_pid.store(target, Release);
        }
        // Idempotent: a no-op unless the provider set actually changed. A session
        // that died meanwhile is not fatal: the next call restarts it.
        if live
            && let Err(e) = self.etw.set_providers(out.providers)
            && self.etw.is_alive()
        {
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

        if out.visible && !self.visible {
            self.last_game = process::exe_name(self.fg_pid);
        }
        self.visible = out.visible;
        if !out.visible {
            self.hide();
        }
    }

    /// The ETW session ended under us (another process stopped it by name,
    /// `logman stop`, session loss) and the consumer thread exited. Start a
    /// fresh one; on failure keep the dead one so the caller retries later.
    fn restart_etw(&mut self) -> bool {
        // Before `start`: the new consumer then sees the target `apply` stores
        // next as new and resets its window (and `saw_pid`) on that pid's first event.
        self.target = 0;
        SHARED.target_pid.store(0, Release);
        let Ok(etw) = etw::Etw::start(&SHARED) else {
            return false;
        };
        // Drops the dead one: no stop (already gone), CloseTrace, join a finished thread.
        // The new session starts with providers off, so `apply` re-enables them.
        self.etw = etw;
        true
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

    /// Settings saved: apply without restart. Hide, drop cached frame, and re-run
    /// the gate for the current foreground so ignore list / min FPS / layout take
    /// effect immediately.
    fn reconfigure(&mut self, cfg: Config) {
        // Before the swap, so apps removed from `mute` are unmuted.
        self.mute_release();
        self.overlay.set_config(&cfg);
        self.cfg = cfg;
        self.hide();
        self.anchor_mon = HMONITOR::default();
        self.stats = Stats::default();
        self.label = (0, None);
        self.last_poll = 0;
        self.keys_armed = false;
        self.gate = GameGate::new();
        let fg = self.fg_hwnd;
        self.fg_hwnd = HWND::default();
        self.foreground(fg);
        self.sync_mute();
    }

    /// Create or drop `MuteState` per `mute_wanted`, reconciling in between.
    /// With `mute` empty and no `muted.txt`, nothing (COM, audio, WTS) is touched.
    fn sync_mute(&mut self) {
        if self.mute.is_none() {
            let stale = config::load_owned();
            if !mute_wanted(self.cfg.mute.is_empty(), stale.is_empty()) || !com_init() {
                return;
            }
            let Ok(mixer) = audio::WasapiMixer::new() else {
                return;
            };
            let muter = mute::Muter::with_stale(stale);
            let saved = muter.persisted();
            // SAFETY: our own window; unregistered in `drop_mute`. Failure only
            // loses the unlock reapply.
            let _ = unsafe { WTSRegisterSessionNotification(self.hwnd, NOTIFY_FOR_THIS_SESSION) };
            self.mute = Some(MuteState {
                mixer,
                muter,
                saved,
            });
        }
        self.mute_reconcile();
        if let Some(m) = &self.mute
            && !mute_wanted(self.cfg.mute.is_empty(), m.muter.persisted().is_empty())
        {
            self.drop_mute();
        }
    }

    fn mute_reconcile(&mut self) {
        if let Some(m) = self.mute.as_mut() {
            m.muter
                .reconcile(&mut m.mixer, &self.cfg.mute, self.fg_exe.as_deref());
            m.save();
        }
    }

    fn mute_release(&mut self) {
        if let Some(m) = self.mute.as_mut() {
            m.muter.release(&mut m.mixer);
            m.save();
        }
    }

    /// `WM_AUDIO`: a device change re-registers session notifications first.
    fn on_audio(&mut self, kind: usize) {
        if let Some(m) = self.mute.as_mut()
            && kind == audio::NOTIFY_DEVICE
        {
            let _ = m.mixer.refresh_endpoints();
        }
        self.mute_reconcile();
    }

    /// Release, save, and free the audio side. Also the exit path: must run
    /// before `App` drops so nothing we muted stays muted.
    fn drop_mute(&mut self) {
        if self.mute.is_none() {
            return;
        }
        self.mute_release();
        // SAFETY: our own window and timer; both calls are no-ops if not active.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), MUTE_TIMER_ID);
            let _ = WTSUnRegisterSessionNotification(self.hwnd);
        }
        self.mute = None;
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
        let mut low = None;
        let sample = (at != 0).then(|| {
            let pid = SHARED.sample_pid.load(Relaxed);
            let fps = f32::from_bits(SHARED.fps_bits.load(Relaxed));
            let bits = SHARED.low_bits.load(Relaxed);
            low = (bits != 0).then(|| f32::from_bits(bits));
            (pid, fps, ms_between(self.freq, qpc, at))
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
        // Hidden by toggle, or windowed while `fullscreen_only`: no poll, no draw.
        if !self.visible || (self.cfg.fullscreen_only && !is_fullscreen(self.fg_hwnd)) {
            self.hide();
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
            self.stats.cpu_mhz = s.cpu_mhz;
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
        self.stats.fps_low = out.fps.and(low);

        let anchor = self.fg_hwnd;
        let (mon, dpi) = monitor_of(anchor);
        let frame = layout::build(&self.stats, &self.cfg);
        let want = needs_draw(
            self.shown,
            mon != self.anchor_mon || dpi != self.anchor_dpi,
            self.frame.as_ref() != Some(&frame),
        );
        let r = if want {
            self.overlay.draw(&frame, anchor)
        } else {
            Ok(())
        };
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

/// True when `client` (screen coords) covers all of `mon`. Exclusive and borderless
/// fullscreen both pass; a maximized window fails because its title bar is outside
/// the client area. ponytail: exact match; a borderless game that leaves a 1 px gap
/// for the taskbar fails. Add a tolerance if one shows up.
fn covers_monitor(client: RECT, mon: RECT) -> bool {
    client.left <= mon.left
        && client.top <= mon.top
        && client.right >= mon.right
        && client.bottom >= mon.bottom
}

fn is_fullscreen(hwnd: HWND) -> bool {
    let mut c = RECT::default();
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: read-only queries with valid out-params; a stale hwnd just fails.
    unsafe {
        if GetClientRect(hwnd, &mut c).is_err() {
            return false;
        }
        let mut tl = POINT {
            x: c.left,
            y: c.top,
        };
        if !ClientToScreen(hwnd, &mut tl).as_bool() {
            return false;
        }
        let screen = RECT {
            left: tl.x,
            top: tl.y,
            right: tl.x + c.right - c.left,
            bottom: tl.y + c.bottom - c.top,
        };
        let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        GetMonitorInfoW(mon, &mut mi).as_bool() && covers_monitor(screen, mi.rcMonitor)
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

/// Every exit path: unmute what we muted and save `muted.txt` (inside
/// `drop_mute`), then drop `App` (ETW stop, overlay, mixer).
fn shutdown(slot: &mut Option<App>) {
    if let Some(app) = slot.as_mut() {
        app.drop_mute();
    }
    drop(slot.take());
}

/// Init COM (STA) on the UI thread once, on first use of the mixer. S_OK and
/// S_FALSE need a matching `CoUninitialize`; RPC_E_CHANGED_MODE means some
/// other code already chose MTA, which the mixer works under too (no uninit).
fn com_init() -> bool {
    if COM_INIT.load(Relaxed) {
        return true;
    }
    // SAFETY: called on the UI thread only; balanced by `com_uninit`.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    if hr.is_ok() {
        COM_INIT.store(true, Relaxed);
    }
    hr.is_ok() || hr == RPC_E_CHANGED_MODE
}

/// After `App` (and so the mixer) dropped. No-op if COM was never initialised.
fn com_uninit() {
    if COM_INIT.swap(false, Relaxed) {
        // SAFETY: balances the successful CoInitializeEx in `com_init`, same thread.
        unsafe { CoUninitialize() };
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn show_error(text: &str) {
    let text = wide(text);
    let title = wide("FPS Overlay");
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
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
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
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

    update::cleanup();
    // Single instance: a second one's `Etw::start` would stop our live session
    // by name. Only the elevated process gets here, so the relaunching
    // non-elevated one never holds it. If creation itself fails, run anyway.
    // SAFETY: plain named-mutex creation; the handle stays open for the process
    // lifetime and is closed only before an update restart.
    let instance = unsafe { CreateMutexW(None, false, w!("Local\\FpsOverlayRs")) };
    // SAFETY: reads this thread's last error, set by CreateMutexW just above.
    if instance.is_ok() && unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return;
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
            show_error(&format!(
                "Overlay window failed: 0x{:08X}",
                e.code().0 as u32
            ));
            return;
        }
    };
    let Some(hwnd) = main_window() else {
        show_error("Overlay message window failed to create");
        return;
    };
    MAIN_HWND.store(hwnd.0 as usize, Relaxed);
    // SAFETY: plain registrations on our own window.
    unsafe {
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Relaxed);
        // We run elevated; UIPI drops messages from the non-elevated Explorer unless allowed.
        for m in [WM_TRAY, TASKBAR_CREATED.load(Relaxed)] {
            let _ = ChangeWindowMessageFilterEx(hwnd, m, MSGFLT_ALLOW, None);
        }
    }
    tray(hwnd, NIM_ADD);

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
        last_game: None,
        fg_exe: None,
        last_fg_exe: None,
        mute: None,
        last_poll: 0,
        keys: (false, false),
        keys_armed: false,
        freq,
        t0,
    };
    APP.with(|slot| *slot.borrow_mut() = Some(app));
    // One check per launch, off the UI thread; the thread ends right after.
    std::thread::spawn(|| {
        if let Some(tag) = update::check() {
            let p = Box::into_raw(Box::new(tag)) as usize;
            // SAFETY: MAIN_HWND was stored above; the box is reclaimed in WM_UPDATE.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(MAIN_HWND.load(Relaxed) as *mut c_void)),
                    WM_UPDATE,
                    WPARAM(0),
                    LPARAM(p as isize),
                );
            }
        }
    });

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
        with_app(|app| {
            app.foreground(GetForegroundWindow());
            app.sync_mute();
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            // Tab / Enter / Esc navigation inside the settings window.
            if let Some(dlg) = settings::hwnd()
                && IsDialogMessageW(dlg, &msg).as_bool()
            {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    // Late COM audio callbacks must not post to a window that is going away.
    MAIN_HWND.store(0, Relaxed);
    // Explicit drop: unmutes, stops the ETW session and frees the overlay before exit.
    APP.with(|slot| shutdown(&mut slot.borrow_mut()));
    com_uninit();
    tray(hwnd, NIM_DELETE);

    let code = FATAL.load(Relaxed);
    if code != 0 {
        show_error(&format!("DXGI provider failed: 0x{code:08X}"));
    }
    if RESTART.load(Relaxed) {
        // Release the instance mutex first, or the new exe would exit at once.
        if let Ok(h) = instance {
            // SAFETY: our own handle from CreateMutexW, closed once.
            unsafe {
                let _ = CloseHandle(h);
            }
        }
        update::restart();
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
                style: CS_DBLCLKS,
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
            if wp.0 == MUTE_TIMER_ID {
                with_app(|app| {
                    // SAFETY: our own one-shot timer on our own window.
                    let _ = unsafe { KillTimer(Some(hwnd), MUTE_TIMER_ID) };
                    app.mute_reconcile();
                });
            } else {
                with_app(|app| app.tick());
            }
            LRESULT(0)
        }
        WM_AUDIO => {
            with_app(|app| app.on_audio(wp.0));
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            // Sent (not posted), so it can arrive while APP is borrowed: skip then;
            // the next foreground change reconciles anyway.
            if wp.0 as u32 == WTS_SESSION_UNLOCK {
                APP.with(|slot| {
                    if let Ok(mut slot) = slot.try_borrow_mut()
                        && let Some(app) = slot.as_mut()
                    {
                        app.mute_reconcile();
                    }
                });
            }
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONDBLCLK => open_settings(),
                WM_RBUTTONUP => tray_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Relaxed) => {
            tray(hwnd, NIM_ADD);
            LRESULT(0)
        }
        WM_UPDATE => {
            // SAFETY: lParam is the Box<String> leaked by the update thread, posted once.
            let tag = unsafe { Box::from_raw(lp.0 as *mut String) };
            prompt_update(hwnd, &tag);
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: posts to the message loop's thread.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_ENDSESSION => {
            // wParam FALSE means the shutdown was cancelled: keep running.
            if wp.0 != 0 {
                // Windows may end the process as soon as we return, so drop the
                // state here: unmute what we muted, and Etw::drop stops the
                // session instead of orphaning it.
                MAIN_HWND.store(0, Relaxed);
                APP.with(|slot| {
                    if let Ok(mut slot) = slot.try_borrow_mut() {
                        shutdown(&mut slot);
                        // Only once the mixer is gone; otherwise COM stays up.
                        com_uninit();
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

/// Ask, then install and quit; the loop exit restarts the new exe.
/// ponytail: the download blocks the UI thread (a few seconds; the overlay freezes).
/// Move `install` to a thread if that bites.
fn prompt_update(hwnd: HWND, tag: &str) {
    let q = wide(&format!(
        "FPS Overlay {tag} is available (you have v{}).\nInstall it now and restart?",
        env!("CARGO_PKG_VERSION")
    ));
    // SAFETY: NUL-terminated buffers outlive the modal calls; no APP borrow held.
    unsafe {
        if MessageBoxW(
            Some(hwnd),
            PCWSTR(q.as_ptr()),
            w!("FPS Overlay update"),
            MB_YESNO | MB_ICONQUESTION,
        ) != IDYES
        {
            return;
        }
        match update::install(tag) {
            Ok(()) => {
                RESTART.store(true, Relaxed);
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            Err(e) => {
                let t = wide(&format!("Update failed:\n{e}"));
                MessageBoxW(
                    Some(hwnd),
                    PCWSTR(t.as_ptr()),
                    w!("FPS Overlay"),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
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
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut c_void)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            );
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

    #[test]
    fn mute_menu_label_follows_listed() {
        assert_eq!(
            mute_menu_label("game.exe", false),
            "Mute game.exe in background"
        );
        assert_eq!(mute_menu_label("game.exe", true), "Stop muting game.exe");
    }

    #[test]
    fn mute_state_wanted_for_list_or_leftover_mutes() {
        assert!(!mute_wanted(true, true)); // idle: no COM at all
        assert!(mute_wanted(false, true));
        // List emptied but a mute (crash, failed unmute) is still to undo.
        assert!(mute_wanted(true, false));
        assert!(mute_wanted(false, false));
    }

    #[test]
    fn taskbar_click_does_not_replace_tray_candidate() {
        assert!(!tray_candidate("explorer.exe"));
        assert!(tray_candidate("game.exe"));
    }

    #[test]
    fn fullscreen_means_client_covers_monitor() {
        let r = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        let mon = r(0, 0, 1920, 1080);
        assert!(covers_monitor(r(0, 0, 1920, 1080), mon));
        // Exclusive modes may report a client larger than the desktop rect.
        assert!(covers_monitor(r(-1, -1, 1921, 1081), mon));
        // Maximized windowed: title bar (~23 px) is outside the client area.
        assert!(!covers_monitor(r(0, 23, 1920, 1032), mon));
        // Borderless but 1 px short (taskbar trick) does not count.
        assert!(!covers_monitor(r(0, 0, 1920, 1079), mon));
        // Second monitor left of the primary, at negative coordinates.
        let left = r(-2560, 0, 0, 1440);
        assert!(covers_monitor(r(-2560, 0, 0, 1440), left));
        assert!(!covers_monitor(r(0, 0, 1920, 1080), left));
    }
}
