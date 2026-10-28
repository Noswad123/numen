# Numen roadmap

## Overview

Delivered: multi-familiar foundation, macOS Go to mouse, familiar-first fzf menu/Wisp adapter, opt-in action installer, menu Open empty bootstrap and graceful graphical quit. Checked items mean implemented behavior, not complete desktop acceptance or automatic application integration. Unchecked work has no assigned dates. See [Wisp](WISP.md) for opt-in user setup; no actual home Wisp configuration was edited during delivery.

## Delivery checklist

### Wisp/menu and startup

- [x] Numen-owned submenu adapter for Wisp's existing generic launcher; no Wisp-source changes.
- [x] Generate a Numen **Summon** action for `wisp palette`, with absolute executable/socket argv and **no key**. No WM-prefix/static hint binding is auto-added.
- [x] Print TOML only by default; explicit `wisp-action --install` appends with comment/entry/permission preservation, snapshot + live-inode backups and no-clobber publication/recovery. Refuse different existing Numen entries; no `--replace`.
- [x] Familiar-first multi-select picker includes running/waiting instances and available presets with distinct typed identities and labels.
- [x] Apply one command to the entire selected set via capability intersection: presets Open only; active Info/Go to Mouse/Attach/Close as supported. Explain no-common-command selections rather than silently skipping rows.
- [x] Fresh target picker per selection, excluding other owners and earlier pending choices, allowing own-current reselection. Empty lists explain protocol-only discovery and offer Refresh/Back.
- [x] Retain familiar selection on command Escape/Back; target Escape/Back discards the entire pending batch. Ctrl-R refreshes the current stage. One accepted action exits.
- [x] Menu Open can start detached current-executable `run --empty`, with no unattached Main, private service log and bounded readiness. Discovery alone never starts/writes; `--no-start` opts out. Concurrent starts reuse the UDS winner; unsupported/unsafe existing services are never replaced or killed.
- [x] `service-info` reports control metadata; `quit` gracefully stops current graphical services, including zero-window services, without stopping source apps.
- [ ] Actual Wisp palette → Kitty launcher handoff acceptance. Installer parsing and real fzf PTY tests do not prove this handoff.
- [ ] Automatic Djinn application/open-session discovery and publishing adapter.
- [ ] Supported application-instance adapters, followed by an unrelated runner. Arbitrary PIDs/process-name matches are not eligible targets.

### Multi-familiar foundation

- [x] One service manages up to 16 independently bound views, each with a service-lifetime ID and exact protocol activity pin.
- [x] Independent bindings, cues, summary caches, pause, Player, bubble timers and native/GPU resources; shared atlas and parsed font with private glyph caches.
- [x] Atomic Open/Attach/Close with one distinct target per selection; source execution is never started/stopped.
- [x] Disconnected familiars wait with exclusive retained attachments rather than disappearing or switching.
- [x] Global Option+I registered once per process; targeted/global transient reveal and hard privacy disable retained.
- [x] Last-close retains service/hotkey for later Open; global quit is separate.
- [ ] Independently configurable characters, screen placement and per-view options.
- [ ] Persistent profiles/preferences, general installation/autostart and packaging.
- [ ] Per-process/app-signaled follow-selected modes; no OS-focus inference. See [Modes](MODES.md).

### Go-to-mouse behavior

- [x] Sample the live pointer while pursuing, not just the command-time position; roughly 60 Hz hard deadline gates prevent early-event acceleration.
- [x] Left/right running art, fractional native motion and bounded non-convergence cancellation.
- [x] Contact requires compositor-matched physical sprite alpha ≥32, held pose and actual native-position confirmation, excluding bubble/window padding and discarded source pixels.
- [x] Stop on first arrival, play success once and resume the latest attached emotes without expired cues or source-state mutation. No ongoing following after arrival.
- [x] Reissue restarts intent. Drag/digits/right-click/L/Tab/close cancel without success; reveal does not. Space pauses motion/animation, not bubble deadlines; occlusion alone does not suspend pursuit/celebration.
- [ ] Hands-on moving-pointer pursuit, mixed-DPI/negative-coordinate monitor crossings, forced native occlusion, edge-clamped/refused motion, cancellation and pause/resume acceptance.
- [ ] Other-platform navigation/desktop validation, gaze and roaming.

## API / Interfaces

Delivered CLI: `menu [--no-start] [--fzf PATH]`, `wisp-action [--install] [--file PATH]`, `service-info`, `quit`, offline `presets`, paged `familiars`/`targets`, scoped `status`/`focus`, targeted/global `bubbles`, Open/Attach/Close and `go-to-mouse --familiar ID1 ID2`. Direct CLI/wire Open requires a running graphical service; only menu Open bootstraps. `run --empty` conflicts with manual/demo. Presets and active IDs are distinct; only Djinn exists. The CLI and menu share `menu::presets` as catalog source of truth.

GUI rows advertise Info/Attach/Close and Go to Mouse only with an installed navigation callback. Headless/legacy rows advertise no GUI capabilities. `Server::start_graphical` takes view/reveal callbacks, optional navigation, quit callback and a bootstrap-main boolean; old constructors remain test compatibility helpers. `companion.service` returns `{api_version:1,pid,service_instance,graphical,lifecycle,navigation}`, not a shutdown flag or GPU acknowledgment. The menu pins that generation so restarted services cannot inherit stale selections. `companion.quit` requires a shutdown callback and returns scheduled `quitting: true`, not completion.

## Data Flow

