//! Settings window: edits every `config.ini` key, validates, saves and hands the
//! new `Config` back to the app so it applies without a restart. Plain Win32
//! controls (visual styles via the manifest in build.rs); created on demand and
//! fully destroyed on close, so it costs nothing while hidden.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, CreateFontIndirectW, DeleteObject, GetMonitorInfoW, HBRUSH, HFONT, HGDIOBJ,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SystemParametersInfoForDpi,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    EnumWindows, GWL_EXSTYLE, GetCursorPos, GetDlgCtrlID, GetWindowLongW, GetWindowRect,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HICON, HMENU, ICON_BIG,
    ICON_SMALL, IDC_ARROW, IMAGE_ICON, IsWindowVisible, LR_DEFAULTCOLOR, LoadCursorW, LoadImageW,
    MB_ICONERROR, MB_ICONWARNING, MB_OK, MESSAGEBOX_STYLE, MF_STRING, MessageBoxW,
    NONCLIENTMETRICSW, RegisterClassW, SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW, SW_SHOWNORMAL,
    SendMessageW, SetForegroundWindow, SetWindowTextW, ShowWindow, TPM_RETURNCMD, TPM_RIGHTALIGN,
    TrackPopupMenu, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_HSCROLL,
    WM_NOTIFY, WM_SETFONT, WM_SETICON, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_EX_CONTROLPARENT, WS_EX_TOOLWINDOW, WS_MINIMIZEBOX, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::config::{self, Config, Corner, Layout};

// Raw control constants (avoids pulling the Controls feature for a few values).
const BS_AUTOCHECKBOX: u32 = 0x3;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_GROUPBOX: u32 = 0x7;
const CBS_DROPDOWNLIST: u32 = 0x3;
const ES_AUTOHSCROLL: u32 = 0x80;
const ES_NUMBER: u32 = 0x2000;
const BM_GETCHECK: u32 = 0xF0;
const BM_SETCHECK: u32 = 0xF1;
const CB_ADDSTRING: u32 = 0x143;
const CB_GETCURSEL: u32 = 0x147;
const CB_GETITEMDATA: u32 = 0x150;
const CB_SETCURSEL: u32 = 0x14E;
const CB_SETITEMDATA: u32 = 0x151;
/// Tab control (`SysTabControl32`), redirected to the comctl32 v6 assembly by
/// the manifest in build.rs.
const TCM_INSERTITEMW: u32 = 0x133E;
const TCM_GETCURSEL: u32 = 0x130B;
const TCIF_TEXT: u32 = 0x1;
/// `NMHDR.code` values are negative; compare as i32.
const TCN_SELCHANGE: i32 = -551;
/// Trackbar (`msctls_trackbar32`), same v6 redirection.
const TBM_GETPOS: u32 = 0x400;
const TBM_SETPOS: u32 = 0x405;
const TBM_SETRANGEMIN: u32 = 0x407;
const TBM_SETRANGEMAX: u32 = 0x408;
const TBM_SETPAGESIZE: u32 = 0x415;
/// `WM_COMMAND` notification code in the high word.
const EN_CHANGE: u32 = 0x300;

const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;
const ID_LAYOUT: i32 = 100;
const ID_POSITION: i32 = 101;
const ID_SCALE: i32 = 102;
const ID_OPACITY: i32 = 103;
const ID_MIN_FPS: i32 = 104;
const ID_IGNORE: i32 = 105;
const ID_TOGGLE: i32 = 106;
const ID_EXIT: i32 = 107;
/// Not in `CHECKS`: state lives in Task Scheduler, not `Config`.
const ID_AUTOSTART: i32 = 108;
const ID_ADD_IGNORE: i32 = 109;
/// Background muting: the exe list and its "Add…" button.
const ID_MUTE: i32 = 130;
const ID_ADD_MUTE: i32 = 131;
/// Checkboxes, in `Fields::checks` order.
const ID_CHECKS: i32 = 110;
/// Tab control and the sliders beside the number boxes.
const ID_TABS: i32 = 140;
const ID_SCALE_SLIDER: i32 = 141;
const ID_OPACITY_SLIDER: i32 = 142;
const ID_MIN_FPS_SLIDER: i32 = 143;

/// Tab pages, in `TCM_INSERTITEMW` order; `OVERLAY` is the one shown first.
const OVERLAY: usize = 0;
const MUTE: usize = 1;
const TABS: [&str; 2] = ["Overlay", "Mute"];
/// Client-space origin of every tab page's controls.
const PAGE_X: i32 = 14;
const PAGE_Y: i32 = 34;

