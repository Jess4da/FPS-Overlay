# FPS Overlay — Mute Hotkey — Design

## Intent
User plays a game (e.g. EAFC) with music playing in another app and wants to silence the game instantly without leaving it. A hotkey, default **Alt+M**, mutes the app of the focused window; pressing it again unmutes. Builds on the background muter (`2026-11-15-background-muter-design.md`), whose non-goal "hotkey for muting" this design lifts.

## Goals
- `Alt+M` (configurable) toggles a **hold** on the foreground exe: held exes are muted whether focused or not.
- Hold survives alt-tab and the game closing/reopening (matched by exe name, like `mute`). It ends only on a second press or when the overlay exits.
- Exit (every path that calls `shutdown`) unmutes what the hold muted; a crash is recovered via `muted.txt` on next start.
- Never clears a mute the user set themselves (Volume Mixer): a press on an already-muted app takes no ownership; a second press does not unmute it.
- Configurable: `mute_key` (virtual-key code, `0` = hotkey off) and `mute_mod` (`none|alt|ctrl|shift`). Settings window edits both.

## Non-goals
On-screen "muted" indicator; working outside games; persisting holds across restarts; a hotkey to add an app to the `mute` list.

## Anti-goals
- A game left muted by us after we exit or crash.
- Any idle cost: no global hotkey, no keyboard hook, no polling outside games, no COM/audio until the first press (when `mute` and `muted.txt` are empty).
- Swallowing Alt+M from the game (non-consuming, like Insert/End).

## Decided (user)
- Option 1: "held" set inside `Muter`, reusing owned/stale/`muted.txt`.
- Works only while a game is foreground (same gate and `GetAsyncKeyState` polling as `toggle_key`/`exit_key`; `ignore`d exes never qualify, so the hotkey does nothing there).
- Held until pressed again or overlay exit.
- Default `mute_key=77` (M), `mute_mod=alt`. "(none)" key in Settings disables it.

## Architecture
- **`src/mute.rs`** — `Muter` gains `held: HashSet<String>` (exe names).
  - `toggle_hold(&mut self, exe: &str) -> bool`: flips membership, returns the new state.
  - `is_holding(&self) -> bool`: `!held.is_empty()`.
  - `reconcile`: a session is *wanted muted* when `(listed && fg != exe) || held.contains(exe)`. Wanted ∧ unmuted ⇒ mute + own. Not wanted ∧ owned ⇒ unmute (failure ⇒ stale). This generalizes today's "listed and focused ⇒ unmute owned"; owned-but-unlisted sessions could not exist before (reconfigure releases first), so list behaviour is unchanged.
  - Stale adoption drops its `listed` condition: any stale exe seen muted is re-adopted, then unmuted by the rule above unless wanted. Required so a held game muted before a crash is unmuted on the next start (its exe is not in `mute`). Changes one ported behaviour: an exe that was dropped from `mute` while the app was not running, but left muted by us, is now unmuted on start (the anti-goal "left muted by us" says it should be).
  - `release` unchanged (unmutes owned; holds stay in memory). `persisted()` unchanged (owned ∪ stale): a held exe we muted is written to `muted.txt` as *owned*, which is what lets the next start adopt and unmute it. Hold *membership* is never persisted: holds do not survive a restart.
  - Known limit (same as the listed path today): if the user mutes a game themselves after a crash, before it is seen again, stale adoption treats that mute as ours and unmutes it.
  - `retain_holds(&mut self, keep: impl FnMut(&String) -> bool)`: drops holds that can no longer be released (see `reconfigure`).
