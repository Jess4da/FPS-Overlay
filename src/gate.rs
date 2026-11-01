//! Foreground game gate: probes candidate processes and qualifies them by FPS.

use crate::config::Config;
use crate::etw::Providers;

/// Probe while a candidate is unqualified, hot tick once it is a game.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Timer {
    Off,
    /// 250 ms.
    Probe,
    /// 100 ms.
    Game,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GateOut {
    pub timer: Timer,
    pub providers: Providers,
    pub visible: bool,
    pub fps: Option<f32>,
}

#[derive(Clone, Copy)]
pub struct GameGate {
    pid: Option<u32>,
    candidate: bool,
    qualified: bool,
    user_visible: bool,
    fg_since_ms: u64,
    providers: Providers,
}

impl GameGate {
    pub fn new() -> Self {
        Self {
            pid: None,
            candidate: false,
            qualified: false,
            user_visible: true,
            fg_since_ms: 0,
            providers: Providers::Off,
        }
    }

    pub fn on_foreground(&mut self, pid: Option<u32>, exe: Option<&str>, cfg: &Config, now_ms: u64) -> GateOut {
        // WinEvent can re-fire for the same window; keep the foreground period.
        // An earlier event without the exe name still needs re-evaluating.
        if pid.is_some() && pid == self.pid && (self.candidate || exe.is_none()) {
            return self.out(None);
        }
        self.pid = pid;
        self.qualified = false;
        self.fg_since_ms = now_ms;
        self.candidate = match (pid, exe) {
            (Some(p), Some(e)) => p != std::process::id() && !cfg.ignore.iter().any(|i| i.eq_ignore_ascii_case(e)),
            _ => false,
        };
        self.providers = if self.candidate { Providers::UserMode } else { Providers::Off };
        self.out(None)
    }

    pub fn on_tick(&mut self, sample: Option<(u32, f32, u64)>, saw_user_mode: bool, cfg: &Config, now_ms: u64) -> GateOut {
        if !self.candidate {
            return self.out(None);
        }
        // The sample must be fresh and taken after this pid became foreground.
        let fresh = sample.filter(|&(pid, _, age)| {
            Some(pid) == self.pid && age <= 2000 && now_ms.saturating_sub(age) >= self.fg_since_ms
        });
        if !self.qualified {
            if !saw_user_mode && now_ms.saturating_sub(self.fg_since_ms) >= 1500 {
                self.providers = Providers::WithKernel;
            }
            if fresh.is_some_and(|(_, fps, _)| fps >= cfg.min_fps) {
                self.qualified = true;
            }
        }
        self.out(if self.qualified { fresh.map(|(_, fps, _)| fps) } else { None })
    }

    pub fn toggle(&mut self) -> GateOut {
        self.user_visible = !self.user_visible;
        self.out(None)
    }

    pub fn is_game(&self) -> bool {
        self.qualified
    }

    fn out(&self, fps: Option<f32>) -> GateOut {
        if !self.candidate {
            return GateOut { timer: Timer::Off, providers: Providers::Off, visible: false, fps: None };
        }
        GateOut {
            timer: if self.qualified { Timer::Game } else { Timer::Probe },
            providers: self.providers,
            visible: self.qualified && self.user_visible,
            fps,
        }
    }
}

