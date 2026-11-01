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

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub layout: Layout,
    pub fps_only_background: bool,
    pub position: Corner,
    pub scale: f32,
    pub background_opacity: f32,
    pub show_fps: bool,
    pub show_cpu: bool,
    pub show_gpu: bool,
    pub show_gpu_clock: bool,
    pub show_vram: bool,
    pub show_ram: bool,
    pub show_process: bool,
    pub fahrenheit: bool,
    pub min_fps: f32,
    pub ignore: Vec<String>,
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
show_cpu=1
show_gpu=1
show_gpu_clock=1
show_vram=1
show_ram=1
show_process=1
fahrenheit=0
min_fps=20
ignore=explorer.exe,dwm.exe,chrome.exe,msedge.exe,firefox.exe,discord.exe
toggle_key=45
exit_key=35
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
            show_cpu: true,
            show_gpu: true,
            show_gpu_clock: true,
            show_vram: true,
            show_ram: true,
            show_process: true,
            fahrenheit: false,
            min_fps: 20.0,
            ignore: ["explorer.exe", "dwm.exe", "chrome.exe", "msedge.exe", "firefox.exe", "discord.exe"]
                .map(String::from)
                .to_vec(),
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

pub fn load() -> Config {
    let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.ini")))
    else {
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
        "show_cpu" => flag(&mut c.show_cpu, &v),
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
        "ignore" => {
            c.ignore = v
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        }
        "toggle_key" => key_code(&v, &mut c.toggle_key),
        "exit_key" => key_code(&v, &mut c.exit_key),
        _ => {}
    }
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
        assert_eq!(parse("layout=fps-only\nlayout=bogus").layout, Layout::FpsOnly);
    }

    #[test]
    fn bom_prefixed_first_key() {
        assert_eq!(parse("\u{feff}layout=fps-only").layout, Layout::FpsOnly);
        assert_eq!(parse("\u{feff}[Overlay]\nlayout=fps-only").layout, Layout::FpsOnly);
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
        let c = parse(
            "scale=999\nbackground_opacity=-1\nmin_fps=abc\ntoggle_key=0\nposition=middle",
        );
        let d = Config::default();
        assert_eq!(c.scale, d.scale);
        assert_eq!(c.background_opacity, d.background_opacity);
        assert_eq!(c.min_fps, d.min_fps);
        assert_eq!(c.toggle_key, d.toggle_key);
        assert_eq!(c.position, d.position);
        // A later bad value must not reset an earlier valid one.
        assert_eq!(parse("position=top-right\nposition=middle").position, Corner::TopRight);
        assert_eq!(parse("scale=150\nscale=999").scale, 1.5);
    }

    #[test]
    fn other_section_ignored() {
        assert_eq!(parse("[Other]\nscale=150").scale, 1.0);
    }

    #[test]
    fn default_ini_roundtrips() {
        assert_eq!(parse(DEFAULT_INI), Config::default());
    }
}
