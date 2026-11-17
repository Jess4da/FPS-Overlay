use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    Steam,
    FpsOnly,
}

/// Modifier half of the mute chord. `vk` is the `GetAsyncKeyState` code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Modifier {
    None,
    Alt,
    Ctrl,
    Shift,
}

impl Modifier {
    #[allow(dead_code)] // Read by the mute-chord poll; lands with the hotkey work.
    pub fn vk(self) -> Option<u8> {
        match self {
            Modifier::None => None,
            Modifier::Alt => Some(0x12),
            Modifier::Ctrl => Some(0x11),
            Modifier::Shift => Some(0x10),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub layout: Layout,
    pub fps_only_background: bool,
    pub position: Corner,
    pub scale: f32,
    pub background_opacity: f32,
    pub show_fps: bool,
    pub show_low: bool,
    pub show_cpu: bool,
    pub show_cpu_clock: bool,
    pub show_gpu: bool,
    pub show_gpu_clock: bool,
    pub show_vram: bool,
    pub show_ram: bool,
    pub show_process: bool,
    pub fahrenheit: bool,
    pub min_fps: f32,
    /// Show only while the game's client area covers its whole monitor.
    pub fullscreen_only: bool,
    pub ignore: Vec<String>,
    /// Exes to mute while not in the foreground (user list).
    pub mute: Vec<String>,
    /// Mute chord: virtual-key code and modifier. `0` disables the chord.
    pub mute_key: u8,
    pub mute_mod: Modifier,
    pub toggle_key: u8,
    pub exit_key: u8,
}

pub const DEFAULT_INI: &str = "\
[Overlay]
layout=steam
fps_only_background=0
position=top-left
scale=100
background_opacity=100
show_fps=1
show_low=0
show_cpu=1
show_cpu_clock=1
show_gpu=1
show_gpu_clock=1
show_vram=1
show_ram=1
show_process=1
fahrenheit=0
min_fps=20
fullscreen_only=0
ignore=explorer.exe,dwm.exe,chrome.exe,msedge.exe,firefox.exe,discord.exe
mute=
toggle_key=45
exit_key=35
mute_key=77
mute_mod=alt
";

impl Default for Config {
    fn default() -> Self {
        Self {
            layout: Layout::Steam,
            fps_only_background: false,
            position: Corner::TopLeft,
            scale: 1.0,
            background_opacity: 1.0,
            show_fps: true,
            show_low: false,
            show_cpu: true,
            show_cpu_clock: true,
            show_gpu: true,
            show_gpu_clock: true,
            show_vram: true,
            show_ram: true,
            show_process: true,
            fahrenheit: false,
            min_fps: 20.0,
            fullscreen_only: false,
            ignore: [
                "explorer.exe",
                "dwm.exe",
                "chrome.exe",
                "msedge.exe",
                "firefox.exe",
                "discord.exe",
            ]
            .map(String::from)
            .to_vec(),
            mute: Vec::new(),
            mute_key: 77,
            mute_mod: Modifier::Alt,
            toggle_key: 45,
            exit_key: 35,
        }
    }
}

pub fn parse(ini: &str) -> Config {
    let ini = ini.strip_prefix('\u{feff}').unwrap_or(ini);
    let mut c = Config::default();
    // Keys before any header belong to [Overlay], so a header-less file still parses.
    let mut section = "overlay";
    for line in ini.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
            section = name.trim();
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        if section.eq_ignore_ascii_case("overlay") {
            set(&mut c, key.trim(), val.trim());
        }
    }
    c
}

fn path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.ini")))
}

/// `muted.txt` beside the exe: exes this app muted and must unmute on exit.
fn owned_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("muted.txt")))
}

/// One exe per line. Missing or unreadable reads as empty: a session must not
/// fail over this file.
fn load_owned_from(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| l.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default()
}

/// An empty set removes the file (nothing was muted); NotFound is success.
/// Otherwise the temp file goes in the same directory and is renamed over the
/// real one, so a crash can't leave half a list.
fn save_owned_to(p: &Path, exes: &BTreeSet<String>) -> std::io::Result<()> {
    if exes.is_empty() {
        return match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let text: String = exes.iter().map(|e| format!("{e}\n")).collect();
    let mut tmp = p.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p)
}

pub fn load_owned() -> Vec<String> {
    owned_path()
        .map(|p| load_owned_from(&p))
        .unwrap_or_default()
}