impl Default for GameGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: Option<u32> = Some(100);
    const EXE: Option<&str> = Some("game.exe");

    fn gate_at(cfg: &Config, t: u64) -> GameGate {
        let mut g = GameGate::new();
        g.on_foreground(GAME, EXE, cfg, t);
        g
    }

    #[test]
    fn ignored_exe_is_idle() {
        let cfg = Config::default();
        let mut g = GameGate::new();
        let out = g.on_foreground(Some(100), Some("explorer.exe"), &cfg, 0);
        assert_eq!(out.timer, Timer::Off);
        assert_eq!(out.providers, Providers::Off);
        assert!(!out.visible);
    }

    #[test]
    fn candidate_probes_user_mode() {
        let cfg = Config::default();
        let mut g = GameGate::new();
        let out = g.on_foreground(GAME, EXE, &cfg, 0);
        assert_eq!(out.timer, Timer::Probe);
        assert_eq!(out.providers, Providers::UserMode);
    }

    #[test]
    fn kernel_fallback_after_1500ms() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 1000);
        assert_eq!(g.on_tick(None, false, &cfg, 2499).providers, Providers::UserMode);
        assert_eq!(g.on_tick(None, false, &cfg, 2500).providers, Providers::WithKernel);
    }

    #[test]
    fn user_mode_events_block_fallback() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        assert_eq!(g.on_tick(None, true, &cfg, 5000).providers, Providers::UserMode);
    }

    #[test]
    fn qualifies_at_min_fps() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        let out = g.on_tick(Some((100, 20.0, 0)), true, &cfg, 250);
        assert_eq!(out.timer, Timer::Game);
        assert!(out.visible);
        assert_eq!(out.fps, Some(20.0));
        assert!(g.is_game());
    }

    #[test]
    fn below_min_not_game() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        let out = g.on_tick(Some((100, 19.0, 0)), true, &cfg, 250);
        assert_eq!(out.timer, Timer::Probe);
        assert!(!out.visible);
        assert!(!g.is_game());
    }

    #[test]
    fn stale_sample_does_not_qualify() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        let out = g.on_tick(Some((100, 60.0, 2500)), true, &cfg, 2600);
        assert!(!g.is_game());
        assert_eq!(out.timer, Timer::Probe);
        assert_eq!(out.fps, None);
    }

    #[test]
    fn pre_foreground_sample_does_not_qualify() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 1000);
        // Taken at 1600-1500 = 100, before the pid became foreground at 1000;
        // still within the 2000 ms freshness window, so only the foreground-time
        // guard rejects it.
        let out = g.on_tick(Some((100, 60.0, 1500)), true, &cfg, 1600);
        assert!(!g.is_game());
        assert_eq!(out.fps, None);
        let out = g.on_tick(Some((100, 60.0, 400)), true, &cfg, 2000);
        assert!(g.is_game());
        assert_eq!(out.fps, Some(60.0));
    }

    #[test]
    fn own_pid_not_candidate() {
        let cfg = Config::default();
        let mut g = GameGate::new();
        let out = g.on_foreground(Some(std::process::id()), EXE, &cfg, 0);
        assert_eq!(out.timer, Timer::Off);
        assert_eq!(out.providers, Providers::Off);
        assert!(!g.is_game());
    }

    #[test]
    fn exe_known_later_is_re_evaluated() {
        let cfg = Config::default();
        let mut g = GameGate::new();
        assert_eq!(g.on_foreground(GAME, None, &cfg, 0).timer, Timer::Off);
        let out = g.on_foreground(GAME, EXE, &cfg, 100);
        assert_eq!(out.timer, Timer::Probe);
        assert_eq!(out.providers, Providers::UserMode);
    }

    #[test]
    fn sticky_after_qualify() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        g.on_tick(Some((100, 60.0, 0)), true, &cfg, 100);
        let out = g.on_tick(None, true, &cfg, 600);
        assert!(out.visible);
        assert_eq!(out.timer, Timer::Game);
        assert_eq!(g.on_tick(Some((100, 60.0, 10)), true, &cfg, 700).fps, Some(60.0));
    }

    #[test]
    fn other_pid_sample_not_displayed() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        g.on_tick(Some((100, 60.0, 0)), true, &cfg, 100);
        let out = g.on_tick(Some((200, 45.0, 0)), true, &cfg, 200);
        assert_eq!(out.fps, None);
        assert!(out.visible);
    }

    #[test]
    fn stale_fps_is_none() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        g.on_tick(Some((100, 60.0, 0)), true, &cfg, 100);
        let out = g.on_tick(Some((100, 60.0, 2500)), true, &cfg, 2600);
        assert_eq!(out.fps, None);
        assert!(out.visible);
    }

    #[test]
    fn sample_other_pid_ignored() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        let out = g.on_tick(Some((200, 60.0, 0)), true, &cfg, 250);
        assert!(!g.is_game());
        assert_eq!(out.fps, None);
    }

    #[test]
    fn same_pid_refire_keeps_qualification() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        g.on_tick(Some((100, 60.0, 0)), true, &cfg, 100);
        let out = g.on_foreground(GAME, EXE, &cfg, 200);
        assert!(g.is_game());
        assert_eq!(out.timer, Timer::Game);
        assert!(out.visible);
    }

    #[test]
    fn leaving_disables_providers() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        g.on_tick(Some((100, 60.0, 0)), true, &cfg, 100);
        let out = g.on_foreground(Some(300), Some("explorer.exe"), &cfg, 500);
        assert_eq!(out.timer, Timer::Off);
        assert_eq!(out.providers, Providers::Off);
        assert!(!out.visible);
        assert!(!g.is_game());
    }

    #[test]
    fn no_window_is_not_candidate() {
        let cfg = Config::default();
        let mut g = GameGate::new();
        let out = g.on_foreground(None, None, &cfg, 0);
        assert_eq!(out.timer, Timer::Off);
        assert!(!g.is_game());
    }

    #[test]
    fn toggle_hides() {
        let cfg = Config::default();
        let mut g = gate_at(&cfg, 0);
        g.on_tick(Some((100, 60.0, 0)), true, &cfg, 100);
        let out = g.toggle();
        assert!(!out.visible);
        assert!(g.is_game());
    }
}
