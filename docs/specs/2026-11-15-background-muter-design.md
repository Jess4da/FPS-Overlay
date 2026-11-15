# FPS Overlay — Background Muter — Design

## Intent
Fold BGMuter (`D:\Coding\Rust\BGMuter`, merged `c453dc3`) into FPS Overlay so one tray app does both: overlay over the focused game, and mute chosen apps while they are not focused. Port BGMuter's tested core and WASAPI layer; rebuild its UI the FPS Overlay way (native Win32, `config.ini`, no extra GUI/serde crates). Then fix the separate bug "Start with Windows launches it but the overlay never shows until exit + relaunch".

## Goals
- `mute=` list of exe names in `config.ini` `[Overlay]` (comma list, lowercased, like `ignore`). A listed app is muted while it is not the foreground app and unmuted when it is focused again. Unlisted apps are never touched.
- Independent of the game gate: muting does not require FPS detection or a candidate, and `ignore` does not affect it.
- BGMuter guarantees kept verbatim (port, not rewrite):
  - Only unmute sessions BGMuter muted (owned set); a mute the user set themselves is respected.
  - Lost ownership (session gone, unmute failed, crash, logoff) is persisted and re-adopted (`stale` exes), so a game is never left muted by us.
  - Reapply on: foreground change, audio session created, audio device change, session unlock, config change.
- Settings window: new "Mute in background (exe names, comma separated)" edit + `Add…` picker (same running-windowed-apps menu as the ignore list). Saved and applied live.
- Tray: "Mute `name.exe` in background" / "Stop muting `name.exe`" for the last foreground app with a window (not our own), applied live.
- Fix: Start with Windows must show the overlay on first game focus without a manual restart.

## Non-goals
Auto-muting every detected game; per-app volume (only mute); fade; hotkey for muting; UWP apps (foreground is `ApplicationFrameHost.exe`).

## Anti-goals
- A game left muted by us after it exits, after we exit, crash, or after logoff.
- Clearing a mute the user set.
- Any idle cost when `mute` is empty: no COM, no audio DLLs, no timer.
- Breaking existing overlay budgets beyond what the user approves (see Constraints).

## Decided (user)
- Merge into FPS-Overlay; muting by explicit list.
- `windows-core = "0.62"` allowed as a second direct dependency (already compiled transitively; needed for `#[implement]` COM sinks). AGENTS.md updated to "windows + windows-core".
- User's uncommitted ETW debug logging was stashed (`stash@{0}`), not part of this work.
- BGMuter repo stays as-is.
- Autostart fix happens after the muter work is merged to `main`.

