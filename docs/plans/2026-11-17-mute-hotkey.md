# Mute Hotkey Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Alt+M (configurable) toggles a mute hold on the focused game's exe (held until pressed again or overlay exit); Settings window split into Overlay/Mute tabs with sliders for number inputs.

**Architecture:** `Muter` gains a `held` exe set that `reconcile` treats as "always wanted muted", reusing owned/stale/`muted.txt`. Config gains `mute_key`/`mute_mod`; `App::hotkeys` polls the chord with the existing in-game `GetAsyncKeyState` path and calls `App::toggle_hold`.

**Tech Stack:** Rust 2024, `windows` 0.62.2 (no new features expected), GDI/Win32.

**Spec:** `docs/specs/2026-11-17-mute-hotkey-design.md`

## Global Constraints

- Exactly two dependencies (`windows`, `windows-core`); no new crates, no dev-dependencies.
- No `RegisterHotKey`, no keyboard hooks; keys only via `GetAsyncKeyState` inside `App::hotkeys` (in game only).
- `mute` empty, no `muted.txt`, no hold ⇒ no COM init, no `WasapiMixer`, no mute timer, no WTS registration.
- Never nest `with_app`; nothing inside it pumps messages.
- Release exe ≤ 550 KB. `cargo clippy --release --all-targets -- -D warnings` and `cargo fmt --check` clean.
- Defaults: `mute_key=77`, `mute_mod=alt`. `mute_key=0` disables the hotkey.
- Tests: inline `#[cfg(test)] mod tests`, names are behaviour statements.

## Review Focus

1. Crash while a game is held (exe not in `mute`): next start must unmute it — Task 1 `stale_unlisted_exe_unmuted_on_start`.
2. Press on a game the user already muted in Volume Mixer: no ownership, second press leaves it muted — Task 1 `hold_respects_user_mute`.
3. Hold on a game with no audio session yet: mutes when its session appears (`WM_AUDIO` reconcile) — Task 1 `hold_mutes_session_that_appears_later`; Task 5 keeps `MuteState` alive while holding.
4. `mute_key` equal to Insert/End: rejected in Settings regardless of modifier — Task 4 `mute_key_clashing_with_other_hotkeys_rejected`.
5. Hand-edited `mute_mod=win` / `mute_key=300`: falls back per key — Task 2 `bad_mute_hotkey_values_fall_back`.
6. Min FPS typed above the slider's 240 (e.g. 500): saved as 500, slider pinned at 240; garbage text leaves the slider alone — Task 3 `slider_pos_clamps_and_ignores_garbage`.

---

### Task 1: Hold set in `Muter`

**Review:** per-task — persisted crash-recovery state (`muted.txt` semantics)

**Files:**
- Modify: `src/mute.rs` (struct `Muter`, `reconcile`, new methods, tests)

**Interfaces:**
- Produces: `Muter::toggle_hold(&mut self, exe: &str) -> bool` (new state: true = now held); `Muter::is_holding(&self) -> bool`; `Muter::retain_holds(&mut self, keep: impl FnMut(&String) -> bool)` (drops holds only; the next `reconcile` unmutes). `reconcile`/`release`/`persisted` signatures unchanged.

- [ ] **Step 1: Write the failing tests** (in `mute::tests`, reuse `FakeMixer`, `list`, `calls`, `set`)

```rust
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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test mute::tests`
Expected: compile error, `toggle_hold` / `is_holding` not found.

- [ ] **Step 3: Implement in `src/mute.rs`**

Add `held: HashSet<String>` to `Muter` (`Default` and `with_stale` init it empty). `toggle_hold`: insert, or remove if present; return membership after. `is_holding`: `!held.is_empty()`. In `reconcile`:
- adoption loop: drop the `listed(list, &s.exe)` condition (any stale exe seen muted is adopted);
- main loop: `let want = (listed(list, &s.exe) && fg != Some(s.exe.as_str())) || self.held.contains(&s.exe);` — `want && !s.muted` ⇒ mute + own; `!want` ⇒ if owned, unmute (failure ⇒ stale); unlisted, unheld, unowned sessions get no call.
Update the `reconcile` and struct doc comments to name `held`. `persisted()` stays owned ∪ stale: a held exe we muted is persisted as owned (crash recovery); hold membership itself is never written. `retain_holds` = `self.held.retain(keep)`.

- [ ] **Step 4: Update the one test the adoption change breaks, then run all mute tests**

`startup_stale_adopts_muted` pins the old "stale but unlisted is left alone" rule. The spec changes it: expect `calls(&[("g", false), ("x", false)])`, persisted still empty; add a `// Regression:`-style comment that a stale exe dropped from `mute` is unmuted on start.

