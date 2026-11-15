# Background Muter in FPS Overlay — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Mute listed apps while unfocused inside FPS Overlay (ported from BGMuter), then fix the Start-with-Windows "overlay never shows" bug.

**Architecture:** Pure `mute.rs` core + `audio.rs` WASAPI mixer ported from `D:\Coding\Rust\BGMuter\src\{core,audio}.rs`; `App` owns an optional `MuteState` driven by the existing foreground hook plus audio/WTS window messages; native Win32 settings/tray additions; `muted.txt` persistence beside `config.ini`.

**Tech Stack:** Rust 1.100 (after Task 0) edition 2024, `windows` 0.62.2, `windows-core` 0.62.

**Spec:** `docs/specs/2026-11-15-background-muter-design.md`

## Global Constraints
- AGENTS.md is binding, except dependencies are now exactly `windows = 0.62.2` and `windows-core = 0.62`.
- Release profile unchanged (`panic = "abort"`, `opt-level = "s"`); exe ≤ 500 KB (stop and report if exceeded).
- With `mute` empty: no COM init, no audio DLL loads, no extra timer, no WTS registration.
- Mute semantics ported verbatim from BGMuter: owned set, `stale` re-adoption, skip session on `GetMute` failure, release before reconcile on config change.
- `muted.txt` next to the exe; one lowercase exe per line; atomic write (tmp + rename); deleted when empty.
- Cross-thread: COM sinks only `PostMessageW(MAIN_HWND, WM_AUDIO, WPARAM(kind), 0)`. `WM_AUDIO = WM_APP + 3`. Mute debounce timer id `MUTE_TIMER_ID = 2`, 150 ms, one-shot.
- Never nest `with_app`; nothing that pumps messages runs under the borrow.
- Resource limit (user): run cargo with `CARGO_BUILD_JOBS=4` and `RUST_TEST_THREADS=2`; never parallel builds.
- `cargo fmt`, `cargo clippy --release --all-targets -- -D warnings`, `cargo test` clean before each commit.

## Review Focus
1. Listed game quits while muted, relaunches muted ⇒ unmuted on focus (ported `lost_session_adopted_on_return`).
2. User mutes a game themselves ⇒ never unmuted by us (ported `respects_user_mute`, `stale_cleared_after_seen`).
3. App removed from `mute` while muted ⇒ unmuted on Save (Task 5 `reconfigure` release-first; manual check).
4. `mute=` with spaces/case/duplicates/empty entries ⇒ normalized; `parse(to_ini(c)) == c` (Task 3 tests).
5. `mute` empty ⇒ no COM/audio work at all (Task 5 manual: `muted.txt` absent, no `MMDevAPI.dll` loaded per Process Explorer/`tasklist /m mmdevapi.dll`).

---

### Task 0: Migrate to Rust 1.100

**Files:** Modify `Cargo.toml` (`rust-version = "1.100"` under `[package]`), `AGENTS.md` (Runtime/Tooling: "Rust 1.100+ (MSRV pinned via `rust-version`), edition 2024; no `rust-toolchain.toml`"), any source file that new 1.100 lints/warnings flag.

- [ ] Step 1: `rustup update stable` (1.99.1 ⇒ 1.100.0; global stable, other projects keep building). Confirm `rustc --version` = 1.100.0.
- [ ] Step 2: Baseline on 1.100: `cargo fmt --check`, `cargo clippy --release --all-targets -- -D warnings`, `cargo test`, `cargo build --release`; record every new warning/lint and the exe size vs 481 KB.
- [ ] Step 3: Fix new lints/warnings without behaviour changes (no `#[allow]` unless the lint is a false positive; justify each). Add `rust-version = "1.100"`.
- [ ] Step 4: All four commands clean; exe size reported. Commit `chore: migrate to Rust 1.100`.

### Task 1: Pure mute core

**Files:** Create `src/mute.rs`; Modify `src/main.rs` (`mod mute;` only).

**Interfaces:**
- Produces (port of BGMuter `src/core.rs`, drop the `paused` parameter):
  ```rust
  pub struct Session { pub id: String, pub exe: String, pub muted: bool }
  pub trait Mixer { fn sessions(&mut self) -> Vec<Session>; fn set_mute(&mut self, id: &str, mute: bool) -> bool; }
  #[derive(Default)] pub struct Muter { /* owned: HashMap<String,String>, stale: HashSet<String> */ }
  impl Muter {
      pub fn with_stale(stale: impl IntoIterator<Item = String>) -> Self;
      pub fn persisted(&self) -> std::collections::BTreeSet<String>;
      pub fn reconcile(&mut self, m: &mut impl Mixer, list: &[String], fg: Option<&str>);
      pub fn release(&mut self, m: &mut impl Mixer);
  }
  ```