pub fn save_owned(exes: &BTreeSet<String>) -> std::io::Result<()> {
    let p = owned_path().ok_or_else(|| std::io::Error::other("no exe directory"))?;
    save_owned_to(&p, exes)
}

pub fn load() -> Config {
    let Some(path) = path() else {
        return Config::default();
    };
    match std::fs::read(&path) {
        Ok(bytes) => parse(&String::from_utf8_lossy(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let _ = std::fs::write(&path, DEFAULT_INI);
            Config::default()
        }
        // Any other error (permissions, sharing, I/O): never overwrite the user's file.
        Err(_) => Config::default(),
    }
}

/// Writes the whole `[Overlay]` section (comments in a hand-edited file are not kept).
pub fn save(c: &Config) -> std::io::Result<()> {
    let path = path().ok_or_else(|| std::io::Error::other("no exe directory"))?;
    std::fs::write(path, to_ini(c))
}

pub fn to_ini(c: &Config) -> String {
    let b = |v: bool| if v { 1 } else { 0 };
    let layout = match c.layout {
        Layout::Steam => "steam",
        Layout::FpsOnly => "fps-only",
    };
    let position = match c.position {
        Corner::TopLeft => "top-left",
        Corner::TopRight => "top-right",
        Corner::BottomLeft => "bottom-left",
        Corner::BottomRight => "bottom-right",
    };
    let mute_mod = match c.mute_mod {
        Modifier::None => "none",
        Modifier::Alt => "alt",
        Modifier::Ctrl => "ctrl",
        Modifier::Shift => "shift",
    };
    format!(
        "[Overlay]\nlayout={layout}\nfps_only_background={}\nposition={position}\nscale={}\n\
         background_opacity={}\nshow_fps={}\nshow_low={}\nshow_cpu={}\nshow_cpu_clock={}\nshow_gpu={}\nshow_gpu_clock={}\n\
         show_vram={}\nshow_ram={}\nshow_process={}\nfahrenheit={}\nmin_fps={}\nfullscreen_only={}\n\
         ignore={}\nmute={}\ntoggle_key={}\nexit_key={}\nmute_key={}\nmute_mod={mute_mod}\n",
        b(c.fps_only_background),
        (c.scale * 100.0).round() as u32,
        (c.background_opacity * 100.0).round() as u32,
        b(c.show_fps),
        b(c.show_low),
        b(c.show_cpu),
        b(c.show_cpu_clock),
        b(c.show_gpu),
        b(c.show_gpu_clock),
        b(c.show_vram),
        b(c.show_ram),
        b(c.show_process),
        b(c.fahrenheit),
        c.min_fps,
        b(c.fullscreen_only),
        c.ignore.join(","),
        c.mute.join(","),
        c.toggle_key,
        c.exit_key,
        c.mute_key,
    )
}

fn set(c: &mut Config, key: &str, val: &str) {
    let v = val.to_ascii_lowercase();
    match key.to_ascii_lowercase().as_str() {
        "layout" => match v.as_str() {
            "steam" => c.layout = Layout::Steam,
            "fps-only" => c.layout = Layout::FpsOnly,
            _ => {}
        },
        "fps_only_background" => flag(&mut c.fps_only_background, &v),
        "position" => match v.as_str() {
            "top-left" => c.position = Corner::TopLeft,
            "top-right" => c.position = Corner::TopRight,
            "bottom-left" => c.position = Corner::BottomLeft,
            "bottom-right" => c.position = Corner::BottomRight,
            _ => {}
        },
        "scale" => {
            if let Some(n) = num(&v, 50.0, 200.0) {
                c.scale = n / 100.0;
            }
        }
        "background_opacity" => {
            if let Some(n) = num(&v, 0.0, 100.0) {
                c.background_opacity = n / 100.0;
            }
        }
        "show_fps" => flag(&mut c.show_fps, &v),
        "show_low" => flag(&mut c.show_low, &v),
        "show_cpu" => flag(&mut c.show_cpu, &v),
        "show_cpu_clock" => flag(&mut c.show_cpu_clock, &v),
        "show_gpu" => flag(&mut c.show_gpu, &v),
        "show_gpu_clock" => flag(&mut c.show_gpu_clock, &v),
        "show_vram" => flag(&mut c.show_vram, &v),
        "show_ram" => flag(&mut c.show_ram, &v),
        "show_process" => flag(&mut c.show_process, &v),
        "fahrenheit" => flag(&mut c.fahrenheit, &v),
        "min_fps" => match v.parse::<f32>() {
            Ok(n) if (1.0..=1000.0).contains(&n) => c.min_fps = n,
            _ => {}
        },
        "fullscreen_only" => flag(&mut c.fullscreen_only, &v),
        "ignore" => c.ignore = ignore_list(&v),
        "mute" => c.mute = ignore_list(&v),
        "toggle_key" => key_code(&v, &mut c.toggle_key),
        "exit_key" => key_code(&v, &mut c.exit_key),
        // 0 disables the chord, so unlike `key_code` this accepts it.
        "mute_key" => {
            if let Ok(n @ 0..=254) = v.parse::<u32>() {
                c.mute_key = n as u8;
            }
        }
        "mute_mod" => match v.as_str() {
            "none" => c.mute_mod = Modifier::None,
            "alt" => c.mute_mod = Modifier::Alt,
            "ctrl" => c.mute_mod = Modifier::Ctrl,
            "shift" => c.mute_mod = Modifier::Shift,
            _ => {}
        },
        _ => {}
    }
}

/// Comma list of exe names, lowercased and trimmed.
pub fn ignore_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Adds `exe` (lowercased) unless already listed. False if it was already there.
pub fn add_to_list(list: &mut Vec<String>, exe: &str) -> bool {
    let exe = exe.trim().to_ascii_lowercase();
    if exe.is_empty() || list.contains(&exe) {
        return false;
    }
    list.push(exe);
    true
}

fn flag(dst: &mut bool, v: &str) {
    if let Ok(n @ 0..=1) = v.parse::<u32>() {
        *dst = n == 1;
    }
}

fn num(v: &str, min: f32, max: f32) -> Option<f32> {
    v.parse::<f32>().ok().filter(|n| (min..=max).contains(n))
}

fn key_code(v: &str, dst: &mut u8) {
    match v.parse::<u32>() {
        Ok(n) if (1..=254).contains(&n) => *dst = n as u8,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_ini_roundtrips_non_defaults() {
        let c = Config {
            layout: Layout::FpsOnly,
            fps_only_background: true,
            position: Corner::BottomRight,
            scale: 1.5,
            background_opacity: 0.4,
            show_low: true,
            show_cpu: false,
            show_cpu_clock: false,
            show_process: false,
            fahrenheit: true,
            min_fps: 30.0,
            fullscreen_only: true,
            ignore: vec!["foo.exe".into(), "bar.exe".into()],
            toggle_key: 112,
            exit_key: 113,
            mute_key: 0x4E,
            mute_mod: Modifier::None,
            ..Config::default()
        };
        assert_eq!(parse(&to_ini(&c)), c);
        assert_eq!(parse(&to_ini(&Config::default())), Config::default());
    }

    #[test]
    fn empty_is_default() {
        let c = parse("");
        assert_eq!(c, Config::default());
        assert_eq!(c.layout, Layout::Steam);
        assert!(!c.fps_only_background);
        assert_eq!(c.toggle_key, 45);
        assert_eq!(c.exit_key, 35);
        assert_eq!(c.min_fps, 20.0);
        assert!(c.ignore.contains(&"explorer.exe".to_string()));
    }

    #[test]
    fn parses_layout() {
        let c = parse("layout=fps-only\nfps_only_background=1");
        assert_eq!(c.layout, Layout::FpsOnly);
        assert!(c.fps_only_background);
        assert_eq!(parse("layout=bogus").layout, Layout::Steam);
        // A later bogus value must not reset an earlier valid one.
        assert_eq!(
            parse("layout=fps-only\nlayout=bogus").layout,
            Layout::FpsOnly
        );
    }

    #[test]
    fn bom_prefixed_first_key() {
        assert_eq!(parse("\u{feff}layout=fps-only").layout, Layout::FpsOnly);
        assert_eq!(
            parse("\u{feff}[Overlay]\nlayout=fps-only").layout,
            Layout::FpsOnly
        );
    }

    #[test]
    fn parses_all_keys() {
        let c = parse(
            "[Overlay]\nposition=bottom-right\nscale=150\nbackground_opacity=50\nshow_ram=0\nfahrenheit=1\nmin_fps=30\nignore=Foo.EXE, bar.exe\ntoggle_key=112",
        );
        assert_eq!(c.position, Corner::BottomRight);
        assert_eq!(c.scale, 1.5);
        assert_eq!(c.background_opacity, 0.5);
        assert!(!c.show_ram);
        assert!(c.fahrenheit);
        assert_eq!(c.min_fps, 30.0);
        assert_eq!(c.ignore, ["foo.exe", "bar.exe"]);
        assert_eq!(c.toggle_key, 112);
    }

    #[test]
    fn out_of_range_falls_back() {
        let c =
            parse("scale=999\nbackground_opacity=-1\nmin_fps=abc\ntoggle_key=0\nposition=middle");
        let d = Config::default();
        assert_eq!(c.scale, d.scale);
        assert_eq!(c.background_opacity, d.background_opacity);
        assert_eq!(c.min_fps, d.min_fps);
        assert_eq!(c.toggle_key, d.toggle_key);
        assert_eq!(c.position, d.position);
        // A later bad value must not reset an earlier valid one.
        assert_eq!(
            parse("position=top-right\nposition=middle").position,
            Corner::TopRight
        );
        assert_eq!(parse("scale=150\nscale=999").scale, 1.5);
    }

    #[test]
    fn other_section_ignored() {
        assert_eq!(parse("[Other]\nscale=150").scale, 1.0);
    }

    #[test]
    fn add_to_list_lowercases_and_skips_duplicates() {
        let mut l = vec!["foo.exe".to_string()];
        assert!(!add_to_list(&mut l, "FOO.exe"));
        assert!(add_to_list(&mut l, " Game.EXE "));
        assert!(!add_to_list(&mut l, ""));
        assert_eq!(l, ["foo.exe", "game.exe"]);
    }

    #[test]
    fn default_ini_roundtrips() {
        assert_eq!(parse(DEFAULT_INI), Config::default());
    }

    #[test]
    fn mute_round_trips_through_ini() {
        let c = Config {
            mute: vec!["a.exe".into(), "b.exe".into()],
            ..Config::default()
        };
        assert_eq!(parse(&to_ini(&c)), c);
    }

    #[test]
    fn mute_list_is_trimmed_lowercased_and_drops_empties() {
        let c = parse("mute= A.exe , ,b.EXE");
        assert_eq!(c.mute, ["a.exe", "b.exe"]);
    }

    #[test]
    fn show_low_missing_defaults_off() {
        assert!(!parse("[Overlay]\nshow_fps=1\n").show_low);
    }

    #[test]
    fn missing_mute_key_means_empty() {
        assert!(parse("[Overlay]\ntoggle_key=112").mute.is_empty());
    }

    #[test]
    fn mute_hotkey_defaults_to_alt_m() {
        let c = parse("");
        assert_eq!((c.mute_key, c.mute_mod), (77, Modifier::Alt));
    }

    #[test]
    fn mute_hotkey_round_trips_and_zero_disables() {
        let c = Config {
            mute_key: 0,
            mute_mod: Modifier::Ctrl,
            ..Config::default()
        };
        assert_eq!(parse(&to_ini(&c)), c);
        let c = parse("mute_key=120\nmute_mod=Shift");
        assert_eq!((c.mute_key, c.mute_mod), (120, Modifier::Shift));
        assert_eq!(parse("mute_mod=none").mute_mod, Modifier::None);
    }

    #[test]
    fn bad_mute_hotkey_values_fall_back() {
        let c = parse("mute_key=300\nmute_mod=win");
        assert_eq!((c.mute_key, c.mute_mod), (77, Modifier::Alt));
        assert_eq!(parse("mute_key=-1").mute_key, 77);
    }

    /// Unique scratch dir per test: pid plus a counter, like BGMuter's tests.
    fn tmp_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "fps-overlay-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn owned_file_round_trips_and_empty_set_deletes_it() {
        let d = tmp_dir();
        let p = d.join("muted.txt");
        let set = BTreeSet::from(["a.exe".to_string(), "b.exe".to_string()]);
        save_owned_to(&p, &set).unwrap();
        assert_eq!(load_owned_from(&p), ["a.exe", "b.exe"]);
        assert!(!d.join("muted.txt.tmp").exists());
        save_owned_to(&p, &BTreeSet::new()).unwrap();
        assert!(!p.exists());
        // Deleting an already-absent file is not an error.
        save_owned_to(&p, &BTreeSet::new()).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_owned_file_is_empty() {
        let d = tmp_dir();
        assert!(load_owned_from(&d.join("nope.txt")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }
}