Wisp palette → Numen Summon action → familiar/preset multi-selection → command → one fresh protocol target per selection where needed → atomic backend revalidation → complete registry snapshots → independent native views. Wisp invokes Numen's interface, not a duplicate store/renderer. Current targets are connected protocol-advertised activities only, not OS scans or automatic Djinn sessions.

One canonical activity store feeds per-familiar exact bindings/cues/retained summaries. Only ordinary bootstrap `main` auto-selects a first eligible activity; empty bootstrap has no `main`, even after source updates. Future adapters should expose supported app instances/open sessions directly, without mandatory process-to-conversation drill-down, and must isolate publishing from source execution, recover via snapshots and tolerate absence/slowness.

## Edge Cases

- Open is attached, not an unattached-pet/history-restore flow. Close removes a familiar, not its app, and there is no separate Hide familiar action. Automatic thought-bubble hiding remains separate. Multiple bubbles/views may overlap with arbitrary stacking; no collision/fanning policy is required.
- Each exact `(source, instance, activity)` belongs to at most one familiar, including disconnected waiting pins. Different sessions/instances of one app can have separate familiars. Pending target reservations prevent duplicate choices; backend mutation revalidates atomically. Attach cannot swap another owner's target even within one batch.
- Up to 16 active views. Generated IDs are monotonic for service lifetime without generated-history growth; explicit history is bounded to 256 plus reserved `main`. Closed IDs are not reused; restart resets state. Closing/omitting Main never redirects legacy scope to another familiar or recreates it.
- Lifecycle/navigation/reveal ACKs mean accepted, not rendered/arrived. Async native/GPU failures remove only the bad view with close retry; shared font initialization remains fatal. Startup readiness is control-only. Later Open never requests focus; initial Main can opt in. Global exit deadlines survive zero windows.
- Quit callback follows ACK flush plus client EOF (or 500 ms); later pipelined input is ignored. Root event-loop exit/drop cleans up all views, hotkey and owned socket, never source apps or a metadata PID. Cancelling Open after startup can leave an empty service resident for explicit quit.
- Installer publication is preservation-first, **not atomic CAS**: the catalog pathname may briefly be absent. Snapshot file and parent are fsynced before displacement; the actual current inode is retained and rechecked, then the completed new file is linked with no clobber. Recreated external paths are not overwritten. Inspect both backups and close editors before manual recovery; never blindly discard live edits. See [Wisp recovery](WISP.md#edge-cases).
- Existing font size 12, half-size sprite, Source Code Pro fallback, transient bubbles, `--no-bubble` privacy semantics and command-scoped macOS SDK workaround are unchanged. Different characters/options and persistent profiles remain future work.

## Validation

Verified: **224 automated tests pass (206 unit + 18 CLI)**, plus formatting, strict clippy, debug build and diff checks. Real fzf PTY checks cover bootstrap/Open, multi-selection Info, cancellation and detached-service survival; native zero-window quit cleans up its socket. Generation-scoped commands reject stale selections after restart. Actual Wisp palette → Kitty handoff remains a manual acceptance check, and the home action catalog has not been installed by this implementation session.

Real fzf PTY validation auto-started an empty native GUI from Preset Open, registered a diagnostic source, selected its target, opened one familiar with no Main and exited the menu while the detached service stayed alive. A second real fzf menu multi-selected two familiars for Info with source JSON unchanged. An existing-empty-GUI test exercised target/command/familiar Escape with no mutation and selection retained at command Back. Graceful quit ACKed/socket cleanup passed, including a separate native zero-window case. Only temporary test catalogs were installed; actual installed `wisp actions list` parsed the Numen row. No real Kitty/WM-prefix handoff or home catalog installation was performed.

Earlier three-view lifecycle and two-window native GPU Go to Mouse evidence remains valid: running art, confirmed arrival, success once, latest-emote restoration and unchanged source state. No global OS key/pointer injection, warp, event posting/tap or permission request was used; scripted PTY inputs affect only terminal interaction. Other-platform navigation, hands-on controls/focus/compositor checks and injected GPU failures remain in the [README acceptance checklist](README.md#validation).

## Example usage

```sh
~/Projects/numen/target/debug/numen menu
~/Projects/numen/target/debug/numen service-info
~/Projects/numen/target/debug/numen quit
# Preview only, no write:
~/Projects/numen/target/debug/numen wisp-action
# Optional user command after delivery, not performed automatically:
~/Projects/numen/target/debug/numen wisp-action --install
wisp palette
```

Select Numen in the palette, then familiars/presets, then a command. Open/Attach CLI batch files remain root JSON arrays with flattened targets and item field `id`, not `familiar`; see [Protocol](PROTOCOL.md).

## Related files

- [README](README.md), [Architecture](ARCHITECTURE.md), [Protocol](PROTOCOL.md), [Modes](MODES.md), [Wisp](WISP.md).
- [`src/menu.rs`](src/menu.rs), [`src/bootstrap.rs`](src/bootstrap.rs), [`src/wisp_action.rs`](src/wisp_action.rs): menu, startup and opt-in installer.
- [`src/main.rs`](src/main.rs), [`src/window.rs`](src/window.rs), [`src/ipc.rs`](src/ipc.rs), [`src/activity.rs`](src/activity.rs), [`src/protocol.rs`](src/protocol.rs): CLI, GUI and service contracts.
- [`src/motion.rs`](src/motion.rs), [`src/pointer.rs`](src/pointer.rs): bounded pursuit/read-only pointer contract; [coordinate details](ARCHITECTURE.md#navigation-and-coordinate-contract).
