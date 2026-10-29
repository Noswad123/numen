# Numen roadmap

## Overview

Delivered: multi-familiar foundation, macOS Go to mouse, familiar-first fzf menu/Wisp adapter, opt-in action installer, menu Open empty bootstrap and graceful graphical quit. The sibling Djinn source now has an optional ordinary-interactive adapter. Checked items mean implemented behavior, not complete desktop acceptance or general application discovery. Unchecked work has no assigned dates. See [Wisp](WISP.md) for opt-in user setup; no actual home Wisp configuration was edited during delivery.

## Delivery checklist

### Read-only doctor and current desktop feedback

- [x] `numen doctor` human output and `--json`: own client executable/embedded build versus responding service PID/executable/embedded build; one bounded read-only `companion.service` query, no startup/signals/directory or log creation/reveal/settings/registration/TTL changes.
- [x] Additive service-info v1 settings boolean, logical familiar-registry/registered-source counts and diagnostics version 1; live shared hotkey registration facts with allowlisted states/reasons, not a frozen startup snapshot or native-window/process counts. No raw argv/environment/summaries.
- [x] Recorded-build comparison `match`/`different`/`unknown`, not executable equality or newer/older. Same version/path alone is insufficient. Source/build fingerprint coverage and nonhermetic/Cargo-cache limits documented in [Architecture](ARCHITECTURE.md#recorded-build-identity).
- [x] Legacy diagnostics absent → legacy/unknown; future diagnostics → unsupported/unknown; malformed current fields, contradictory known statuses and duplicate JSON keys fail. Missing/refused socket → not_running exit 0; offline/legacy/unsupported/different informational 0; unsafe/error/timeout/malformed 1.
- [x] **USER-REPORTED:** “I verified everything working fine” — current desktop familiar workflow, including relevant click/menu/settings/hotkey use, working. This is user feedback, not automated or exhaustive acceptance of the edge cases below.
- [x] Main-agent final doctor validation: 307 passed (270 binary unit + 7 build-fingerprint integration + 30 CLI), one existing visual-preview test ignored; formatting, strict all-target Clippy, offline locked tests/build and diff checks passed with the macOS 26.5 SDK workaround. Live installed-binary private-socket and read-only default-socket checks passed; details below. Historical totals are separate.
- [x] Main-agent latest debug install via `cargo install --path . --debug --locked --offline --root ~/.local --force`; bare `numen` resolves to `~/.local/bin/numen` and doctor works. No home configuration changes or user-app restarts. Updated doctor works with legacy services immediately; one user-chosen Numen restart is needed only to add diagnostics to an old running service, never automatic. No Djinn restart required.

Historical validation/manual-check statements below describe their original scope. The user report does not verify mixed-DPI, Wisp palette/external handoff, real OS collision/failure cases or other unexercised acceptance scenarios.

### Native familiar click menu (macOS)

- [x] Normal left-click release opens AppKit `NSMenu`; right-click opens the same menu instead of Preview cycling. Digits 1–9 unchanged; non-macOS retains left-drag/right-cycle. Separate from existing fzf/Wisp menu; both remain.
- [x] Five-logical-point held-drag slop; retain/revalidate original `NSEvent` `LeftMouseDown` and native window. CursorLeave cancels click only, preserving held drag; mouse-up coordinates cannot begin drag without physical button-down. Poll held/native-drag state every 20 ms for lost mouse-up.
- [x] Typed declarative extensible action catalog: Show status (`--no-bubble` respected), clicked-only Go to mouse (pointer/navigation required), clicked-only Pause/Resume, Settings, Close familiar, and explicit Quit Numen (all familiars). Last Close retains service; Quit uses normal root shutdown and never stops Djinn.
- [x] Shared three existing Settings fields/preset sets; one-field patch through nonblocking 32-entry `FamiliarControl::settings` → canonical Store → authoritative snapshot, no optimistic projection. Checks/Custom numeric labels use canonical `f64`, not renderer `f32`. No settings IPC/navigation in manual/demo; pause, permitted reveal, Close/Quit remain usable.
- [x] Pending gesture/menu holds geometry and suspends motion clocks without clearing cue/manual override/pause/navigation; committed drag cancels motion and rebases hidden anchor. Revalidate latest registry and familiar/WindowId after tracking; ignore stale/failed/closing per-view actions. Service Quit does not depend on target survival.
- [x] Synchronous native tracking suspends Rust ticks; 25 ms `NSTimer` cancels on clicked-target removal, external quit or service `--exit-after`, not ordinary source updates. No source-driven auto-popup/execution authority, input taps, synthetic keys or permission requests; direct objc2/AppKit dependencies use already-locked versions.
- [x] Input-free production-adapter native popup construction/placement API and timer autocancellation: three main-thread runs in an independent temporary AppKit/winit window (380/378/353 ms), repeated tracking cleanup, exit 0/no errors. No physical input/item-selection claim.
- [x] Focused checks, formatting, strict Clippy and updated binary build green after latest poll/click-race fix.
- [x] Main-agent full confirmation after latest poll/click-race fix: 278 passed (257 unit + 21 CLI), one existing visual-preview test ignored; formatting, strict all-target Clippy, offline locked tests/build and diff check passed with the macOS 26.5 SDK workaround.
- [ ] Extended interaction acceptance beyond the user-reported working desktop flow: mixed-DPI/edge dragging, focus transitions, stale-target/cancellation races and fault cases. The normal workflow is accepted by user feedback above, not by automated physical-input tests.

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
- [x] Optional Djinn local owning-Worker ordinary interactive adapter: default-on discovery in updated builds for explicit Chat-tab selections and tracked busy chats, existing idle-chat admission, title-only summaries, current-state snapshots and reconnects. Plain `djinn`, `djinn -s REF` and `djinn session chat REF` need no enablement export. `DJINN_NUMEN_DISABLED=1` opts out of reporting; legacy `DJINN_NUMEN=0` remains supported. See [scope/setup and exact boolean policy](README.md#optional-djinn-interactive-adapter).
- [x] Authoritative response-owned/settled outcomes in that adapter: completed response → `succeeded` / `response_completed`, final assistant non-abort error or failure/defect → `failed` / `run_failed`, winning user cancellation → `cancelled` / `run_cancelled`. Response completion is not task-success proof. Internal channel only; no public Schema/SDK or Numen Rust renderer changes.
- [x] Real interactive idle-chat handoff smoke: source Djinn TUI → real fzf Open/target picker → attached Metal familiar; exit retains unavailable pin, last Close retains service, quit removes socket. No model prompts or Wisp launcher involved; full visual/focus/outcome-animation acceptance remains manual.
- [ ] Broader Djinn coverage (Rust-native ask/session run, mini, external attach, remote web, current core v2 API and arbitrary background/child sessions). No OS scanning is implemented.
- [ ] Supported application-instance adapters, followed by an unrelated runner. Arbitrary PIDs/process-name matches are not eligible targets.

### Multi-familiar foundation

- [x] One service manages up to 16 independently bound views, each with a service-lifetime ID and exact protocol activity pin.
- [x] Independent bindings, cues, summary caches, pause, Player, bubble timers and native/GPU resources; shared atlas and parsed font with private glyph caches.
- [x] Atomic Open/Attach/Close with one distinct target per selection; source execution is never started/stopped.
- [x] Disconnected familiars wait with exclusive retained attachments rather than disappearing or switching.
- [x] Global Option+I registered once per process; targeted/global transient reveal and hard privacy disable retained.
- [x] Last-close retains service/hotkey for later Open; global quit is separate.
- [x] Runtime per-selected-view sprite size, text size and bubble duration: atomic CLI patches, one-field menu presets, complete registry/discovery values and capability/generation guarding; no persistence.
- [ ] Independent characters/variants, placement controls and additional per-view options (native dragging already exists).
- [x] Private-fixture native GUI settings smoke, including paused views: independent resize, shown/hidden foot-center restoration, rapid edits, duration deadlines, real Settings menu and startup-default inheritance.
- [ ] Hands-on mixed-DPI settings resize, visual typography/composition and focus acceptance.
- [ ] Persistent profiles/preferences, general installation/autostart and packaging.
- [ ] Per-process/app-signaled follow-selected modes; no OS-focus inference. See [Modes](MODES.md).

### Go-to-mouse behavior

- [x] Global macOS Option+M sends all current eligible service views to the live pointer; GUI-only broadcast, zero-view no-op and later Open support, no cross-process broadcast or CLI/wire implicit all.
- [x] Independent I/M guards, matching-release repeat latches and collision fallback share one main-thread Carbon manager and capture-free two-weak-target dispatcher. Bounded callbacks queue intent; the main GUI loop applies the latest registry and skips closing/failed views.
- [x] `--no-hotkey` disables both global keys, retaining focused local/CLI controls; `--no-bubble` disables global I only. Exact focused Alt+M moves only that view if global M is unregistered, independently of I. No M/navigation after pointer-init failure, on non-macOS or in manual/demo.
- [x] Bounded private zero-view native Carbon registration and normal-exit cleanup: M-only with `--no-bubble`, both keys normally, neither with `--no-bubble --no-hotkey`, then M again in a fresh process. All four exited code 0 and removed their own sockets. Collision tests are deterministic, not real OS collisions.
- [ ] Extended Option+M matrix beyond the user-reported working normal flow: real OS collisions, repeated press/release, pause, zero views/later Open, different focused applications and local fallback. Registration smoke alone is not physical-delivery evidence.
- [x] Sample the live pointer while pursuing, not just the command-time position; roughly 60 Hz hard deadline gates prevent early-event acceleration.
- [x] Left/right running art, fractional native motion and bounded non-convergence cancellation.
- [x] Contact requires compositor-matched physical sprite alpha ≥32, held pose and actual native-position confirmation, excluding bubble/window padding and discarded source pixels.
- [x] Stop on first arrival, play success once and resume the latest attached emotes without expired cues or source-state mutation. No ongoing following after arrival.
- [x] Reissue restarts intent. Actual drag/digits/non-macOS right-cycle/L/Tab/close cancel without success; macOS menu opening/dismissal and reveal do not. Space pauses motion/animation, not bubble deadlines outside synchronous tracking; occlusion alone does not suspend pursuit/celebration.
- [ ] Hands-on moving-pointer pursuit, mixed-DPI/negative-coordinate monitor crossings, forced native occlusion, edge-clamped/refused motion, cancellation and pause/resume acceptance.
- [ ] Other-platform navigation/desktop validation, gaze and roaming.

## API / Interfaces

Delivered CLI: `menu [--no-start] [--fzf PATH]`, `wisp-action [--install] [--file PATH]`, `service-info`, `quit`, offline `presets`, paged `familiars`/`targets`, scoped `status`/`focus`, targeted/global `bubbles`, Open/Attach/Close and `go-to-mouse --familiar ID1 ID2`. Direct CLI/wire Open requires a running graphical service; only menu Open bootstraps. `run --empty` conflicts with manual/demo. Presets and active IDs are distinct; only Djinn exists. The CLI and menu share `menu::presets` as catalog source of truth.

GUI rows advertise Info/Attach/Close, Settings with live settings support, and Go to Mouse with navigation, independently. Headless/legacy non-multi rows advertise no GUI capabilities. `Server::start_graphical_with_settings` takes view/reveal callbacks, optional navigation, quit callback, a bootstrap-main boolean and complete startup settings; old constructors remain test compatibility helpers without settings support. `companion.service` returns `{api_version:1,pid,service_instance,graphical,lifecycle,navigation,settings,familiar_count,source_count,diagnostics}` in current services, not a shutdown flag or GPU acknowledgment. The menu pins that generation so restarted services cannot inherit stale selections. `companion.quit` requires a shutdown callback and returns scheduled `quitting: true`, not completion. `doctor [--json]` reads this metadata once; see [exact schema and exit behavior](PROTOCOL.md#read-only-diagnostics).

## Data Flow

Doctor: own executable/embedded build → one private bounded service query → live service diagnostics/current logical registry counts → validated human/JSON report. JSON uses `client`, `socket`, `service`, `service_state`, `build_comparison`; build fields are `version`, `fingerprint`, `source_fingerprint`, `target`, `profile`. Comparison covers recorded hashed inputs only, not binary identity or age. Source FNV-1a-128 covers manifests/lock/build scripts/src/assets; build adds compiler/configuration. No Git/timestamp, new dependencies or hermetic external-toolchain/dependency provenance; clean-rebuild if Cargo cache validity is questioned.

Runtime settings: explicit selected IDs → `familiar.settings {ids,patch}` → validate whole batch and root generation precondition → registry-owned complete settings → coalesced `FamiliarPresentation.settings` → per-view renderer/visibility/layout. CLI accepts any nonempty subset of sprite scale 0.25–2, font size 8–32 and bubble duration 0.25–30 seconds, all finite. No implicit all, null/unknown patch keys or partial acceptance. ACK `updated` contains exactly the requested IDs. `familiars` reads complete values; no settings-get CLI exists.

The new `Server::start_graphical_with_settings` takes the legacy graphical arguments plus complete startup `FamiliarSettings`; the legacy test constructor rejects settings. Capability `settings` is independent of navigation. Actual startup options seed immutable future-Open defaults, including empty startup; editing Main never changes them. Edits preserve attachment/cues/source TTL, Preview, pause, player and navigation. Duration changes affect future accepted reveals only, not hidden visibility/current deadlines or quiet/history/disabled/always policy. No persistence, character variants, placement controls or autostart is delivered.

Wisp palette → Numen Summon action → familiar/preset multi-selection → command → one fresh protocol target per selection where needed → atomic backend revalidation → complete registry snapshots → independent native views. Wisp invokes Numen's interface, not a duplicate store/renderer. Targets are connected protocol-advertised activities, including supported Djinn chats discovered by default unless opted out, not OS scans or all historical/background sessions. Discovery does not start Numen, open familiars, publish history, read raw prompts, change Wisp/transcripts or make model calls. Titles/status are shared automatically and may be sensitive; opting out prevents reporting, while `--no-bubble` hides display only.

One canonical activity store feeds per-familiar exact bindings/cues/retained summaries. Only ordinary bootstrap `main` auto-selects a first eligible activity; empty bootstrap has no `main`, even after source updates. Future adapters should expose supported app instances/open sessions directly, without mandatory process-to-conversation drill-down, and must isolate publishing from source execution, recover via snapshots and tolerate absence/slowness.

## Edge Cases

- Default-on discovery requires an updated Djinn build; an already-running older Djinn UI needs one restart with the updated binary, then Chat selection and explicit reattachment if its instance changed. This is separate from the Numen runtime-settings restart below. Source fallback uses Bun from `clients/djinn-ui/packages/opencode` for the OpenTUI/Solid preload, without an enablement flag. Missing enablement/disablement defaults are true/false; parsing is case-sensitive without trimming, accepting `true/yes/on/1/y` and `false/no/off/0/n`. Malformed enablement becomes false, malformed disablement true; effective enablement is enable AND NOT disable in both CLI and backend. Numen remains optional and unsupported execution paths remain unchanged.
- Djinn adapter: backend UUID survives reconnect, not reload/restart; exact pins then require explicit reattachment and reload abandons ownership. Chat presence renews every 5 seconds with a 20-second TTL. Unviewed idle chats are removed; observed busy chats remain tracked. Viewed terminal bases persist until new work/external busy; unviewed observed terminals remain 5 seconds, then are removed. Admission fails closed at 16 current/32 lifetime distinct session IDs; restart backend + reattach resets capacity without ID reassignment. State polling is 250 ms, potentially about 500 ms after an event. Mid-run attach captures the active owner, not settled-history replay; ownership order protects B from late A cancellation and joins yield one outcome.
- Adapter outcomes are not inferred from idle, disconnect, error events, individual tool/model steps or shell completion/nonzero exit. Normal stop/length final assistant completion qualifies as response completion, not task success; missing finish, tool-calls, unfinished/non-assistant results remain neutral. Teardown/interruption without winning user cancellation is abandoned/neutral; final non-abort assistant error/defect takes precedence over cancellation.
- Adapter live cue submission is at most once, not exactly-once GUI animation: service/transport each buffer at most 32 events with 2-second expiry, ACK waits at most 500 ms. The service seals `transport.connection()` at event time; offline, unACKed-registration and prior-generation events become authoritative snapshot-only state. Overflow/expiry/lost ACKs drop cues, never replay on reconnect. Terminal snapshots can show Completed/Failed/Cancelled without historical cues; cancellation never celebrates and existing failure/attention priority is unchanged.
- Open is attached, not an unattached-pet/history-restore flow. Close removes a familiar, not its app, and there is no separate Hide familiar action. Automatic thought-bubble hiding remains separate. Multiple bubbles/views may overlap with arbitrary stacking; no collision/fanning policy is required.
- Each exact `(source, instance, activity)` belongs to at most one familiar, including disconnected waiting pins. Different sessions/instances of one app can have separate familiars. Pending target reservations prevent duplicate choices; backend mutation revalidates atomically. Attach cannot swap another owner's target even within one batch.
- Up to 16 active views. Generated IDs are monotonic for service lifetime without generated-history growth; explicit history is bounded to 256 plus reserved `main`. Closed IDs are not reused; restart resets state. Closing/omitting Main never redirects legacy scope to another familiar or recreates it.
- Lifecycle/navigation/reveal ACKs mean accepted, not rendered/arrived. Async native/GPU failures remove only the bad view with close retry; shared font initialization remains fatal. Startup readiness is control-only. Later Open never requests focus; initial Main can opt in. Global exit deadlines survive zero windows.
- Quit callback follows ACK flush plus client EOF (or 500 ms); later pipelined input is ignored. Root event-loop exit/drop cleans up all views, hotkey and owned socket, never source apps or a metadata PID. Cancelling Open after startup can leave an empty service resident for explicit quit.
- Installer publication is preservation-first, **not atomic CAS**: the catalog pathname may briefly be absent. Snapshot file and parent are fsynced before displacement; the actual current inode is retained and rechecked, then the completed new file is linked with no clobber. Recreated external paths are not overwritten. Inspect both backups and close editors before manual recovery; never blindly discard live edits. See [Wisp recovery](WISP.md#edge-cases).
- Font size 12 and half-size sprite remain startup defaults, now runtime-editable per view. Source Code Pro fallback, transient-bubble policies, `--no-bubble` privacy semantics and command-scoped SDK workaround remain unchanged. Characters/additional options and persistent profiles remain future work.
- Rebuilding does not upgrade an already-running older Numen service: one explicit restart after deployment enables native click menus, Option+M and runtime settings. When ready, use `numen quit`, then `numen menu` → Djinn preset → Open existing chat (same socket, rebuilt binary). No Djinn restart is needed; subsequent runtime edits need no restart. Never automatically quit/replace a user's old service. Existing live user processes were untouched.

## Validation

**Current user feedback:** current desktop familiar click/menu/settings/hotkey workflow is USER-REPORTED working (“I verified everything working fine”). Not automated, not exhaustive mixed-DPI/Wisp/external acceptance. This documentation edit ran no commands/tests, install, app controls, restart or commits; the following doctor results are confirmed main-agent evidence.

**Final doctor validation: 307 passed — 270 binary unit + 7 build-fingerprint integration + 30 CLI; one existing visual-preview test ignored.** Formatting, strict all-target Clippy, offline locked tests/build and diff checks passed with the command-scoped macOS 26.5 SDK workaround. Contradictory known status/reason and duplicate raw ACK key rejection tests passed. Actual Cargo panic/debug-assertions overrides changed the recorded build ID with the same source fingerprint, then restored baseline; not executable equality, age or cryptographic proof.

Live private-socket harness checks used the latest installed debug binary: offline created no directory; empty graphical `--no-hotkey` reported both keys `disabled` / `no_hotkey` with settings/navigation capabilities independent of registration; native `--no-bubble` actually registered M (`registered`, null reason) and disabled I (`disabled` / `no_bubble`); headless reported both `unavailable` / `headless` with logical `familiar_count: 1`, not a native window count. Same client/service recorded fingerprints compared `match`. Fixture producer connect/disconnect changed `source_count` 0 → 1 → 0, with no source registration/mutation from doctor queries and zero auto-created native familiars. All owned services exited 0 and cleaned up sockets.

At validation time, default-user-socket `numen doctor` returned `not_running` and started nothing; this is an observation, not permanent machine state. Latest local debug installation and bare-command resolution are confirmed in the checklist above, with no home configuration change or user-app restart. No hotkey presses or synthetic OS input were used, and no new actual native-input acceptance is claimed. The **278 menu, 255 hotkey and 246 settings** results below are historical, not current doctor totals.

Confirmed after the latest poll/click-race fix with the command-scoped macOS 26.5 SDK workaround: **278 passed (257 unit + 21 CLI), with one existing visual-preview test ignored**. `cargo fmt --check`, `cargo test --offline --locked`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo build --offline --locked` and `git diff --check` all passed; the updated Numen binary was built. These are reported main-agent results, not checks rerun by this documentation edit.

An independent temporary AppKit/winit window used the production `context_menu.rs` popup adapter on the main thread, showing and automatically cancelling three times in **380/378/353 ms** via `Arc<AtomicBool>` and the native 25 ms event-tracking `NSTimer`. It exited 0 without errors. This proves popup construction/placement API use, timer cancellation and repeated tracking cleanup only—not physical click/drag/item selection. No synthetic OS keys/mouse, hotkeys or existing-user-app controls were used. Hands-on Close versus Quit, Esc, focus, mixed-DPI and physical input remain manual. No app restart or home configuration change occurred. The evidence below is historical validation of preceding source-handoff/settings/Option+M features, not physical native-menu acceptance. This documentation edit ran no tests, app controls, hooks or commits.

Final Option+M validation: the full Rust suite passed **255 tests (234 unit + 21 CLI)**, with one existing visual-preview test ignored; formatting, strict all-target Clippy and build passed. The four bounded private zero-view GUI registration cases above passed actual native Carbon registration and normal-exit cleanup, including fresh-process M registration. Both keys succeeded in the normal case; collision behavior was tested deterministically, not through real OS collision. No synthetic OS keys, actual key presses, input taps, permission requests or existing-user-service control/restarts occurred. Actual Option+M OS-chord/background-focus acceptance remains manual, and hotkey-driven motion is not proven; historical CLI-driven native motion evidence is separate. These reported checks were not rerun by this documentation edit. Native unregister is best-effort: on rare OS unregister error the public crate loses its native handle; Rust callbacks release and a warning is emitted, with OS cleanup at process exit. No leak-free native-error guarantee is claimed.

Runtime settings: **246 tests passed (225 unit + 21 CLI)**, one existing visual-preview test ignored, plus formatting, strict all-target Clippy, debug build and diff checks. Private-socket Apple M2 Pro/Metal smoke passed animated and paused: read-only native bounds confirmed selected-only resize, stable window identity and restored hidden foot-center after shown/hidden and rapid size edits. The current reveal kept its deadline and a later reveal used the new duration. Real fzf text-size selection, atomic multi-view updates, invalid-batch rollback, disconnected-view editing and original defaults on later Open passed. All test-owned services quit and removed their sockets. Existing user service/home configuration were untouched; no global input or pointer motion was injected. Mixed-DPI crossings, hands-on typography/composition and focus preservation remain manual checks.

Prior Numen validation: **224 automated tests passed (206 unit + 18 CLI)**, plus formatting, strict clippy, debug build and diff checks; not rerun for this documentation/menu-text update. Real fzf PTY checks cover bootstrap/Open, multi-selection Info, cancellation and detached-service survival; native zero-window quit cleans up its socket. Generation-scoped commands reject stale selections after restart. Actual Wisp palette → Kitty handoff remains a manual acceptance check, and no home action catalog installation is claimed.

Separately reported Djinn validation covers unit/integration tests, package typechecks and headless real Rust wire cues/reconnect behavior; no final focused-test count is asserted. See the [Djinn guide](../djinn/docs/numen.md#checks) for package-local Bun tests/typecheck and opt-in `NUMEN_TEST_BINARY=/absolute/path/to/numen` interop (`1` is a developer-local convenience; prefer an explicit path). Checks were not rerun for this documentation update. Wisp palette → Kitty handoff and full visual/focus/outcome-animation acceptance remain manual. This is not installation, commit, or Wisp acceptance evidence.

On 2026-10-28, the real source Djinn Chat-tab TUI advertised an existing idle session through its owning Worker. The real fzf menu selected Preset Open and that session, and Numen initialized its attached Apple M2 Pro/Metal view with the existing title. TUI exit retained the unavailable exact pin/summary; familiar Close retained the zero-window service; graceful quit exited and removed the socket. The isolated fixture had zero conversation messages. HOME/XDG/SQLite/project/socket and tmux state were temporary; installed binaries and home/Wisp configuration were unchanged. This smoke used `--no-hotkey --normal-window-level`, not a Wisp/Kitty launch or a live model turn.

Real fzf PTY validation auto-started an empty native GUI from Preset Open, registered a diagnostic source, selected its target, opened one familiar with no Main and exited the menu while the detached service stayed alive. A second real fzf menu multi-selected two familiars for Info with source JSON unchanged. An existing-empty-GUI test exercised target/command/familiar Escape with no mutation and selection retained at command Back. Graceful quit ACKed/socket cleanup passed, including a separate native zero-window case. Only temporary test catalogs were installed; actual installed `wisp actions list` parsed the Numen row. No real Kitty/WM-prefix handoff or home catalog installation was performed.

Earlier three-view lifecycle and two-window native GPU Go to Mouse evidence remains valid: running art, confirmed arrival, success once, latest-emote restoration and unchanged source state. No global OS key/pointer injection, warp, event posting/tap or permission request was used; scripted PTY inputs affect only terminal interaction. Other-platform navigation, hands-on controls/focus/compositor checks and injected GPU failures remain in the [README acceptance checklist](README.md#validation).

## Example usage

```sh
numen doctor
numen doctor --json
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

Native macOS example: left-click release on a familiar → Settings → Text size → Large (16). Close familiar retains the service; Quit Numen (all familiars) exits it. Q/Esc remain per-view close, not service Quit.

## Related files

- [`src/doctor.rs`](src/doctor.rs), [`src/diagnostics.rs`](src/diagnostics.rs), [`src/build_info.rs`](src/build_info.rs), [`build.rs`](build.rs), [`build_support.rs`](build_support.rs): read-only diagnostics and recorded build identity.
- [README](README.md), [Architecture](ARCHITECTURE.md), [Protocol](PROTOCOL.md), [Modes](MODES.md), [Wisp](WISP.md).
- [`src/context_menu.rs`](src/context_menu.rs): typed native menu catalog, AppKit tracking cancellation and retained mouse-down drag.
- [`src/menu.rs`](src/menu.rs), [`src/bootstrap.rs`](src/bootstrap.rs), [`src/wisp_action.rs`](src/wisp_action.rs): menu, startup and opt-in installer.
- [`src/main.rs`](src/main.rs), [`src/window.rs`](src/window.rs), [`src/ipc.rs`](src/ipc.rs), [`src/activity.rs`](src/activity.rs), [`src/protocol.rs`](src/protocol.rs): CLI, GUI and service contracts.
- [`src/motion.rs`](src/motion.rs), [`src/pointer.rs`](src/pointer.rs): bounded pursuit/read-only pointer contract; [coordinate details](ARCHITECTURE.md#navigation-and-coordinate-contract).
