//! Settings window: edits every `config.ini` key, validates, saves and hands the
//! new `Config` back to the app so it applies without a restart. Plain Win32
//! controls (visual styles via the manifest in build.rs); created on demand and
//! fully destroyed on close, so it costs nothing while hidden.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
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
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowTextLengthW,
    GetWindowTextW, HICON, HMENU, ICON_BIG, ICON_SMALL, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR,
    LoadCursorW, LoadImageW, MB_ICONERROR, MB_ICONWARNING, MB_OK, MESSAGEBOX_STYLE, MessageBoxW,
    NONCLIENTMETRICSW, RegisterClassW, SPI_GETNONCLIENTMETRICS, SW_SHOWNORMAL, SendMessageW,
    SetForegroundWindow, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND,
    WM_DESTROY, WM_SETFONT, WM_SETICON, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_EX_CLIENTEDGE,
    WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

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
/// Checkboxes, in `Fields::checks` order.
const ID_CHECKS: i32 = 110;
const CHECKS: [&str; 9] = [
    "Background in FPS-only layout",
    "FPS",
    "CPU usage",
    "GPU usage / temp",
    "GPU clock",
    "VRAM",
    "RAM",
    "Process name",
    "Fahrenheit",
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
    /// fps_only_background, fps, cpu, gpu, gpu_clock, vram, ram, process, fahrenheit
    pub checks: [bool; 9],
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
    let [fps_bg, fps, cpu, gpu, gpu_clock, vram, ram, process, fahrenheit] = f.checks;
    Ok(Config {
        layout: if f.layout == 1 { Layout::FpsOnly } else { Layout::Steam },
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
        show_cpu: cpu,
        show_gpu: gpu,
        show_gpu_clock: gpu_clock,
        show_vram: vram,
        show_ram: ram,
        show_process: process,
        fahrenheit,
        min_fps: min_fps as f32,
        ignore: config::ignore_list(f.ignore),
        toggle_key: f.toggle_key,
        exit_key: f.exit_key,
    })
}

struct Ui {
    hwnd: HWND,
    font: HFONT,
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
        let (cw, ch) = (px(400), px(498));
        let mut r = RECT { left: 0, top: 0, right: cw, bottom: ch };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let (x, y) = (wa.left + (wa.right - wa.left - ww) / 2, wa.top + (wa.bottom - wa.top - wh) / 2);

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