## Architecture
New flat modules, declared in `src/main.rs`:
- `src/mute.rs` — pure core, ported from BGMuter `src/core.rs` unchanged in behaviour: `Session`, `trait Mixer`, `Muter { owned: HashMap<id, exe>, stale: HashSet<exe> }` with `with_stale`, `persisted`, `reconcile(m, list, fg: Option<&str>, paused=false)`, `release`. All BGMuter core tests ported. `paused` parameter dropped (no pause feature here).
- `src/audio.rs` — `WasapiMixer` ported from BGMuter `src/audio.rs` (COM sinks only `PostMessageW(MAIN_HWND, WM_AUDIO, kind)`; `refresh_endpoints`; skip session on `GetMute` failure). Uses `crate::process::exe_name`.
- Persisted ownership: `muted.txt` next to the exe (same dir as `config.ini`), one exe per line, atomic tmp+rename, deleted when empty. Functions `config::load_owned()` / `config::save_owned(&BTreeSet<String>)`.
- `App` gains `mute: Option<MuteState>`; `MuteState { mixer: WasapiMixer, muter: Muter, saved: BTreeSet<String> }`.
  - Created lazily (CoInitializeEx STA + `WasapiMixer::new`) the first time `cfg.mute` is non-empty or `muted.txt` is non-empty at startup; dropped (after `release` + save) when `mute` becomes empty and nothing is stale. COM init once per process (`CoUninitialize` at exit only).
  - Foreground exe comes from the existing `App::foreground` (`process::exe_name`); our own process counts as "other" (fg = `None` mutes all listed).
  - Debounce: `App::foreground` arms a one-shot `SetTimer(MUTE_TIMER_ID, 150 ms)` only when `mute` is `Some`; `WM_TIMER` with that id kills it and reconciles. Absorbs alt-tab flicker. Not a polling timer.
  - `WM_AUDIO` (`WM_APP + 3`): `NOTIFY_DEVICE` ⇒ `refresh_endpoints` then reconcile; `NOTIFY_SESSION` ⇒ reconcile.
  - `WM_WTSSESSION_CHANGE` / `WTS_SESSION_UNLOCK` (register with `WTSRegisterSessionNotification` only while `mute` is `Some`) ⇒ reconcile.
  - `reconfigure(cfg)`: `release` before applying the new list (so removed apps unmute), then reconcile.
  - After every reconcile/release, save `muted.txt` when `persisted()` changed.
  - Exit paths: normal loop exit, `End` hotkey, tray Exit, `WM_ENDSESSION`, update restart ⇒ `release` + save before `App` drops (and before ETW stop is fine either order). Release profile is `panic = "abort"`: a panic cannot unwind, so `muted.txt` is the crash recovery, as in BGMuter.
  - Reentrancy: all mute work runs inside `with_app`; nothing in it pumps messages (WASAPI calls don't). Never nest `with_app`.
- Settings: new `ID_MUTE` edit + `ID_ADD_MUTE` button under a "Background muting" group; `Fields` gains `mute: &str`; `from_fields` fills `cfg.mute` via the same list normalizer; picker reuses `candidates`/`collect` but excludes already-muted exes. Window grows to fit.
- Config: `Config.mute: Vec<String>`, default empty, `mute=` written by `to_ini` (after `ignore`), parsed by `set` with `ignore_list`; `add_ignore` generalised or a twin `add_mute`. `DEFAULT_INI` gains `mute=`.

## Autostart bug (fix after merge)
Symptom (user): logon start runs the exe, tray present, overlay never appears until Exit + manual relaunch.
Evidence found during planning (`%TEMP%\fpsdbg.txt` from the stashed debug build): two interleaved `now=` streams with different start times — two instances running at once. There is no single-instance guard, and `Etw::start` stops any existing `FPSOverlayRs_ETW` session **by name** on `ERROR_ALREADY_EXISTS`, which kills the other live instance's session.
Hypotheses to verify first (systematic-debugging, before any fix):
1. Duplicate instances at logon (task + something else, or `StartWhenAvailable` firing twice), second start stopping the first's ETW session; the surviving tray belongs to the dead-session instance.
2. Logon start runs before the desktop/DWM/display is ready, so the first overlay/DPI/monitor setup is wrong for the session.
3. Task runs with `HighestAvailable` but the ETW realtime consumer dies at logon (ProcessTrace returns early) and is never restarted.
Expected fix shape (decided after diagnosis): named mutex `Local\FpsOverlayRs` single-instance guard (second instance exits silently before touching ETW), plus whatever the diagnosis proves (e.g. restart the consumer when `ProcessTrace` returns while the app is alive, or a logon `<Delay>`). Regression test for any pure decision logic extracted.

## Constraints
- AGENTS.md rules stay binding except the one-dependency rule (now `windows` + `windows-core`).
- Exe budget is ≤ 500 KB; last measured build 481 KB. Measure after the muter lands; if it exceeds 500 KB, stop and ask the user before trimming or raising the budget.
- Idle with `mute` empty: unchanged budgets (0 timers, ≤ 5 MB). Idle with `mute` non-empty: event-driven only; private bytes measured and reported (audio COM adds some).
- New `windows` features only as needed: `Win32_Media_Audio`, `Win32_System_Com`, `Win32_System_Com_StructuredStorage`, `Win32_System_Variant`, `Win32_System_RemoteDesktop`.
- Tests: pure core + config round-trip (`parse(to_ini(c)) == c` with `mute`), `from_fields` with mute; no wiring tests.
- Docs: README config table + tray + footprint, AGENTS.md (deps, modules, messages), this spec.

## Left to the builder
Exact settings layout coordinates, tray wording details, module-internal helper names.