/// Number boxes with a slider beside them: (box id, slider id, min, max). The box
/// keeps its own wider validation; the slider only ever shows the clamped value.
const SLIDERS: [(i32, i32, u32, u32); 3] = [
    (ID_SCALE, ID_SCALE_SLIDER, 50, 200),
    (ID_OPACITY, ID_OPACITY_SLIDER, 0, 100),
    (ID_MIN_FPS, ID_MIN_FPS_SLIDER, 1, 240),
];
const CHECKS: [&str; 12] = [
    "Background in FPS-only layout",
    "FPS",
    "1% low",
    "CPU usage",
    "CPU clock",
    "GPU usage / temp",
    "GPU clock",
    "VRAM",
    "RAM",
    "Process name",
    "Fahrenheit",
    "Only show in fullscreen / borderless",
];

const LAYOUTS: [&str; 2] = ["Steam-like bar", "FPS only"];
const POSITIONS: [&str; 4] = ["Top left", "Top right", "Bottom left", "Bottom right"];
const KEYS: [(&str, u8); 20] = [
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("Home", 0x24),
    ("End", 0x23),
    ("Page Up", 0x21),
    ("Page Down", 0x22),
    ("Pause", 0x13),
    ("Scroll Lock", 0x91),
    ("F1", 0x70),
    ("F2", 0x71),
    ("F3", 0x72),
    ("F4", 0x73),
    ("F5", 0x74),
    ("F6", 0x75),
    ("F7", 0x76),
    ("F8", 0x77),
    ("F9", 0x78),
    ("F10", 0x79),
    ("F11", 0x7A),
    ("F12", 0x7B),
];

/// Raw control values, validated by [`from_fields`].
pub struct Fields<'a> {
    pub layout: usize,
    pub position: usize,
    pub scale: &'a str,
    pub opacity: &'a str,
    pub min_fps: &'a str,
    pub ignore: &'a str,
    pub mute: &'a str,
    /// fps_only_background, fps, low, cpu, cpu_clock, gpu, gpu_clock, vram, ram, process,
    /// fahrenheit, fullscreen_only
    pub checks: [bool; 12],
    pub toggle_key: u8,
    pub exit_key: u8,
}

/// Validates the form; the error names the field so the user can fix it.
pub fn from_fields(f: &Fields) -> Result<Config, String> {
    let int = |name: &str, v: &str, lo: u32, hi: u32| match v.trim().parse::<u32>() {
        Ok(n) if (lo..=hi).contains(&n) => Ok(n),
        _ => Err(format!("{name} must be a whole number from {lo} to {hi}.")),
    };
    let scale = int("Scale", f.scale, 50, 200)?;
    let opacity = int("Background opacity", f.opacity, 0, 100)?;
    let min_fps = int("Minimum FPS", f.min_fps, 1, 1000)?;
    if f.toggle_key == 0 || f.exit_key == 0 {
        return Err("Pick a key for both hotkeys.".into());
    }
    if f.toggle_key == f.exit_key {
        return Err("The show/hide key and the exit key must be different.".into());
    }
    let [
        fps_bg,
        fps,
        low,
        cpu,
        cpu_clock,
        gpu,
        gpu_clock,
        vram,
        ram,
        process,
        fahrenheit,
        fullscreen_only,
    ] = f.checks;
    Ok(Config {
        layout: if f.layout == 1 {
            Layout::FpsOnly
        } else {
            Layout::Steam
        },
        fps_only_background: fps_bg,
        position: match f.position {
            1 => Corner::TopRight,
            2 => Corner::BottomLeft,
            3 => Corner::BottomRight,
            _ => Corner::TopLeft,
        },
        scale: scale as f32 / 100.0,
        background_opacity: opacity as f32 / 100.0,
        show_fps: fps,
        show_low: low,
        show_cpu: cpu,
        show_cpu_clock: cpu_clock,
        show_gpu: gpu,
        show_gpu_clock: gpu_clock,
        show_vram: vram,
        show_ram: ram,
        show_process: process,
        fahrenheit,
        min_fps: min_fps as f32,
        fullscreen_only,
        ignore: config::ignore_list(f.ignore),
        mute: config::ignore_list(f.mute),
        toggle_key: f.toggle_key,
        exit_key: f.exit_key,
        mute_key: 77,
        mute_mod: config::Modifier::Alt,
    })
}

/// Slider position for a box's text: `None` when it is not a number (the slider
/// keeps its last position), else clamped to the slider's own range.
fn slider_pos(text: &str, lo: u32, hi: u32) -> Option<u32> {
    text.trim().parse::<u32>().ok().map(|n| n.clamp(lo, hi))
}