- **`src/config.rs`** — `pub enum Modifier { None, Alt, Ctrl, Shift }` with VK `0x12`/`0x11`/`0x10`. `Config.mute_key: u8` (default 77, parse accepts 0–254), `Config.mute_mod: Modifier` (default `Alt`). `to_ini` writes `mute_key=` and `mute_mod=` after `exit_key`; `DEFAULT_INI` gains both. Bad values fall back per key.
- **`src/main.rs`**
  - Hotkey: in `App::hotkeys`, the mute chord is down when `mute_key != 0`, the key is down, and the modifier (if any) is down. Edge-triggered on the chord, armed with the others. `edges` generalizes to `[bool; 3]`.
  - Press ⇒ `App::toggle_hold()`: if `fg_exe` is `None` do nothing; ensure `MuteState` exists (create it even when `mute` is empty), `toggle_hold(exe)`, reconcile, save.
  - `mute_wanted(list_empty, persisted_empty, holding)`: also true while holding, so a hold on a silent game (no session yet) keeps `MuteState`; unholding the last one with nothing else wanted drops it.
  - `reconfigure` keeps its release-then-reconcile; a held game is briefly unmuted and re-muted when Settings is saved. ponytail: accept the blip; skip held exes in that release if it bites. `MuteState` (and its holds) is dropped only by `drop_mute`, i.e. on exit or when nothing is wanted.
  - `reconfigure` also drops holds that could never be released again (builder-check finding; my call): all holds when the new `mute_key` is 0, and holds on exes now in `ignore` (an ignored exe never becomes a game, so its chord is never polled). Their sessions unmute in the same reconcile.
  - Tray mute item stays list-only (my call): it still offers "Mute X in background" for a held exe and edits only `mute`. No hold state in the tray.
- **`src/settings.rs`** — Mute tab gains a "Mute focused game" row: modifier combo (`None`, `Alt`, `Ctrl`, `Shift`) + key combo (`(none)` = 0, then the existing `KEYS`, then `A`–`Z`, then the existing "Key code N" entry for any other configured value so Save never rewrites it). `Fields` gains `mute_key: u8`, `mute_mod: usize`. `from_fields` rejects `mute_key` equal to `toggle_key` or `exit_key` (any modifier: Insert alone would still toggle on Alt+Insert). The error names both keys, since they sit on different tabs.

## Settings window: tabs and sliders (user request, same branch)
- Two tabs via the native tab control (`SysTabControl32`): **Overlay** (Appearance, Steam-like bar, Game detection, Hotkeys show/hide + exit) and **Mute** (background mute list + Add…, mute hotkey). Below the tabs, shared on both: Start with Windows, Save, Cancel. Save validates and writes both tabs; an error on the hidden tab only names the field (no auto-switch; add if it bites).
- Pages are child controls of the settings window shown/hidden on tab change (`TCN_SELCHANGE` via `WM_NOTIFY`). Hidden controls are skipped by Tab. The tab strip is a Tab stop; arrow keys switch pages. No Ctrl+Tab.
- Window height = tallest page + tab header + bottom row (builder sizes it).
- Number inputs get a native slider (`msctls_trackbar32`) beside the existing number box, two-way synced:

  | Field | Slider range | Box accepts (unchanged validation) |
  |---|---|---|
  | Scale (%) | 50–200 | 50–200 |
  | Background opacity (%) | 0–100 | 0–100 |
  | Minimum FPS | 1–240 | 1–1000; above 240 the slider sits at its right end |

  Arrow keys step 1; Page Up/Down and track clicks step 10. Slider move (`WM_HSCROLL`) sets the box text; box edit (`EN_CHANGE`) sets the slider position, clamped, ignoring unparsable text. `TBM_SETPOS` does not notify, so no loop.
- Raw `TCM_*`/`TBM_*`/`TCN_*` constants like the existing `BS_*`/`CB_*`; no `Win32_UI_Controls` feature. Unverified: that both classes create without `InitCommonControlsEx` under the existing v6 manifest. If not, add the minimal `Win32_UI_Controls` feature for `InitCommonControlsEx(ICC_TAB_CLASSES | ICC_BAR_CLASSES)` called in `settings::open` only, so `comctl32.dll` is still not touched until Settings opens.

## Constraints
- AGENTS.md rules stay binding: two dependencies, exe ≤ 550 KB, idle budgets, no `RegisterHotKey`/hooks, never nest `with_app`.
- Tests: `Muter` hold behaviour and crash recovery, config round-trip/fallback, `from_fields` conflicts, `edges` for three keys. No wiring tests.
- Docs: README (hotkey table, config table), AGENTS.md (hotkeys/mute invariants, data-flow block), background-muter spec (non-goal + pointer here), overlay spec hotkey section.

## Left to the builder
Settings layout coordinates, label wording, internal helper names.