        let mut ncm = NONCLIENTMETRICSW { cbSize: size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut _),
            0,
            dpi,
        );
        let font = CreateFontIndirectW(&ncm.lfMessageFont);
        build(hwnd, font, cfg, &px);

        for (kind, metric) in [(ICON_BIG, 32), (ICON_SMALL, 16)] {
            if let Ok(h) = LoadImageW(Some(hinst), PCWSTR(1 as _), IMAGE_ICON, px(metric), px(metric), LR_DEFAULTCOLOR) {
                SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(HICON(h.0).0 as isize)));
            }
        }

        UI.with(|u| *u.borrow_mut() = Some(Ui { hwnd, font, on_save }));
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Creates and fills every control. `px` maps 96-DPI units to pixels.
unsafe fn build(hwnd: HWND, font: HFONT, c: &Config, px: &dyn Fn(i32) -> i32) {
    let ctl = |class: PCWSTR, text: &str, style: u32, ex: WINDOW_EX_STYLE, x: i32, y: i32, w: i32, h: i32, id: i32| {
        let text = crate::wide(text);
        // SAFETY: child of our window; text buffer outlives the call.
        unsafe {
            let h = CreateWindowExW(
                ex,
                class,
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
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
            SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            h
        }
    };
    let label = |text: &str, x: i32, y: i32, w: i32| ctl(w!("STATIC"), text, 0, WINDOW_EX_STYLE(0), x, y + 3, w, 20, -1);
    let edit = |text: &str, x: i32, y: i32, w: i32, id: i32, number: bool| {
        let st = WS_TABSTOP.0 | ES_AUTOHSCROLL | if number { ES_NUMBER } else { 0 };
        ctl(w!("EDIT"), text, st, WS_EX_CLIENTEDGE, x, y, w, 23, id)
    };
    let combo = |x: i32, y: i32, w: i32, id: i32, items: &[(String, usize)], sel: usize| {
        let h = ctl(w!("COMBOBOX"), "", WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST, WINDOW_EX_STYLE(0), x, y, w, 300, id);
        for (text, data) in items {
            let t = crate::wide(text);
            // SAFETY: our combo; string copied by the control.
            unsafe {
                let i = SendMessageW(h, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
                SendMessageW(h, CB_SETITEMDATA, Some(WPARAM(i.0 as usize)), Some(LPARAM(*data as isize)));
            }
        }
        // SAFETY: our combo.
        unsafe { SendMessageW(h, CB_SETCURSEL, Some(WPARAM(sel)), None) };
    };
    let check = |text: &str, x: i32, y: i32, w: i32, id: i32, on: bool| {
        let h = ctl(w!("BUTTON"), text, WS_TABSTOP.0 | BS_AUTOCHECKBOX, WINDOW_EX_STYLE(0), x, y, w, 22, id);
        // SAFETY: our checkbox.
        unsafe { SendMessageW(h, BM_SETCHECK, Some(WPARAM(on as usize)), None) };
    };
    let group = |text: &str, y: i32, h: i32| ctl(w!("BUTTON"), text, BS_GROUPBOX, WINDOW_EX_STYLE(0), 10, y, 380, h, -1);
    let list = |names: &[&str]| names.iter().enumerate().map(|(i, s)| (s.to_string(), i)).collect::<Vec<_>>();

    // Appearance
    group("Appearance", 8, 176);
    label("Layout", 24, 32, 150);
    combo(180, 30, 196, ID_LAYOUT, &list(&LAYOUTS), (c.layout == Layout::FpsOnly) as usize);
    label("Position", 24, 62, 150);
    let pos = match c.position {
        Corner::TopLeft => 0,
        Corner::TopRight => 1,
        Corner::BottomLeft => 2,
        Corner::BottomRight => 3,
    };
    combo(180, 60, 196, ID_POSITION, &list(&POSITIONS), pos);
    label("Scale (%)", 24, 92, 150);
    edit(&((c.scale * 100.0).round() as u32).to_string(), 180, 90, 80, ID_SCALE, true);
    label("Background opacity (%)", 24, 122, 150);
    edit(&((c.background_opacity * 100.0).round() as u32).to_string(), 180, 120, 80, ID_OPACITY, true);
    check(CHECKS[0], 24, 150, 352, ID_CHECKS, c.fps_only_background);

    // Steam-like bar contents
    group("Show in the Steam-like bar", 192, 104);
    let on = [c.show_fps, c.show_cpu, c.show_gpu, c.show_gpu_clock, c.show_vram, c.show_ram, c.show_process, c.fahrenheit];
    for (i, &v) in on.iter().enumerate() {
        let (col, row) = ((i % 2) as i32, (i / 2) as i32);
        check(CHECKS[i + 1], 24 + col * 180, 212 + row * 20, 170, ID_CHECKS + 1 + i as i32, v);
    }

    // Game detection
    group("Game detection", 304, 92);
    label("Minimum FPS to count as a game", 24, 326, 190);
    edit(&(c.min_fps.round() as u32).to_string(), 220, 324, 60, ID_MIN_FPS, true);
    label("Never show for (exe names, comma separated)", 24, 350, 340);
    edit(&c.ignore.join(", "), 24, 368, 352, ID_IGNORE, false);

    // Hotkeys (in game only)
    group("Hotkeys (work only while a game is in front)", 404, 54);
    let keys = |cur: u8| {
        let mut items: Vec<(String, usize)> = KEYS.iter().map(|(n, v)| (n.to_string(), *v as usize)).collect();
        if !KEYS.iter().any(|(_, v)| *v == cur) {
            items.push((format!("Key code {cur}"), cur as usize));
        }
        let sel = items.iter().position(|(_, v)| *v == cur as usize).unwrap_or(0);
        (items, sel)
    };
    label("Show/hide", 24, 426, 70);
    let (items, sel) = keys(c.toggle_key);
    combo(96, 424, 100, ID_TOGGLE, &items, sel);
    label("Exit", 214, 426, 40);
    let (items, sel) = keys(c.exit_key);
    combo(256, 424, 120, ID_EXIT, &items, sel);

    ctl(w!("BUTTON"), "Save", WS_TABSTOP.0 | BS_DEFPUSHBUTTON, WINDOW_EX_STYLE(0), 214, 466, 84, 26, IDOK);
    ctl(w!("BUTTON"), "Cancel", WS_TABSTOP.0, WINDOW_EX_STYLE(0), 306, 466, 84, 26, IDCANCEL);
}

unsafe fn item(hwnd: HWND, id: i32) -> HWND {
    // SAFETY: child lookup on our window.
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetDlgItem(Some(hwnd), id).unwrap_or_default() }
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
        SendMessageW(h, CB_GETITEMDATA, Some(WPARAM(i as usize)), None).0.max(0) as usize
    }
}

unsafe fn save(hwnd: HWND) {
    // SAFETY: reads our own controls.
    let (scale, opacity, min_fps, ignore, checks, layout, position, toggle, exit) = unsafe {
        let mut checks = [false; 9];
        for (i, c) in checks.iter_mut().enumerate() {
            *c = SendMessageW(item(hwnd, ID_CHECKS + i as i32), BM_GETCHECK, None, None).0 == 1;
        }
        (
            text(hwnd, ID_SCALE),
            text(hwnd, ID_OPACITY),
            text(hwnd, ID_MIN_FPS),
            text(hwnd, ID_IGNORE),
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
        checks,
        toggle_key: toggle,
        exit_key: exit,
    };
    let cfg = match from_fields(&fields) {
        Ok(c) => c,
        Err(msg) => return message(hwnd, &msg, MB_ICONWARNING),
    };
    if let Err(e) = config::save(&cfg) {
        return message(hwnd, &format!("Could not write config.ini:\n{e}"), MB_ICONERROR);
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
        MessageBoxW(Some(hwnd), PCWSTR(t.as_ptr()), w!("FPS Overlay"), MB_OK | icon);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            match (wp.0 & 0xFFFF) as i32 {
                // SAFETY: called on our window from its own message.
                IDOK => unsafe { save(hwnd) },
                IDCANCEL => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                _ => {}
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
            checks: [true, true, false, true, false, true, false, true, true],
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
        assert!(c.fps_only_background && c.show_fps && !c.show_cpu && c.show_gpu);
        assert!(!c.show_gpu_clock && c.show_vram && !c.show_ram && c.show_process && c.fahrenheit);
        assert_eq!((c.toggle_key, c.exit_key), (0x70, 0x7B));
        // Saved form must survive the ini round trip unchanged.
        assert_eq!(config::parse(&config::to_ini(&c)), c);
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
    fn same_hotkey_rejected() {
        assert!(from_fields(&fields("100", 0x2D, 0x2D)).is_err());
        assert!(from_fields(&fields("100", 0, 0x2D)).is_err());
    }
}