/// Start of `WM_NOTIFY`'s payload (same layout as the SDK's `NMHDR`, which needs
/// the Controls feature); only these three fields are read.
#[repr(C)]
struct Nmhdr {
    hwnd_from: HWND,
    id_from: usize,
    code: u32,
}

struct Ui {
    hwnd: HWND,
    font: HFONT,
    /// Controls of each tab page, shown/hidden on `TCN_SELCHANGE`.
    pages: [Vec<HWND>; 2],
    on_save: fn(Config),
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// The open settings window, for `IsDialogMessageW` (Tab/Enter/Esc) in the loop.
pub fn hwnd() -> Option<HWND> {
    UI.with(|u| u.borrow().as_ref().map(|u| u.hwnd))
}

/// Opens the window (or focuses it if already open). `on_save` gets the saved config.
pub fn open(cfg: &Config, on_save: fn(Config)) {
    if let Some(h) = hwnd() {
        // SAFETY: our own live window.
        unsafe {
            let _ = ShowWindow(h, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    // SAFETY: plain window/control creation on this (UI) thread; every handle is
    // owned by the window and released in WM_DESTROY.
    unsafe {
        let class = w!("FpsOverlaySettings");
        let hinst = GetModuleHandleW(None).unwrap_or_default().into();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as _),
            ..Default::default()
        };
        // Fails harmlessly with "class already exists" on the second open.
        RegisterClassW(&wc);

        // Open on the monitor the user clicked the tray on, at that monitor's DPI.
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let (mut dpi, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dpi, &mut dy);
        let dpi = dpi.max(96);
        let s = dpi as f32 / 96.0;
        let px = |v: i32| (v as f32 * s).round() as i32;

        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let ex = WS_EX_CONTROLPARENT;
        let (cw, ch) = (px(400), px(588));
        let mut r = RECT {
            left: 0,
            top: 0,
            right: cw,
            bottom: ch,
        };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let (x, y) = (
            wa.left + (wa.right - wa.left - ww) / 2,
            wa.top + (wa.bottom - wa.top - wh) / 2,
        );

        let Ok(hwnd) = CreateWindowExW(
            ex,
            class,
            w!("FPS Overlay Settings"),
            style,
            x,
            y,
            ww,
            wh,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };

        let mut ncm = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            0,
            dpi,
        );
        let font = CreateFontIndirectW(&ncm.lfMessageFont);
        let pages = build(hwnd, font, cfg, &px);

        for (kind, metric) in [(ICON_BIG, 32), (ICON_SMALL, 16)] {
            if let Ok(h) = LoadImageW(
                Some(hinst),
                PCWSTR(1 as _),
                IMAGE_ICON,
                px(metric),
                px(metric),
                LR_DEFAULTCOLOR,
            ) {
                SendMessageW(
                    hwnd,
                    WM_SETICON,
                    Some(WPARAM(kind as usize)),
                    Some(LPARAM(HICON(h.0).0 as isize)),
                );
            }
        }

        UI.with(|u| {
            *u.borrow_mut() = Some(Ui {
                hwnd,
                font,
                pages,
                on_save,
            })
        });
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// `TCITEMW` for `TCM_INSERTITEMW`; only `pszText` is used. The state fields
/// precede the text (v6 layout, which the manifest redirects the class to).
#[repr(C)]
struct TcItemW {
    mask: u32,
    state: u32,
    state_mask: u32,
    text: *const u16,
    text_max: i32,
    image: i32,
    lparam: isize,
}

/// Creates and fills every control. `px` maps 96-DPI units to pixels. Returns
/// each tab page's controls, shown/hidden on `TCN_SELCHANGE`.
unsafe fn build(hwnd: HWND, font: HFONT, c: &Config, px: &dyn Fn(i32) -> i32) -> [Vec<HWND>; 2] {
    // Page the following controls belong to; `None` is the shared bottom row.
    let cur: Cell<Option<usize>> = Cell::new(None);
    let pages: RefCell<[Vec<HWND>; 2]> = RefCell::new([Vec::new(), Vec::new()]);
    let page = |tab: usize| cur.set(Some(tab));

    let ctl = |class: PCWSTR,
               text: &str,
               style: u32,
               ex: WINDOW_EX_STYLE,
               x: i32,
               y: i32,
               w: i32,
               h: i32,
               id: i32| {
        let text = crate::wide(text);
        let tab = cur.get();
        let (x, y) = if tab.is_some() {
            (x + PAGE_X, y + PAGE_Y)
        } else {
            (x, y)
        };
        // Overlay is the initially selected page, so only Mute starts hidden.
        let shown = tab.is_none_or(|t| t == OVERLAY);
        // SAFETY: child of our window; text buffer outlives the call.
        unsafe {
            let h = CreateWindowExW(
                ex,
                class,
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | style | if shown { WS_VISIBLE.0 } else { 0 }),
                px(x),
                px(y),
                px(w),
                px(h),
                Some(hwnd),
                Some(HMENU(id as _)),
                None,
                None,
            )
            .unwrap_or_default();
            if let Some(t) = tab {
                pages.borrow_mut()[t].push(h);
            }
            SendMessageW(
                h,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            h
        }
    };
    let label = |text: &str, x: i32, y: i32, w: i32| {
        ctl(
            w!("STATIC"),
            text,
            0,
            WINDOW_EX_STYLE(0),
            x,
            y + 3,
            w,
            20,
            -1,
        )
    };
    let edit = |text: &str, x: i32, y: i32, w: i32, id: i32, number: bool| {
        let st = WS_TABSTOP.0 | ES_AUTOHSCROLL | if number { ES_NUMBER } else { 0 };
        ctl(w!("EDIT"), text, st, WS_EX_CLIENTEDGE, x, y, w, 23, id)
    };
    let combo = |x: i32, y: i32, w: i32, id: i32, items: &[(String, usize)], sel: usize| {
        let h = ctl(
            w!("COMBOBOX"),
            "",
            WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST,
            WINDOW_EX_STYLE(0),
            x,
            y,
            w,
            300,
            id,
        );
        for (text, data) in items {
            let t = crate::wide(text);
            // SAFETY: our combo; string copied by the control.
            unsafe {
                let i = SendMessageW(h, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
                SendMessageW(
                    h,
                    CB_SETITEMDATA,
                    Some(WPARAM(i.0 as usize)),
                    Some(LPARAM(*data as isize)),
                );
            }
        }
        // SAFETY: our combo.
        unsafe { SendMessageW(h, CB_SETCURSEL, Some(WPARAM(sel)), None) };
    };
    let check = |text: &str, x: i32, y: i32, w: i32, id: i32, on: bool| {
        let h = ctl(
            w!("BUTTON"),
            text,
            WS_TABSTOP.0 | BS_AUTOCHECKBOX,
            WINDOW_EX_STYLE(0),
            x,
            y,
            w,
            22,
            id,
        );
        // SAFETY: our checkbox.
        unsafe { SendMessageW(h, BM_SETCHECK, Some(WPARAM(on as usize)), None) };
    };
    let group = |text: &str, y: i32, h: i32| {
        ctl(
            w!("BUTTON"),
            text,
            BS_GROUPBOX,
            WINDOW_EX_STYLE(0),
            0,
            y,
            372,
            h,
            -1,
        )
    };
    // Trackbar beside a number box; starts at the box's own (clamped) value.
    let slider = |x: i32, y: i32, id: i32, lo: u32, hi: u32, text: &str| {
        let h = ctl(
            w!("msctls_trackbar32"),
            "",
            WS_TABSTOP.0,
            WINDOW_EX_STYLE(0),
            x,
            y,
            100,
            24,
            id,
        );
        // SAFETY: our trackbar; the TBM_* setters do not notify the parent.
        unsafe {
            SendMessageW(
                h,
                TBM_SETRANGEMIN,
                Some(WPARAM(1)),
                Some(LPARAM(lo as isize)),
            );
            SendMessageW(
                h,
                TBM_SETRANGEMAX,
                Some(WPARAM(1)),
                Some(LPARAM(hi as isize)),
            );
            SendMessageW(h, TBM_SETPAGESIZE, None, Some(LPARAM(10)));
            if let Some(p) = slider_pos(text, lo, hi) {
                SendMessageW(h, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(p as isize)));
            }
        }
    };
    let list = |names: &[&str]| {
        names
            .iter()
            .enumerate()
            .map(|(i, s)| (s.to_string(), i))
            .collect::<Vec<_>>()
    };

    // Tabs first, so the page controls created after it paint above its body.
    let tabs = ctl(
        w!("SysTabControl32"),
        "",
        WS_TABSTOP.0,
        WINDOW_EX_STYLE(0),
        10,
        8,
        380,
        542,
        ID_TABS,
    );
    for (i, name) in TABS.iter().enumerate() {
        let t = crate::wide(name);
        let item = TcItemW {
            mask: TCIF_TEXT,
            state: 0,
            state_mask: 0,
            text: t.as_ptr(),
            text_max: 0,
            image: 0,
            lparam: 0,
        };
        // SAFETY: our tab control; the item and its text outlive the call.
        unsafe {
            SendMessageW(
                tabs,
                TCM_INSERTITEMW,
                Some(WPARAM(i)),
                Some(LPARAM(&item as *const _ as isize)),
            );
        }
    }

    // Overlay page
    page(OVERLAY);
    group("Appearance", 0, 176);
    label("Layout", 14, 24, 130);
    combo(
        170,
        22,
        196,
        ID_LAYOUT,
        &list(&LAYOUTS),
        (c.layout == Layout::FpsOnly) as usize,
    );
    label("Position", 14, 54, 130);
    let pos = match c.position {
        Corner::TopLeft => 0,
        Corner::TopRight => 1,
        Corner::BottomLeft => 2,
        Corner::BottomRight => 3,
    };
    combo(170, 52, 196, ID_POSITION, &list(&POSITIONS), pos);
    let scale = ((c.scale * 100.0).round() as u32).to_string();
    label("Scale (%)", 14, 84, 130);
    slider(150, 80, ID_SCALE_SLIDER, 50, 200, &scale);
    edit(&scale, 256, 82, 60, ID_SCALE, true);
    let opacity = ((c.background_opacity * 100.0).round() as u32).to_string();
    label("Background opacity (%)", 14, 114, 130);
    slider(150, 110, ID_OPACITY_SLIDER, 0, 100, &opacity);
    edit(&opacity, 256, 112, 60, ID_OPACITY, true);
    check(CHECKS[0], 14, 142, 352, ID_CHECKS, c.fps_only_background);

    // Steam-like bar contents
    group("Show in the Steam-like bar", 192, 124);
    let on = [
        c.show_fps,
        c.show_low,
        c.show_cpu,
        c.show_cpu_clock,
        c.show_gpu,
        c.show_gpu_clock,
        c.show_vram,
        c.show_ram,
        c.show_process,
        c.fahrenheit,
    ];
    for (i, &v) in on.iter().enumerate() {
        let (col, row) = ((i % 2) as i32, (i / 2) as i32);
        check(
            CHECKS[i + 1],
            14 + col * 180,
            20 + row * 20,
            170,
            ID_CHECKS + 1 + i as i32,
            v,
        );
    }

    // Game detection
    group("Game detection", 324, 124);
    let min_fps = (c.min_fps.round() as u32).to_string();
    label("Minimum FPS", 14, 22, 130);
    slider(150, 18, ID_MIN_FPS_SLIDER, 1, 240, &min_fps);
    edit(&min_fps, 256, 20, 60, ID_MIN_FPS, true);
    label("Never show for (exe names, comma separated)", 14, 50, 340);
    edit(&c.ignore.join(", "), 14, 68, 268, ID_IGNORE, false);
    ctl(
        w!("BUTTON"),
        "Add…",
        WS_TABSTOP.0,
        WINDOW_EX_STYLE(0),
        288,
        67,
        78,
        25,
        ID_ADD_IGNORE,
    );
    check(CHECKS[11], 14, 98, 352, ID_CHECKS + 11, c.fullscreen_only);

    // Hotkeys (in game only)
    group("Hotkeys (work only while a game is in front)", 456, 54);
    let keys = |key: u8| {
        let mut items: Vec<(String, usize)> = KEYS
            .iter()
            .map(|(n, v)| (n.to_string(), *v as usize))
            .collect();
        if !KEYS.iter().any(|(_, v)| *v == key) {
            items.push((format!("Key code {key}"), key as usize));
        }
        let sel = items
            .iter()
            .position(|(_, v)| *v == key as usize)
            .unwrap_or(0);
        (items, sel)
    };
    label("Show/hide", 14, 22, 70);
    let (items, sel) = keys(c.toggle_key);
    combo(86, 20, 100, ID_TOGGLE, &items, sel);
    label("Exit", 204, 22, 40);
    let (items, sel) = keys(c.exit_key);
    combo(246, 20, 120, ID_EXIT, &items, sel);

    // Mute page (Task 4 adds the hotkey row below this list)
    page(MUTE);
    group("Background muting", 0, 200);
    label(
        "Mute while not focused (exe names, comma separated)",
        14,
        22,
        340,
    );
    edit(&c.mute.join(", "), 14, 40, 268, ID_MUTE, false);
    ctl(
        w!("BUTTON"),
        "Add…",
        WS_TABSTOP.0,
        WINDOW_EX_STYLE(0),
        288,
        39,
        78,
        25,
        ID_ADD_MUTE,
    );

    // Shared bottom row, below the tab control and on no page.
    cur.set(None);
    check(
        "Start with Windows",
        14,
        558,
        190,
        ID_AUTOSTART,
        crate::autostart::enabled(),
    );
    ctl(
        w!("BUTTON"),
        "Save",
        WS_TABSTOP.0 | BS_DEFPUSHBUTTON,
        WINDOW_EX_STYLE(0),
        214,
        556,
        84,
        26,
        IDOK,
    );
    ctl(
        w!("BUTTON"),
        "Cancel",
        WS_TABSTOP.0,
        WINDOW_EX_STYLE(0),
        306,
        556,
        84,
        26,
        IDCANCEL,
    );

    pages.into_inner()
}

unsafe fn item(hwnd: HWND, id: i32) -> HWND {
    // SAFETY: child lookup on our window.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetDlgItem(Some(hwnd), id).unwrap_or_default()
    }
}

unsafe fn text(hwnd: HWND, id: i32) -> String {
    // SAFETY: buffer sized from the control's own length.
    unsafe {
        let h = item(hwnd, id);
        let mut buf = vec![0u16; GetWindowTextLengthW(h) as usize + 1];
        let n = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

unsafe fn combo_value(hwnd: HWND, id: i32) -> usize {
    // SAFETY: our combo; CB_ERR (-1) maps to data 0 / index 0 below.
    unsafe {
        let h = item(hwnd, id);
        let i = SendMessageW(h, CB_GETCURSEL, None, None).0;
        if i < 0 {
            return 0;
        }
        SendMessageW(h, CB_GETITEMDATA, Some(WPARAM(i as usize)), None)
            .0
            .max(0) as usize
    }
}

unsafe fn save(hwnd: HWND) {
    // SAFETY: reads our own controls.
    let (scale, opacity, min_fps, ignore, mute, checks, layout, position, toggle, exit) = unsafe {
        let mut checks = [false; 12];
        for (i, c) in checks.iter_mut().enumerate() {
            *c = SendMessageW(item(hwnd, ID_CHECKS + i as i32), BM_GETCHECK, None, None).0 == 1;
        }
        (
            text(hwnd, ID_SCALE),
            text(hwnd, ID_OPACITY),
            text(hwnd, ID_MIN_FPS),
            text(hwnd, ID_IGNORE),
            text(hwnd, ID_MUTE),
            checks,
            combo_value(hwnd, ID_LAYOUT),
            combo_value(hwnd, ID_POSITION),
            combo_value(hwnd, ID_TOGGLE) as u8,
            combo_value(hwnd, ID_EXIT) as u8,
        )
    };
    let fields = Fields {
        layout,
        position,
        scale: &scale,
        opacity: &opacity,
        min_fps: &min_fps,
        ignore: &ignore,
        mute: &mute,
        checks,
        toggle_key: toggle,
        exit_key: exit,
    };
    let cfg = match from_fields(&fields) {
        Ok(c) => c,
        Err(msg) => return message(hwnd, &msg, MB_ICONWARNING),
    };
    if let Err(e) = config::save(&cfg) {
        return message(
            hwnd,
            &format!("Could not write config.ini:\n{e}"),
            MB_ICONERROR,
        );
    }
    // SAFETY: our checkbox.
    let auto = unsafe { SendMessageW(item(hwnd, ID_AUTOSTART), BM_GETCHECK, None, None).0 == 1 };
    if auto != crate::autostart::enabled()
        && let Err(e) = crate::autostart::set(auto)
    {
        return message(
            hwnd,
            &format!("Could not change Start with Windows:\n{e}"),
            MB_ICONERROR,
        );
    }
    // Copy the callback out: the borrow must be released before it runs and
    // before DestroyWindow re-enters WM_DESTROY.
    let on_save = UI.with(|u| u.borrow().as_ref().map(|u| u.on_save));
    if let Some(f) = on_save {
        f(cfg);
    }
    // SAFETY: our own window.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

fn message(hwnd: HWND, text: &str, icon: MESSAGEBOX_STYLE) {
    let t = crate::wide(text);
    // SAFETY: NUL-terminated buffers that outlive the modal call.
    unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR(t.as_ptr()),
            w!("FPS Overlay"),
            MB_OK | icon,
        );
    }
}

/// Running windowed apps not yet in the list, as `(exe, label)`, sorted by label,
/// one per exe. Input is one `(exe, label)` per top-level window.
fn candidates(windows: Vec<(String, String)>, excluded: &[String]) -> Vec<(String, String)> {
    let mut v: Vec<_> = windows
        .into_iter()
        .filter(|(e, _)| !excluded.contains(e))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v.dedup_by(|a, b| a.0 == b.0);
    v.sort_by_key(|(_, l)| l.to_ascii_lowercase());
    v
}

unsafe extern "system" fn collect(hwnd: HWND, lp: LPARAM) -> BOOL {
    // SAFETY: lp is the &mut Vec passed by pick_into, alive for EnumWindows.
    let out = unsafe { &mut *(lp.0 as *mut Vec<(String, String)>) };
    // SAFETY: plain queries on a window handle supplied by EnumWindows.
    unsafe {
        let tool = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0;
        // Cloaked = hidden by DWM (suspended UWP hosts, other virtual desktops).
        let mut cloaked = 0u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            &mut cloaked as *mut _ as *mut _,
            size_of::<u32>() as u32,
        );
        if !IsWindowVisible(hwnd).as_bool()
            || tool
            || cloaked != 0
            || GetWindowTextLengthW(hwnd) == 0
        {
            return true.into();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid != 0
            && pid != std::process::id()
            && let Some(exe) = crate::process::exe_name(pid)
        {
            let label = crate::process::label(pid).unwrap_or_else(|| exe.clone());
            out.push((exe, label));
        }
    }
    true.into()
}

/// "Add…" for a comma list box: popup menu of running windowed apps not already
/// listed in `edit_id`; the pick is appended to that box (applied on Save like
/// any other edit). `button_id` anchors the menu.
unsafe fn pick_into(hwnd: HWND, edit_id: i32, button_id: i32) {
    let mut found: Vec<(String, String)> = Vec::new();
    // SAFETY: callback only runs during this call, while `found` is alive.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&mut found as *mut _ as isize)) };
    // SAFETY: reads our own edit box.
    let current = unsafe { text(hwnd, edit_id) };
    let mut list = config::ignore_list(&current);
    let items = candidates(found, &list);
    if items.is_empty() {
        return message(
            hwnd,
            "No other apps with a window are running.",
            MB_ICONWARNING,
        );
    }
    // SAFETY: menu created, shown modally and destroyed in this scope; labels
    // outlive TrackPopupMenu.
    let cmd = unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let labels: Vec<Vec<u16>> = items.iter().map(|(_, l)| crate::wide(l)).collect();
        for (i, l) in labels.iter().enumerate() {
            let _ = AppendMenuW(menu, MF_STRING, i + 1, PCWSTR(l.as_ptr()));
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(item(hwnd, button_id), &mut r);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTALIGN,
            r.right,
            r.bottom,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        cmd.0 as usize
    };
    let Some((exe, _)) = cmd.checked_sub(1).and_then(|i| items.get(i)) else {
        return;
    };
    config::add_to_list(&mut list, exe);
    let t = crate::wide(&list.join(", "));
    // SAFETY: our edit box; buffer outlives the call.
    unsafe {
        let _ = SetWindowTextW(item(hwnd, edit_id), PCWSTR(t.as_ptr()));
    }
}

/// Shows tab page `sel` and hides the other one; hidden controls are skipped by
/// `IsDialogMessageW`'s Tab handling.
fn show_page(sel: usize) {
    let (mut show, mut hide) = (Vec::new(), Vec::new());
    UI.with(|u| {
        if let Some(u) = u.borrow().as_ref() {
            for (i, page) in u.pages.iter().enumerate() {
                let out = if i == sel { &mut show } else { &mut hide };
                out.extend_from_slice(page);
            }
        }
    });
    // SAFETY: page controls of our own live window.
    unsafe {
        for h in hide {
            let _ = ShowWindow(h, SW_HIDE);
        }
        for h in show {
            let _ = ShowWindow(h, SW_SHOW);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wp.0 & 0xFFFF) as i32;
            if (wp.0 >> 16) as u32 == EN_CHANGE {
                // A number box changed: follow it with its slider, ignoring text
                // that is not a number. TBM_SETPOS does not notify, so no loop.
                // SAFETY: reads our own box and moves its own slider.
                unsafe {
                    if let Some(&(_, bar, lo, hi)) = SLIDERS.iter().find(|s| s.0 == id)
                        && let Some(p) = slider_pos(&text(hwnd, id), lo, hi)
                    {
                        SendMessageW(
                            item(hwnd, bar),
                            TBM_SETPOS,
                            Some(WPARAM(1)),
                            Some(LPARAM(p as isize)),
                        );
                    }
                }
                return LRESULT(0);
            }
            match id {
                // SAFETY: called on our window from its own message.
                IDOK => unsafe { save(hwnd) },
                IDCANCEL => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                ID_ADD_IGNORE => unsafe { pick_into(hwnd, ID_IGNORE, ID_ADD_IGNORE) },
                ID_ADD_MUTE => unsafe { pick_into(hwnd, ID_MUTE, ID_ADD_MUTE) },
                _ => {}
            }
            LRESULT(0)
        }
        WM_HSCROLL => {
            // A trackbar moved: show its position in the paired number box.
            // SAFETY: lParam is the trackbar that sent this; we only set text.
            unsafe {
                let bar = HWND(lp.0 as _);
                if let Some(&(box_id, ..)) = SLIDERS.iter().find(|s| s.1 == GetDlgCtrlID(bar)) {
                    let t = crate::wide(&SendMessageW(bar, TBM_GETPOS, None, None).0.to_string());
                    let _ = SetWindowTextW(item(hwnd, box_id), PCWSTR(t.as_ptr()));
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY's lParam starts with an NMHDR valid for this call.
            let (from, code) = unsafe {
                let nm = &*(lp.0 as *const Nmhdr);
                (nm.id_from, nm.code as i32)
            };
            if from == ID_TABS as usize && code == TCN_SELCHANGE {
                // SAFETY: our own tab control and page controls.
                let sel = unsafe { SendMessageW(item(hwnd, ID_TABS), TCM_GETCURSEL, None, None).0 };
                show_page(sel.max(0) as usize);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: our own window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(ui) = UI.with(|u| u.borrow_mut().take()) {
                // SAFETY: font created in open(); controls are already gone.
                unsafe {
                    let _ = DeleteObject(HGDIOBJ(ui.font.0));
                }
            }
            // The window pulled in comctl32 and fonts; give the memory back.
            // SAFETY: own process handle.
            unsafe {
                let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
            }
            LRESULT(0)
        }
        // SAFETY: default handling for our window.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields<'a>(scale: &'a str, toggle: u8, exit: u8) -> Fields<'a> {
        Fields {
            layout: 1,
            position: 3,
            scale,
            opacity: "40",
            min_fps: "30",
            ignore: " Foo.EXE ,, bar.exe",
            mute: " Game.EXE, ,x.exe",
            checks: [
                true, true, true, false, true, true, false, true, false, true, true, true,
            ],
            toggle_key: toggle,
            exit_key: exit,
        }
    }

    #[test]
    fn valid_form_maps_every_field() {
        let c = from_fields(&fields("150", 0x70, 0x7B)).unwrap();
        assert_eq!(c.layout, Layout::FpsOnly);
        assert_eq!(c.position, Corner::BottomRight);
        assert_eq!((c.scale, c.background_opacity, c.min_fps), (1.5, 0.4, 30.0));
        assert_eq!(c.ignore, ["foo.exe", "bar.exe"]);
        assert!(
            c.fps_only_background && c.show_fps && c.show_low && !c.show_cpu && c.show_cpu_clock
        );
        assert!(c.show_gpu);
        assert!(!c.show_gpu_clock && c.show_vram && !c.show_ram && c.show_process && c.fahrenheit);
        assert!(c.fullscreen_only);
        assert_eq!((c.toggle_key, c.exit_key), (0x70, 0x7B));
        // Saved form must survive the ini round trip unchanged.
        assert_eq!(config::parse(&config::to_ini(&c)), c);
    }

    #[test]
    fn from_fields_normalizes_mute_list() {
        let c = from_fields(&fields("100", 0x70, 0x7B)).unwrap();
        assert_eq!(c.mute, ["game.exe", "x.exe"]);
    }

    #[test]
    fn out_of_range_names_the_field() {
        for bad in ["49", "201", "", "1.5", "abc"] {
            let e = from_fields(&fields(bad, 0x70, 0x7B)).unwrap_err();
            assert!(e.starts_with("Scale"), "{bad}: {e}");
        }
        assert!(from_fields(&fields("50", 0x70, 0x7B)).is_ok());
        assert!(from_fields(&fields("200", 0x70, 0x7B)).is_ok());
    }

    #[test]
    fn candidates_drop_ignored_and_duplicate_exes_sorted_by_label() {
        let s = |e: &str, l: &str| (e.to_string(), l.to_string());
        let got = candidates(
            vec![
                s("game.exe", "Game.exe (Zeta)"),
                s("chrome.exe", "chrome.exe"),
                s("app.exe", "App.exe (alpha)"),
                s("game.exe", "Game.exe (Zeta)"),
            ],
            &["chrome.exe".to_string()],
        );
        assert_eq!(
            got,
            [
                s("app.exe", "App.exe (alpha)"),
                s("game.exe", "Game.exe (Zeta)")
            ]
        );
    }

    #[test]
    fn slider_pos_clamps_and_ignores_garbage() {
        assert_eq!(slider_pos("500", 1, 240), Some(240));
        assert_eq!(slider_pos(" 60 ", 1, 240), Some(60));
        assert_eq!(slider_pos("0", 1, 240), Some(1));
        assert_eq!(slider_pos("", 1, 240), None);
        assert_eq!(slider_pos("abc", 50, 200), None);
    }

    #[test]
    fn same_hotkey_rejected() {
        assert!(from_fields(&fields("100", 0x2D, 0x2D)).is_err());
        assert!(from_fields(&fields("100", 0, 0x2D)).is_err());
    }
}