- [ ] Step 1: Port all BGMuter core tests (rename to behaviour statements per AGENTS.md; drop pause-only tests, keep everything else incl. `lost_session_adopted_on_return`, `stale_cleared_after_seen`, `failed_unmute_becomes_stale`, `failed_release_becomes_stale`, `startup_stale_adopts_muted`, `persisted_tracks_owned_and_stale`, `respects_user_mute`, `none_foreground_mutes_all_listed`, `failed_mute_not_owned`, `forgets_owned_ids_when_session_gone`).
- [ ] Step 2: `cargo test mute::tests` ⇒ FAIL (missing items).
- [ ] Step 3: Port implementation; `//!` header; `#[allow(dead_code)]` only on items unused until Task 5, removed there.
- [ ] Step 4: `cargo test mute::tests` ⇒ all pass. Step 5: commit `feat: background mute core (ported from BGMuter)`.

### Task 2: WASAPI mixer

**Review:** per-task — concurrency (COM callbacks on foreign threads)

**Files:** Create `src/audio.rs`; Modify `src/main.rs` (`mod audio;`, `pub const WM_AUDIO: u32 = WM_APP + 3;`), `Cargo.toml` (add `windows-core = "0.62"`; features `Win32_Media_Audio`, `Win32_System_Com`, `Win32_System_Com_StructuredStorage`, `Win32_System_Variant`).

**Interfaces:**
- Consumes: `crate::mute::{Session, Mixer}`, `crate::process::exe_name(pid) -> Option<String>`, `crate::MAIN_HWND` (AtomicUsize).
- Produces:
  ```rust
  pub const NOTIFY_SESSION: usize = 1; pub const NOTIFY_DEVICE: usize = 2;
  pub struct WasapiMixer { .. }
  impl WasapiMixer { pub fn new() -> windows::core::Result<Self>; pub fn refresh_endpoints(&mut self) -> windows::core::Result<()>; }
  impl crate::mute::Mixer for WasapiMixer {}
  impl Drop for WasapiMixer {} // unregisters everything
  ```
- Port BGMuter `src/audio.rs` as-is except: sink reads `MAIN_HWND` at post time (skip when 0) instead of storing an HWND; caller has already initialised COM STA.
- [ ] Step 1: `#[test] #[ignore] fn lists_live_sessions()` (CoInitializeEx STA, `WasapiMixer::new`, print `sessions()`).
- [ ] Step 2: `cargo build`, `cargo test audio -- --ignored --nocapture` ⇒ prints sessions with non-empty exe names.
- [ ] Step 3: `cargo build --release`; record exe size. Step 4: commit `feat: WASAPI mixer for background muting`.

### Task 3: Config `mute` + `muted.txt`

**Review:** per-task — persisted format

**Files:** Modify `src/config.rs`.

**Interfaces:**
- Produces: `Config.mute: Vec<String>` (default empty); `DEFAULT_INI` gains `mute=` after `ignore=`; `to_ini` writes `mute=<a,b>`; `set` parses `mute` with `ignore_list`; `pub fn add_to_list(list: &mut Vec<String>, exe: &str) -> bool` (rename of `add_ignore`; update its two callers in `main.rs`/`settings.rs`); `pub fn load_owned() -> Vec<String>`, `pub fn save_owned(exes: &BTreeSet<String>) -> std::io::Result<()>` with path-taking `load_owned_from(&Path)` / `save_owned_to(&Path, ..)` for tests (`muted.txt` beside exe).
- [ ] Step 1: Tests: `mute_round_trips_through_ini` (`parse(to_ini(c)) == c` with `mute = ["a.exe","b.exe"]`), `mute_list_is_trimmed_lowercased_and_drops_empties` (`" A.exe , ,b.EXE"` ⇒ `["a.exe","b.exe"]`), `missing_mute_key_means_empty`, `owned_file_round_trips_and_empty_set_deletes_it`, `missing_owned_file_is_empty`.
- [ ] Step 2: FAIL. Step 3: implement. Step 4: PASS + full suite. Step 5: commit `feat: mute list and muted.txt in config`.

### Task 4: Settings window

**Files:** Modify `src/settings.rs`.

