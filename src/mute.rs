//! Pure mute decision logic for background apps (no Windows APIs).
// Ported from BGMuter `src/core.rs`. Nothing here calls into the Win32 shell,
// so the whole policy is testable without a mixer.

use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub exe: String,
    pub muted: bool,
}

pub trait Mixer {
    fn sessions(&mut self) -> Vec<Session>;
    /// `false` = failed or session gone.
    fn set_mute(&mut self, id: &str, mute: bool) -> bool;
}

/// Only unmutes sessions it muted itself. `owned` maps those session ids to
/// their exe. `stale` holds exes whose mute may still be in effect but whose
/// session was lost (gone, failed unmute, previous run): Windows persists
/// per-app mute, so the next muted session of such an exe is re-adopted.
/// `held` is the hotkey hold set: those exes stay muted even in the foreground.
/// Holding is a live decision, so it is never persisted; an exe it muted is
/// persisted through `owned`/`stale` like any other crash-recovery entry.
#[derive(Default)]
pub struct Muter {
    owned: HashMap<String, String>,
    stale: HashSet<String>,
    held: HashSet<String>,
}

impl Muter {
    /// Start with exes a previous run may have left muted.
    pub fn with_stale(stale: impl IntoIterator<Item = String>) -> Self {
        Self {
            owned: HashMap::new(),
            stale: stale.into_iter().collect(),
            held: HashSet::new(),
        }
    }

    /// Exes to persist: owned ∪ stale.
    pub fn persisted(&self) -> BTreeSet<String> {
        self.owned.values().chain(&self.stale).cloned().collect()
    }

    /// Add or remove `exe` from the hold set; returns whether it is now held.
    // ponytail: no caller until the mute hotkey lands; drop the allow with it.
    #[allow(dead_code)]
    pub fn toggle_hold(&mut self, exe: &str) -> bool {
        if self.held.remove(exe) {
            return false;
        }
        self.held.insert(exe.to_string());
        true
    }

    #[allow(dead_code)]
    pub fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Drop holds failing `keep`. `owned` mutes stay until the next
    /// `reconcile` sees them unwanted.
    #[allow(dead_code)]
    pub fn retain_holds(&mut self, keep: impl FnMut(&String) -> bool) {
        self.held.retain(keep);
    }

    /// Mute sessions that are either listed and not in the foreground or held;
    /// unmute owned ones that are neither. Everything else is left alone.
    pub fn reconcile(&mut self, m: &mut impl Mixer, list: &[String], fg: Option<&str>) {
        let sessions = m.sessions();
        let present: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        let stale = &mut self.stale;
        self.owned.retain(|id, exe| {
            present.contains(id.as_str()) || {
                stale.insert(exe.clone());
                false
            }
        });
        for s in &sessions {
            if s.muted && self.stale.contains(&s.exe) {
                self.owned
                    .entry(s.id.clone())
                    .or_insert_with(|| s.exe.clone());
            }
        }
        // Windows state for these exes is now known; later user mutes are respected.
        for s in &sessions {
            self.stale.remove(&s.exe);
        }
        for s in &sessions {
            let want =
                (listed(list, &s.exe) && fg != Some(s.exe.as_str())) || self.held.contains(&s.exe);
            if want {
                if !s.muted && m.set_mute(&s.id, true) {
                    self.owned.insert(s.id.clone(), s.exe.clone());
                }
            } else if let Some(exe) = self.owned.remove(&s.id)
                && !m.set_mute(&s.id, false)
            {
                self.stale.insert(exe);
            }
        }
    }

    /// Unmute everything this instance muted, then forget it (failures go stale).
    pub fn release(&mut self, m: &mut impl Mixer) {
        for (id, exe) in self.owned.drain() {
            if !m.set_mute(&id, false) {
                self.stale.insert(exe);
            }
        }
    }
}

