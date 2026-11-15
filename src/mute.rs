//! Pure mute decision logic for background apps (no Windows APIs).
// Ported from BGMuter `src/core.rs`. Nothing here calls into the Win32 shell,
// so the whole policy is testable without a mixer.
#![cfg_attr(not(test), allow(dead_code))] // Task 5 wires this into the app.

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
#[derive(Default)]
pub struct Muter {
    owned: HashMap<String, String>,
    stale: HashSet<String>,
}

impl Muter {
    /// Start with exes a previous run may have left muted.
    pub fn with_stale(stale: impl IntoIterator<Item = String>) -> Self {
        Self {
            owned: HashMap::new(),
            stale: stale.into_iter().collect(),
        }
    }

    /// Exes to persist: owned ∪ stale.
    pub fn persisted(&self) -> BTreeSet<String> {
        self.owned.values().chain(&self.stale).cloned().collect()
    }

    /// Mute listed apps that are not in the foreground; unmute owned ones that
    /// are. Unlisted sessions are never touched.
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
            if s.muted && listed(list, &s.exe) && self.stale.contains(&s.exe) {
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
            if !listed(list, &s.exe) {
                continue;
            }
            if fg != Some(s.exe.as_str()) {
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
    fn startup_stale_adopts_muted() {
        let mut m = FakeMixer::new(&[("g", "game.exe", true), ("x", "x.exe", true)]);
        let mut muter = Muter::with_stale(["game.exe".to_string(), "x.exe".to_string()]);
        muter.reconcile(&mut m, &list(&["game.exe"]), Some("game.exe"));
        assert_eq!(m.calls, calls(&[("g", false)]));
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