**Interfaces:**
- Consumes: `Config.mute`, `config::ignore_list`, `config::add_to_list`.
- Produces: `Fields.mute: &str`; `from_fields` sets `cfg.mute`; new controls `ID_MUTE` (edit) and `ID_ADD_MUTE` (`Add…`) in a "Background muting" group: label "Mute while not focused (exe names, comma separated)"; picker shares `collect`/`candidates`, excluding exes already in the mute box. Window height grows to fit; nothing else moves except what sits below the new group.
- [ ] Step 1: Test `from_fields_normalizes_mute_list` (`mute: " Game.EXE, ,x.exe"` ⇒ `["game.exe","x.exe"]`).
- [ ] Step 2: FAIL. Step 3: implement. Step 4: PASS. Step 5: release build, open Settings via tray once (manual), confirm layout; commit `feat: background muting in Settings`.

### Task 5: App integration, tray, docs

**Review:** per-task — concurrency (message loop, COM lifetime, exit-path release)

**Files:** Modify `src/main.rs`, `README.md`, `AGENTS.md`.

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces in `main.rs`:
  - `struct MuteState { mixer: audio::WasapiMixer, muter: mute::Muter, saved: BTreeSet<String> }`; `App.mute: Option<MuteState>`, `App.fg_exe: Option<String>` (set in `foreground`, `None` for own pid / no window).
  - `App::sync_mute(&mut self)` — create/drop `MuteState` per spec (create when `cfg.mute` non-empty or persisted stale non-empty; CoInitializeEx once; WTS register/unregister with it); then reconcile and save `muted.txt` on change.
  - `App::mute_reconcile(&mut self)`, `App::mute_release(&mut self)`.
  - `foreground()` arms `SetTimer(hwnd, MUTE_TIMER_ID, 150)` when `mute.is_some()`; `WM_TIMER` dispatches by id (`TIMER_ID` ⇒ `tick`, `MUTE_TIMER_ID` ⇒ kill + reconcile).
  - `WM_AUDIO`, `WM_WTSSESSION_CHANGE` (unlock) handlers per spec.
  - `reconfigure`: `mute_release` before swapping cfg, then `sync_mute`.
  - Exit: before `APP` drops (loop end and `WM_ENDSESSION`), `mute_release` + save.
  - Tray: item "Mute {exe} in background" / "Stop muting {exe}" for `fg_exe` remembered as the last non-own foreground exe (`last_fg_exe`), toggling `cfg.mute`, save, `reconfigure`.
  - Startup: after `APP` is set, `with_app(|a| a.sync_mute())`.
- [ ] Step 1: Unit-test any extracted pure helper (e.g. tray label `mute_menu_label(exe, listed) -> String`) — only if extracted.
- [ ] Step 2: clippy/fmt/test clean; `cargo build --release`; exe size ≤ 500 KB (else stop, report).
- [ ] Step 3: Manual smoke (report honestly what can't be driven): with `mute=` empty, `tasklist /m mmdevapi.dll` does not list fps-overlay and no `muted.txt`; with a playing app listed and in background ⇒ muted (`muted.txt` lists it); Exit from tray ⇒ unmuted, `muted.txt` gone; `taskkill /f` then relaunch ⇒ re-adopted. Idle private bytes reported for both cases.
- [ ] Step 4: README (config table row `mute`, tray item, muted.txt, footprint), AGENTS.md (deps, modules `mute.rs`/`audio.rs`, `WM_AUDIO`, timer ids, invariants). Commit `feat: mute listed apps while unfocused`.

### Task 6: Start-with-Windows overlay bug (after merge to `main`)

**Review:** per-task — concurrency/startup

**Files:** determined by diagnosis; expected `src/main.rs` (single-instance), possibly `src/etw.rs`, `src/autostart.rs`.

- [ ] Step 1: Diagnose with superpowers:systematic-debugging. Evidence to collect: number of fps-overlay processes right after logon (simulate with `schtasks /Run /TN "FPS Overlay"` while one instance runs, and a debug build logging `Etw::start` result + `ProcessTrace` return); confirm whether a second start kills the first's ETW session (`logman query FPSOverlayRs_ETW -ets` before/after).
- [ ] Step 2: Write the failing regression check for the proven cause (pure logic test if extractable, else a scripted repro in the report).
- [ ] Step 3: Fix. Baseline expectation: named mutex `Local\FpsOverlayRs` acquired before `Etw::start`; second instance exits silently. Add only what the diagnosis proves beyond that.
- [ ] Step 4: Verify: two quick launches ⇒ one process; `schtasks /Run` while running ⇒ still one, overlay still shows on game focus. Commit `fix: single instance so a second start can't kill the ETW session` (wording per actual cause). Update README/AGENTS if behaviour changes.

## Ordering
0 → 1 → 2 → 3 → 4 → 5 sequential (shared `main.rs`/`Cargo.toml`, and one build at a time per user). Merge `feat/background-muter` to `main` after final review. Task 6 on a new branch `fix/autostart-overlay` from `main` after that merge.
