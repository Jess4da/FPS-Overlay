//! Text layout: turns a `Stats` snapshot into the runs the renderer draws.
//!
//! Colors are the spec's 0–1 floats, scaled to bytes here.

use crate::config::{Config, Layout};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub color: Rgb,
    pub gap_before: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub runs: Vec<Run>,
    pub size: f32,
}

/// What the renderer draws. `font_px` is the base font px at scale 1.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub lines: Vec<Line>,
    pub font_px: f32,
    pub semibold: bool,
    pub padding: (f32, f32),
    pub background: bool,
    pub shadow: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub fps: Option<f32>,
    pub cpu: Option<f32>,
    pub gpu: Option<f32>,
    pub gpu_temp_c: Option<f32>,
    pub gpu_mhz: Option<f32>,
    pub vram_used_gb: Option<f32>,
    pub vram_total_gb: Option<f32>,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub label: Option<String>,
}

/// Spec color: 0–1 floats × 255, rounded.
fn rgb(r: f32, g: f32, b: f32) -> Rgb {
    Rgb(
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

const LABEL_GAP: f32 = 3.0;
const PART_GAP: f32 = 4.0;

fn label(text: &str, color: Rgb) -> Run {
    Run {
        text: text.to_string(),
        color,
        gap_before: 0.0,
    }
}

fn part(text: String, color: Rgb, gap_before: f32) -> Run {
    Run {
        text,
        color,
        gap_before,
    }
}

fn fps_color(fps: f32) -> Rgb {
    if fps >= 60.0 {
        rgb(0.18, 0.94, 0.45)
    } else if fps >= 30.0 {
        rgb(1.0, 0.85, 0.15)
    } else {
        rgb(1.0, 0.25, 0.25)
    }
}

fn load_color(pct: f32) -> Rgb {
    if pct > 90.0 {
        rgb(1.0, 0.3, 0.3)
    } else if pct > 70.0 {
        rgb(1.0, 0.85, 0.15)
    } else {
        rgb(0.70, 0.70, 0.75)
    }
}

/// Thresholds are always in °C, even when the value is displayed in °F.
fn temp_color(c: f32) -> Rgb {
    if c > 85.0 {
        rgb(1.0, 0.3, 0.3)
    } else if c > 70.0 {
        rgb(1.0, 0.85, 0.15)
    } else {
        rgb(0.70, 0.70, 0.75)
    }
}

fn no_data() -> Rgb {
    rgb(0.50, 0.50, 0.55)
}

fn temp_text(c: f32, fahrenheit: bool) -> String {
    if fahrenheit {
        format!("{:.0}°F", c * 9.0 / 5.0 + 32.0)
    } else {
        format!("{c:.0}°C")
    }
}

fn usage(used: f32, total: f32) -> f32 {
    if total > 0.0 { used / total * 100.0 } else { 0.0 }
}

/// `%.0f`, or `---` when there is no reading.
fn fps_text(s: &Stats) -> (String, Rgb) {
    match s.fps {
        Some(f) => (format!("{f:.0}"), fps_color(f)),
        None => ("---".to_string(), no_data()),
    }
}

pub fn build(s: &Stats, c: &Config) -> Frame {
    if c.layout == Layout::FpsOnly {
        let (text, color) = fps_text(s);
        return Frame {
            lines: vec![Line {
                runs: vec![part(text, color, 0.0)],
                size: 1.0,
            }],
            font_px: 22.0,
            semibold: true,
            padding: (4.0, 2.0),
            background: c.fps_only_background,
            shadow: true,
        };
    }

    let mut groups: Vec<Vec<Run>> = Vec::new();

    if c.show_fps {
        let (text, color) = fps_text(s);
        groups.push(vec![
            label("FPS", rgb(0.92, 0.52, 0.58)),
            part(text, color, LABEL_GAP),
        ]);
    }

    if c.show_cpu {
        let value = match s.cpu {
            Some(p) => part(format!("{p:.0}%"), load_color(p), LABEL_GAP),
            None => part("---".to_string(), no_data(), LABEL_GAP),
        };
        groups.push(vec![label("CPU", rgb(0.94, 0.88, 0.58)), value]);
    }

    if c.show_gpu {
        let value = match s.gpu {
            Some(p) => part(format!("{p:.0}%"), load_color(p), LABEL_GAP),
            None => part("N/A".to_string(), no_data(), LABEL_GAP),
        };
        let mut group = vec![label("GPU", rgb(0.52, 0.90, 0.70)), value];
        if let Some(t) = s.gpu_temp_c {
            group.push(part(temp_text(t, c.fahrenheit), temp_color(t), PART_GAP));
        }
        groups.push(group);
    }

    if c.show_gpu_clock
        && let Some(mhz) = s.gpu_mhz
    {
        groups.push(vec![part(
            format!("{mhz:.0} MHz"),
            rgb(0.70, 0.70, 0.75),
            0.0,
        )]);
    }

    if c.show_vram
        && let (Some(used), Some(total)) = (s.vram_used_gb, s.vram_total_gb)
    {
        let pct = usage(used, total);
        groups.push(vec![part(
            format!("VRAM {pct:.0}% {used:.1}/{total:.0}G"),
            load_color(pct),
            0.0,
        )]);
    }

    if c.show_ram {
        let pct = usage(s.ram_used_gb, s.ram_total_gb);
        groups.push(vec![part(
            format!(
                "RAM {pct:.0}% {:.1}/{:.0}G",
                s.ram_used_gb, s.ram_total_gb
            ),
            load_color(pct),
            0.0,
        )]);
    }

    // `" | "` only between drawn groups.
    let mut line1 = Vec::new();
    for (i, group) in groups.into_iter().enumerate() {
        if i > 0 {
            line1.push(part(" | ".to_string(), rgb(0.55, 0.55, 0.58), 0.0));
        }
        line1.extend(group);
    }

    let mut lines = vec![Line {
        runs: line1,
        size: 1.0,
    }];

    if c.show_process
        && let Some(l) = &s.label
    {
        lines.push(Line {
            runs: vec![part(l.clone(), rgb(0.42, 0.52, 0.42), 0.0)],
            size: 0.78,
        });
    }

    Frame {
        lines,
        font_px: 17.0,
        semibold: false,
        padding: (10.0, 6.0),
        background: true,
        shadow: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Layout};

    fn full() -> Stats {
        Stats {
            fps: Some(180.0),
            cpu: Some(15.0),
            gpu: Some(25.0),
            gpu_temp_c: Some(54.0),
            gpu_mhz: Some(2505.0),
            vram_used_gb: Some(2.0),
            vram_total_gb: Some(8.0),
            ram_used_gb: 12.8,
            ram_total_gb: 16.0,
            label: Some("RocketLeague.exe (Rocket League)".to_string()),
        }
    }

    fn text(line: &Line) -> String {
        line.runs.iter().map(|r| r.text.as_str()).collect()
    }

    /// Config with every group off except FPS, so line 1 is `[FPS label, value]`.
    fn fps_only_groups() -> Config {
        Config {
            show_cpu: false,
            show_gpu: false,
            show_gpu_clock: false,
            show_vram: false,
            show_ram: false,
            show_process: false,
            ..Config::default()
        }
    }

    #[test]
    fn full_bar_text() {
        let f = build(&full(), &Config::default());
        assert_eq!(f.lines.len(), 2);
        // Run texts carry their own spaces; gaps are pixels, not characters.
        assert_eq!(
            text(&f.lines[0]),
            "FPS180 | CPU15% | GPU25%54°C | 2505 MHz | VRAM 25% 2.0/8G | RAM 80% 12.8/16G"
        );
        assert_eq!(f.lines[0].size, 1.0);
        assert_eq!(f.lines[1].size, 0.78);
        assert_eq!(f.lines[1].runs[0].text, "RocketLeague.exe (Rocket League)");
        assert_eq!(f.lines[1].runs[0].color, Rgb(107, 133, 107));
        assert_eq!(f.font_px, 17.0);
        assert!(!f.semibold);
        assert_eq!(f.padding, (10.0, 6.0));
        assert!(f.background);
        assert!(!f.shadow);
    }

    #[test]
    fn fps_colors() {
        let c = fps_only_groups();
        for (fps, want) in [
            (60.0, Rgb(46, 240, 115)),
            (59.0, Rgb(255, 217, 38)),
            (29.0, Rgb(255, 64, 64)),
        ] {
            let f = build(
                &Stats {
                    fps: Some(fps),
                    ..Stats::default()
                },
                &c,
            );
            let runs = &f.lines[0].runs;
            assert_eq!(runs.len(), 2, "fps {fps}");
            assert_eq!(runs[0].text, "FPS");
            assert_eq!(runs[1].text, format!("{fps:.0}"), "fps {fps}");
            assert_eq!(runs[1].color, want, "fps {fps}");
            assert_eq!(runs[1].gap_before, 3.0);
        }
        let f = build(&Stats::default(), &c);
        assert_eq!(f.lines[0].runs[1].text, "---");
        assert_eq!(f.lines[0].runs[1].color, Rgb(128, 128, 140));
    }

    #[test]
    fn load_colors() {
        let c = Config {
            show_fps: false,
            show_gpu: false,
            show_gpu_clock: false,
            show_vram: false,
            show_ram: false,
            show_process: false,
            ..Config::default()
        };
        for (cpu, want) in [
            (91.0, Rgb(255, 77, 77)),
            (71.0, Rgb(255, 217, 38)),
            (70.0, Rgb(179, 179, 191)),
        ] {
            let f = build(
                &Stats {
                    cpu: Some(cpu),
                    ..Stats::default()
                },
                &c,
            );
            let runs = &f.lines[0].runs;
            assert_eq!(runs.len(), 2, "cpu {cpu}");
            assert_eq!(runs[0].text, "CPU");
            assert_eq!(runs[1].text, format!("{cpu:.0}%"));
            assert_eq!(runs[1].color, want, "cpu {cpu}");
        }
    }

    #[test]
    fn fahrenheit() {
        let mut c = Config {
            show_fps: false,
            show_cpu: false,
            show_gpu_clock: false,
            show_vram: false,
            show_ram: false,
            show_process: false,
            ..Config::default()
        };
        let s = Stats {
            gpu: Some(25.0),
            gpu_temp_c: Some(54.0),
            ..Stats::default()
        };
        assert!(text(&build(&s, &c).lines[0]).contains("54°C"));
        c.fahrenheit = true;
        assert!(text(&build(&s, &c).lines[0]).contains("129°F"));

        // Thresholds stay in °C even when the unit shown is °F.
        let hot = Stats {
            gpu: Some(25.0),
            gpu_temp_c: Some(86.0),
            ..Stats::default()
        };
        for unit in [false, true] {
            c.fahrenheit = unit;
            let f = build(&hot, &c);
            let temp = f.lines[0].runs.last().unwrap();
            assert_eq!(temp.color, Rgb(255, 77, 77), "fahrenheit {unit}");
        }
    }

    #[test]
    fn gpu_none_renders_na() {
        let s = Stats {
            fps: Some(180.0),
            cpu: Some(15.0),
            gpu: None,
            gpu_temp_c: None,
            gpu_mhz: None,
            vram_used_gb: None,
            vram_total_gb: None,
            ram_used_gb: 12.8,
            ram_total_gb: 16.0,
            label: None,
        };
        let f = build(&s, &Config::default());
        assert_eq!(f.lines.len(), 1);
        let t = text(&f.lines[0]);
        assert!(t.contains("GPU"), "{t}");
        assert!(t.contains("N/A"), "{t}");
        assert!(!t.contains("MHz"), "{t}");
        assert!(!t.contains("VRAM"), "{t}");
    }

    #[test]
    fn disabled_groups_no_dangling_separator() {
        let c = Config {
            show_fps: false,
            show_cpu: false,
            show_gpu: false,
            show_gpu_clock: false,
            show_vram: false,
            show_process: false,
            ..Config::default()
        };
        let f = build(&full(), &c);
        assert_eq!(f.lines[0].runs.len(), 1);
        assert_eq!(text(&f.lines[0]), "RAM 80% 12.8/16G");
    }

    #[test]
    fn fps_only_single_run() {
        let mut c = Config {
            layout: Layout::FpsOnly,
            ..Config::default()
        };
        let f = build(&full(), &c);
        assert_eq!(f.lines.len(), 1);
        assert_eq!(f.lines[0].runs.len(), 1);
        let r = &f.lines[0].runs[0];
        assert_eq!(r.text, "180");
        assert_eq!(r.color, Rgb(46, 240, 115));
        assert_eq!(f.font_px, 22.0);
        assert!(f.semibold);
        assert!(f.shadow);
        assert!(!f.background);
        assert_eq!(f.padding, (4.0, 2.0));

        c.fps_only_background = true;
        assert!(build(&full(), &c).background);
    }

    #[test]
    fn fps_only_none() {
        let c = Config {
            layout: Layout::FpsOnly,
            ..Config::default()
        };
        let f = build(&Stats::default(), &c);
        assert_eq!(f.lines[0].runs[0].text, "---");
        assert_eq!(f.lines[0].runs[0].color, Rgb(128, 128, 140));
    }
}