Run: `cargo test mute::tests`
Expected: PASS; every other pre-existing test unchanged.

- [ ] **Step 5: Commit** `src/mute.rs` — "feat(mute): hold set that keeps an exe muted while focused"

---

### Task 2: `mute_key` / `mute_mod` config keys

**Review:** per-task — persisted format (`config.ini`)

**Files:**
- Modify: `src/config.rs` (new enum, `Config`, `DEFAULT_INI`, `Default`, `to_ini`, `set`, tests)

**Interfaces:**
- Produces: `#[derive(Clone, Copy, Debug, PartialEq)] pub enum Modifier { None, Alt, Ctrl, Shift }` with `pub fn vk(self) -> Option<u8>` (`None` ⇒ `None`, Alt `0x12`, Ctrl `0x11`, Shift `0x10`); `Config.mute_key: u8`, `Config.mute_mod: Modifier`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn mute_hotkey_defaults_to_alt_m() {
    let c = parse("");
    assert_eq!((c.mute_key, c.mute_mod), (77, Modifier::Alt));
}

#[test]
fn mute_hotkey_round_trips_and_zero_disables() {
    let c = Config { mute_key: 0, mute_mod: Modifier::Ctrl, ..Config::default() };
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
```

Also extend `to_ini_roundtrips_non_defaults` with `mute_key: 0x4E, mute_mod: Modifier::None`. `default_ini_roundtrips` must keep passing.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test config::tests`
Expected: compile error, no `mute_key` / `Modifier`.

- [ ] **Step 3: Implement**

`DEFAULT_INI` and `to_ini`: `mute_key=77` and `mute_mod=alt` lines after `exit_key`. `to_ini` writes `none|alt|ctrl|shift`. `set`: `mute_key` accepts 0–254 (unlike `key_code`, which rejects 0); `mute_mod` matches the four lowercase names, anything else ignored.

- [ ] **Step 4: Run tests**

Run: `cargo test config::tests`
Expected: PASS.

- [ ] **Step 5: Commit** `src/config.rs` — "feat(config): mute_key and mute_mod"

---

### Task 3: Settings window — Overlay/Mute tabs + sliders

**Files:**
- Modify: `src/settings.rs` (constants, `Ui`, `open` window size, `build`, `wndproc`, tests)
- Modify (only if Step 3's check fails): `Cargo.toml` (add `Win32_UI_Controls` feature)

**Interfaces:**
- Consumes: nothing from other tasks.
- Produces: `fn slider_pos(text: &str, lo: u32, hi: u32) -> Option<u32>` (`None` for unparsable; else clamped). A Mute tab page that Task 4 adds rows to: `build` registers each control on a page through one helper (e.g. `page(MUTE, hwnd)`), so Task 4 only places controls.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn slider_pos_clamps_and_ignores_garbage() {
    assert_eq!(slider_pos("500", 1, 240), Some(240));
    assert_eq!(slider_pos(" 60 ", 1, 240), Some(60));
    assert_eq!(slider_pos("0", 1, 240), Some(1));
    assert_eq!(slider_pos("", 1, 240), None);
    assert_eq!(slider_pos("abc", 50, 200), None);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test settings::tests`
Expected: compile error, `slider_pos` not found.

- [ ] **Step 3: Check the native classes create**

Throwaway (do not commit): in `build`, `CreateWindowExW` one `SysTabControl32` and one `msctls_trackbar32`; `cargo build`, run the debug exe elevated, open Settings, confirm both appear. If either handle is null, add the `Win32_UI_Controls` feature and call `InitCommonControlsEx(ICC_TAB_CLASSES | ICC_BAR_CLASSES)` at the top of `settings::open` (never at startup). Record the outcome in the commit message.

- [ ] **Step 4: Implement**

- Raw constants: `TCM_INSERTITEMW = 0x133E`, `TCM_GETCURSEL = 0x130B`, `TCN_SELCHANGE = -551` (`NMHDR.code` as i32), `TBM_GETPOS = 0x400`, `TBM_SETPOS = 0x405`, `TBM_SETRANGEMIN = 0x407`, `TBM_SETRANGEMAX = 0x408`, `TBM_SETPAGESIZE = 0x415`, `EN_CHANGE = 0x300`. New IDs: `ID_TABS = 140`, `ID_SCALE_SLIDER = 141`, `ID_OPACITY_SLIDER = 142`, `ID_MIN_FPS_SLIDER = 143`.
- Layout: tab control at the top of the client area; Overlay page = Appearance, Steam-like bar, Game detection, Hotkeys (show/hide, exit); Mute page = "Background muting" group (list + Add…). Shared bottom row (Start with Windows, Save, Cancel) below the tab control. Client height = tallest page + tab header + bottom row. Mute page controls created hidden.
- `Ui` gains `pages: [Vec<HWND>; 2]`. `WM_NOTIFY` with `TCN_SELCHANGE` from `ID_TABS`: hide every HWND of the other page, show the selected one.
- Sliders: one per row from the spec table (ranges 50–200, 0–100, 1–240), between label and box, page size 10, initial position = `slider_pos(box text, lo, hi)`. `WM_HSCROLL` from a slider: `SetWindowTextW` its box to `TBM_GETPOS`. `WM_COMMAND` with `EN_CHANGE` (high word) from a box: if `slider_pos` is `Some`, `TBM_SETPOS` (wParam 1 = redraw). Box `ES_NUMBER` and `from_fields` validation unchanged (Min FPS still 1–1000).

- [ ] **Step 5: Run tests and smoke**

Run: `cargo test settings::tests`; expected PASS. Smoke (debug build, elevated): tabs switch; Tab key never lands on a hidden control; dragging each slider updates its box; typing `500` in Min FPS pins its slider right and saves `min_fps=500`; Save from the Mute tab still saves Overlay values; window fits at 150% DPI.

- [ ] **Step 6: Commit** `src/settings.rs` (and `Cargo.toml` if changed) — "feat(settings): Overlay/Mute tabs and slider inputs"

---

### Task 4: Settings window — mute hotkey pickers

**Files:**
- Modify: `src/settings.rs` (constants, `Fields`, `from_fields`, `build` Mute page, `save`, tests)

**Interfaces:**
- Consumes: `config::Modifier`, `Config.mute_key`, `Config.mute_mod` (Task 2); Mute page registration helper (Task 3).
- Produces: `Fields.mute_key: u8`, `Fields.mute_mod: usize` (index into `MODS`).

- [ ] **Step 1: Write the failing tests**

Update the `fields()` helper to set `mute_key: 0x4D, mute_mod: 1`. Add to `valid_form_maps_every_field`: `assert_eq!((c.mute_key, c.mute_mod), (0x4D, Modifier::Alt));` (the existing ini round-trip line then covers both).

```rust
#[test]
fn mute_key_clashing_with_other_hotkeys_rejected() {
    let mut f = fields("100", 0x70, 0x7B);
    f.mute_key = 0x70;
    assert!(from_fields(&f).is_err());
    f.mute_key = 0x7B;
    f.mute_mod = 0;
    assert!(from_fields(&f).is_err());
    f.mute_key = 0; // disabled: never clashes
    assert_eq!(from_fields(&f).unwrap().mute_key, 0);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test settings::tests`
Expected: compile error, `Fields` has no `mute_key`.

- [ ] **Step 3: Implement**

- `const ID_MUTE_KEY: i32 = 132; const ID_MUTE_MOD: i32 = 133;` and `const MODS: [(&str, Modifier); 4] = [("None", None), ("Alt", Alt), ("Ctrl", Ctrl), ("Shift", Shift)];`
- `from_fields`: when `mute_key != 0` and it equals `toggle_key` or `exit_key` ⇒ `Err("The mute key must differ from the show/hide and exit keys.")`. `mute_mod` index out of range ⇒ `Modifier::None`.
- `build`: on the **Mute** page add a "Mute focused game" row with a modifier combo (`MODS`, selected from `c.mute_mod`) and a key combo whose items are `("(none)", 0)`, `KEYS`, then `A`–`Z` (`0x41..=0x5A`), plus the existing "Key code N" fallback for an unknown value. Register both on the Mute page; grow it if needed (window height follows the tallest page).
- `save`: read both combos (`combo_value(.., ID_MUTE_KEY) as u8`, `combo_value(.., ID_MUTE_MOD)`; the mod combo's item data is its index).

- [ ] **Step 4: Run tests**

Run: `cargo test settings::tests`
Expected: PASS.

- [ ] **Step 5: Commit** `src/settings.rs` — "feat(settings): mute hotkey pickers"

---

### Task 5: Hotkey wiring in `App` + docs

**Review:** per-task — destructive (leaves apps muted if exit/`mute_wanted` wiring is wrong)

**Files:**
- Modify: `src/main.rs` (`mute_wanted`, `sync_mute`, new `toggle_hold`, `hotkeys`, `edges`, `App.keys` field and init, tests)
- Modify: `README.md`, `AGENTS.md`, `docs/specs/2026-11-15-background-muter-design.md`, `docs/specs/2026-11-01-steam-overlay-port-design.md`

**Interfaces:**
- Consumes: `Muter::toggle_hold`, `Muter::is_holding`, `Muter::retain_holds` (Task 1); `Config.mute_key`, `Config.mute_mod`, `Modifier::vk` (Task 2).
- Produces: `fn mute_wanted(list_empty: bool, persisted_empty: bool, holding: bool) -> bool`; `fn edges(prev: [bool; 3], cur: [bool; 3]) -> [bool; 3]` (order: toggle, exit, mute); `fn hold_releasable(exe: &str, cfg: &Config) -> bool` (`cfg.mute_key != 0 && !cfg.ignore.contains(exe)`); `App::toggle_hold(&mut self)`.

- [ ] **Step 1: Write the failing tests** (in `main::tests`)

Rewrite `held_key_fires_once` for `[bool; 3]` (same cases, third slot included, plus `edges([false, false, false], [false, false, true]) == [false, false, true]`). Extend the existing `mute_wanted` test:

```rust
assert!(!mute_wanted(true, true, false)); // idle: no COM at all
assert!(mute_wanted(true, true, true));   // a hold alone keeps the mixer
```

```rust
#[test]
fn hold_unreleasable_when_key_off_or_exe_ignored() {
    let c = Config::default();
    assert!(hold_releasable("game.exe", &c));
    assert!(!hold_releasable("game.exe", &Config { mute_key: 0, ..c.clone() }));
    assert!(!hold_releasable("game.exe", &Config { ignore: vec!["game.exe".into()], ..c }));
}
```

(existing cases gain `, false`).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test main::tests`
Expected: compile error (arity).

- [ ] **Step 3: Implement**

- `mute_wanted`: `!list_empty || !persisted_empty || holding`. Callers in `sync_mute` pass `false` on creation and `m.muter.is_holding()` on the drop check.
- `App::toggle_hold`: `let Some(exe) = self.fg_exe.clone() else { return };` if `self.mute.is_none()`, create `MuteState` exactly as `sync_mute` does but regardless of `mute_wanted` (extract the creation block into `fn create_mute(&mut self) -> bool` used by both; `com_init`/`WasapiMixer::new` failure ⇒ return). Then `m.muter.toggle_hold(&exe)`, `self.mute_reconcile()`, and if nothing is wanted any more, `self.drop_mute()`.
- `App::hotkeys`: `keys: [bool; 3]`; mute chord down = `cfg.mute_key != 0 && down(cfg.mute_key) && cfg.mute_mod.vk().is_none_or(down)`. On its edge call `self.toggle_hold()` (no `with_app`, already inside). Exit handling unchanged.
- `App::reconfigure`: right after `self.cfg = cfg;`, drop holds that could never be released: `if let Some(m) = self.mute.as_mut() { m.muter.retain_holds(|e| hold_releasable(e, &self.cfg)); }` (disjoint field borrows). The existing `sync_mute` at the end reconciles (unmutes them) and drops `MuteState` if nothing is wanted.
- Tray mute item: unchanged (list-only, per spec).

- [ ] **Step 4: Run tests, clippy, fmt**

Run: `cargo test` then `cargo clippy --release --all-targets -- -D warnings` then `cargo fmt --check`
Expected: all pass, 0 warnings, no diff.

- [ ] **Step 5: Docs**

- README: hotkey table row "Mute/unmute the focused game | `Alt+M`"; config rows `mute_key` (`77` (M), 0–254, 0 = off) and `mute_mod` (`alt`, `none|alt|ctrl|shift`); one line that the mute lasts until pressed again or exit; Settings section mentions the Overlay/Mute tabs and sliders (Min FPS slider tops out at 240, box accepts up to 1000).
- AGENTS.md: data-flow block gains `hotkeys → mute chord edge → App::toggle_hold → MuteState (created lazily) → reconcile`; mute invariant becomes "`mute` empty, no `muted.txt`, and no hold ⇒ no COM…"; Mute bullet mentions `held`.
- Background-muter spec: remove "hotkey for muting" from Non-goals with a pointer to `2026-11-17-mute-hotkey-design.md`; note stale adoption no longer requires the exe to be listed.
- Overlay spec, Hotkeys section: add the mute chord (polled the same way; modifier checked with `GetAsyncKeyState`).

- [ ] **Step 6: Release build + smoke**

Run: `cargo build --release`; check exe size ≤ 550 KB. Manual (elevated): game focused, Alt+M ⇒ game silent in Volume Mixer, other apps unaffected; alt-tab ⇒ still muted; Alt+M ⇒ sound back; Alt+M then tray Exit ⇒ game unmuted; desktop focused with `mute` empty ⇒ no `mmdevapi.dll` loaded until the first press.

- [ ] **Step 7: Commit** `src/main.rs`, docs — "feat: Alt+M mutes the focused game"