fn listed(list: &[String], exe: &str) -> bool {
    list.iter().any(|l| l == exe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct FakeMixer {
        sessions: Vec<Session>,
        calls: Vec<(String, bool)>,
        fail: std::collections::HashSet<String>,
    }

    impl FakeMixer {
        fn new(items: &[(&str, &str, bool)]) -> Self {
            Self {
                sessions: items
                    .iter()
                    .map(|(id, exe, muted)| Session {
                        id: id.to_string(),
                        exe: exe.to_string(),
                        muted: *muted,
                    })
                    .collect(),
                calls: Vec::new(),
                fail: Default::default(),
            }
        }
    }

    impl Mixer for FakeMixer {
        fn sessions(&mut self) -> Vec<Session> {
            self.sessions.clone()
        }

        fn set_mute(&mut self, id: &str, mute: bool) -> bool {
            self.calls.push((id.to_string(), mute));
            if self.fail.contains(id) {
                return false;
            }
            if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                s.muted = mute;
            }
            true
        }
    }

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn calls(items: &[(&str, bool)]) -> Vec<(String, bool)> {
        items.iter().map(|(id, b)| (id.to_string(), *b)).collect()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mutes_listed_unfocused() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false), ("d", "discord.exe", false)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("discord.exe"));
        assert_eq!(m.calls, calls(&[("g", true)]));
    }

    #[test]
    fn unmutes_on_focus() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false), ("d", "discord.exe", false)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("discord.exe"));
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", true), ("g", false)]));
    }

    #[test]
    fn respects_user_mute() {
        let mut m = FakeMixer::new(&[("g", "game.exe", true)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("other.exe"));
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("game.exe"));
        assert!(m.calls.is_empty());
    }

    #[test]
    fn none_foreground_mutes_all_listed() {
        let mut m = FakeMixer::new(&[
            ("a", "a.exe", false),
            ("b", "b.exe", false),
            ("u", "u.exe", false),
        ]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["a.exe", "b.exe"]), None);
        assert_eq!(m.calls, calls(&[("a", true), ("b", true)]));
    }

    #[test]
    fn failed_mute_not_owned() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        m.fail.insert("g".to_string());
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), None);
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", true)]));
    }

    #[test]
    fn release_only_owned() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false), ("u", "u.exe", true)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), None);
        muter.release(&mut m);
        assert_eq!(m.calls, calls(&[("g", true), ("g", false)]));
    }

    #[test]
    fn lost_session_adopted_on_return() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), None);
        m.sessions = FakeMixer::new(&[("g2", "game.exe", true)]).sessions;
        m.calls.clear();
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g2", false)]));
    }

    #[test]
    fn stale_cleared_after_seen() {
        let l = list(&["game.exe"]);
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &l, None);
        m.sessions = FakeMixer::new(&[("g2", "game.exe", true)]).sessions;
        m.calls.clear();
        muter.reconcile(&mut m, &l, Some("other.exe"));
        assert!(m.calls.is_empty());
        assert_eq!(muter.persisted(), set(&["game.exe"]));
        // A user mute appearing after the exe was seen again is respected.
        m.sessions
            .push(FakeMixer::new(&[("g3", "game.exe", true)]).sessions[0].clone());
        muter.reconcile(&mut m, &l, Some("other.exe"));
        muter.reconcile(&mut m, &l, Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g2", false)]));
    }

    #[test]
    fn failed_unmute_becomes_stale() {
        let l = list(&["game.exe"]);
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &l, None);
        m.fail.insert("g".to_string());
        muter.reconcile(&mut m, &l, Some("game.exe"));
        assert_eq!(muter.persisted(), set(&["game.exe"]));
        m.fail.clear();
        muter.reconcile(&mut m, &l, Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", true), ("g", false), ("g", false)]));
        assert!(muter.persisted().is_empty());
    }

    #[test]
    fn failed_release_becomes_stale() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), None);
        m.fail.insert("g".to_string());
        muter.release(&mut m);
        assert_eq!(muter.persisted(), set(&["game.exe"]));
    }

    #[test]
    fn hold_mutes_focused_unlisted_exe() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false), ("s", "spotify.exe", false)]);
        let mut muter = Muter::default();
        assert!(muter.toggle_hold("game.exe"));
        muter.reconcile(&mut m, &[], Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", true)]));
        assert_eq!(muter.persisted(), set(&["game.exe"]));
    }

    #[test]
    fn unhold_unmutes_owned() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.toggle_hold("game.exe");
        muter.reconcile(&mut m, &[], Some("game.exe"));
        assert!(!muter.toggle_hold("game.exe"));
        assert!(!muter.is_holding());
        muter.reconcile(&mut m, &[], Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", true), ("g", false)]));
        assert!(muter.persisted().is_empty());
    }

    #[test]
    fn hold_respects_user_mute() {
        let mut m = FakeMixer::new(&[("g", "game.exe", true)]);
        let mut muter = Muter::default();
        muter.toggle_hold("game.exe");
        muter.reconcile(&mut m, &[], Some("game.exe"));
        muter.toggle_hold("game.exe");
        muter.reconcile(&mut m, &[], Some("game.exe"));
        assert!(m.calls.is_empty());
    }

    #[test]
    fn hold_overrides_focus_for_listed_exe() {
        let l = list(&["game.exe"]);
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.toggle_hold("game.exe");
        muter.reconcile(&mut m, &l, Some("game.exe"));
        muter.toggle_hold("game.exe");
        muter.reconcile(&mut m, &l, Some("other.exe")); // still listed + background: stays muted
        assert_eq!(m.calls, calls(&[("g", true)]));
    }

    #[test]
    fn hold_mutes_session_that_appears_later() {
        let mut m = FakeMixer::new(&[]);
        let mut muter = Muter::default();
        muter.toggle_hold("game.exe");
        muter.reconcile(&mut m, &[], Some("game.exe"));
        m.sessions = FakeMixer::new(&[("g", "game.exe", false)]).sessions;
        muter.reconcile(&mut m, &[], Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", true)]));
    }

    #[test]
    fn release_unmutes_held() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.toggle_hold("game.exe");
        muter.reconcile(&mut m, &[], None);
        muter.release(&mut m);
        assert_eq!(m.calls, calls(&[("g", true), ("g", false)]));
    }

    // Regression guard for crash recovery of a held, unlisted exe.
    #[test]
    fn stale_unlisted_exe_unmuted_on_start() {
        let mut m = FakeMixer::new(&[("g", "game.exe", true)]);
        let mut muter = Muter::with_stale(["game.exe".to_string()]);
        muter.reconcile(&mut m, &[], Some("other.exe"));
        assert_eq!(m.calls, calls(&[("g", false)]));
        assert!(muter.persisted().is_empty());
    }

    #[test]
    fn dropped_hold_unmutes_on_next_reconcile() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false), ("h", "other.exe", false)]);
        let mut muter = Muter::default();
        muter.toggle_hold("game.exe");
        muter.toggle_hold("other.exe");
        muter.reconcile(&mut m, &[], None);
        muter.retain_holds(|e| e != "game.exe");
        muter.reconcile(&mut m, &[], None);
        assert_eq!(m.calls, calls(&[("g", true), ("h", true), ("g", false)]));
        assert!(muter.is_holding());
    }

    // Regression: a stale exe dropped from the mute list is unmuted on start.
    #[test]
    fn startup_stale_adopts_muted() {
        let mut m = FakeMixer::new(&[("g", "game.exe", true), ("x", "x.exe", true)]);
        let mut muter = Muter::with_stale(["game.exe".to_string(), "x.exe".to_string()]);
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", false), ("x", false)]));
        assert!(muter.persisted().is_empty());
    }

    #[test]
    fn persisted_tracks_owned_and_stale() {
        let l = list(&["a.exe", "b.exe"]);
        let mut m = FakeMixer::new(&[("a", "a.exe", false), ("b", "b.exe", false)]);
        let mut muter = Muter::with_stale(["c.exe".to_string()]);
        assert_eq!(muter.persisted(), set(&["c.exe"]));
        muter.reconcile(&mut m, &l, None);
        assert_eq!(muter.persisted(), set(&["a.exe", "b.exe", "c.exe"]));
        m.sessions.retain(|s| s.id != "b");
        muter.reconcile(&mut m, &l, Some("a.exe"));
        assert_eq!(muter.persisted(), set(&["b.exe", "c.exe"]));
    }

    #[test]
    fn forgets_owned_ids_when_session_gone() {
        let mut m = FakeMixer::new(&[("g", "game.exe", false)]);
        let mut muter = Muter::default();
        muter.reconcile(&mut m, &list(&["game.exe"]), None);
        m.sessions.retain(|s| s.id != "g");
        m.calls.clear();
        muter.reconcile(&mut m, &list(&["game.exe"]), None);
        muter.release(&mut m);
        assert!(m.calls.is_empty());
    }
}
