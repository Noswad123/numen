# Numen

![numen](img/numen.jpg)
An independent, event-driven familiar that gives visible expression to what your tools are doing.

**Status:** multi-familiar foundation, macOS Go to mouse/Option+M, runtime settings, native macOS familiar click menu, familiar-first fzf menu, opt-in Wisp action installer, menu Open empty-service bootstrap, graceful graphical quit, and read-only `doctor` diagnostics are implemented. Only the Djinn preset is available; targets come from connected protocol sources, including default-on discovery in updated supported Djinn interactive builds, not OS scanning. **USER-REPORTED:** “I verified everything working fine” confirms the current desktop familiar workflow, including relevant click/menu/settings/hotkey use, as working; this is not automated or exhaustive acceptance evidence. Mixed-DPI, Wisp palette/external handoff and other unexercised cases remain unverified. No actual home Wisp catalog was edited. See [Wisp setup and limitations](WISP.md).

Numen means divine power or presence: an unseen, animating presence in the machine. The CLI spelling is `numen`; a local Cargo debug installation is supported below. General packaging and autostart remain future work.

## Overview

Inspired by the pet feature in the Codex app, but generalized beyond a single application: the companion is its own process. Tools publish semantic activity through the initial local v1 protocol. In the implemented **shared-focused** mode, one visible familiar represents **one selected conversation (activity)**, never an aggregate of every conversation. Background activities remain tracked and queryable but cannot steal its expression or completion cues. The included publisher is an interactive diagnostic, not a stable public SDK or automatic integration.

The source reports **what is happening**; the companion chooses **how to express it**.

Each bubble starts **hidden**, with independent status/summary notification timers (five seconds by default). On macOS, **Option+I (Alt+I)** is registered once per process and reveals all enabled current views without taking focus. `numen bubbles` is the socket alternative; `bubbles --familiar main familiar-1` targets only that subset. Cross-independent-process broadcast is not implemented.

On a navigation-capable live macOS service, **Option+M (Alt+M)** sends all current eligible familiars to the live mouse pointer without activation or focus stealing. They run left/right, stop on sprite contact, celebrate once and restore their latest attached source expression; this is not ongoing following or a source outcome. With zero views it is a no-op; later Open works with the retained registration. Broadcast is GUI-only and service-local: CLI `go-to-mouse --familiar ID1 ID2` still requires explicit IDs.

For example, a source reports `waiting` with the reason `permission_required`, not `play-sad-genie-animation`. Different personalities and rendering surfaces can express the same activity differently.

## Product principles

- Independent of Djinn; Djinn is the first integration, not the owner.
- Local-first, lightweight, and optional.
- Useful ambient awareness without becoming another notification feed.
- No model calls or tokens required for basic behavior.
- Adapters must not impede source execution when the companion is absent or slow; the diagnostic publisher reports errors instead.
- Personality and artwork are separate from the activity contract.
- Observe logical activities, not just operating-system PIDs.

## Example usage

Requires Rust 1.89 or newer (edition 2024) and a graphical desktop for `run`. Rendering uses winit 0.30.13, pixels 0.17.2, and image 0.25.8.

```sh
cargo run --locked -- check-assets
cargo run --locked -- run
cargo run --locked -- run --bubble-duration 3 # Timed notifications and reveals
cargo run --locked -- run --no-hotkey # Neither global key; focused local Option+I/Option+M remain
cargo run --locked -- run --font-family 'SauceCodePro Nerd Font' --font-size 12
cargo run --locked -- run --font-file assets/fonts/SourceCodePro-Regular.ttf --font-size 12
cargo run --locked -- run --sprite-scale 1 # Full-size character; bubble/text unchanged
cargo run --locked -- run --sprite-scale 0.75 --font-size 12
cargo run --locked -- font-license
cargo run --locked -- run --no-bubble
cargo run --locked -- run --demo
cargo run --locked -- run --demo --always-bubble # Persistent Preview for debugging
cargo run --locked -- run --manual --animation waiting --paused --always-bubble --exit-after 5
# After a debug build:
./target/debug/numen run
# In another terminal, while a fresh run is listening:
./target/debug/numen publish --demo
# Alternative: keep a named diagnostic conversation connected for selection:
./target/debug/numen publish --source djinn --instance producer-1 --activity chatid --state working --summary 'Investigating retry backoff' --duration 60
# Inspect/select activities while a publisher is still connected:
./target/debug/numen status
./target/debug/numen bubbles # Brief reveal; graphical service required
./target/debug/numen --socket /path/to/private/events.sock bubbles # Target one service
./target/debug/numen status --offset 4
./target/debug/numen focus --source djinn --activity chatid
./target/debug/numen focus --source djinn --activity chatid --instance producer-1
./target/debug/numen focus --next
# Headless alternative to run (do not bind both to the same path):
./target/debug/numen listen --exit-after 5
```

Each publisher command has its own connection lifetime. After it disconnects, the familiar retains that unavailable pin: use `focus --next` or select another connected activity to follow a new publisher. The `--source djinn` example is diagnostic data, not an automatic Djinn application integration.

### Optional Djinn interactive adapter

The sibling Djinn implements a best-effort local adapter for the **local ordinary interactive UI with its owning internal Worker**. Chat discovery is **on by default in updated Djinn builds**; Numen remains optional. No enablement export is needed:

```sh
djinn # Open a chat from the dashboard
djinn -s REF
# Or:
djinn session chat REF
# Separately, select Djinn preset → Open → the chat target:
~/Projects/numen/target/debug/numen menu
```

- The first existing chat opened in the **Chat tab** is eligible even while idle. Global dashboard/history views do not publish new targets. Viewer presence renews every 5 seconds with a 20-second TTL; leaving Chat removes an idle target. A previously observed busy chat remains tracked until idle, even after leaving Chat.
- Source is `djinn`; instance is a backend-lifetime UUID stable across socket reconnects; activity is the actual UI `SessionID`. Process exit/disconnect leaves exact familiar pins Unavailable. Reload or a new backend creates a new instance: explicitly reattach rather than inheriting an old pin.
- Publishes `idle`, `working`, and `waiting` (permission/question waits require attention; retry does not), plus authoritative owning-response outcomes: completed response → `succeeded` / `response_completed`, final assistant non-abort error or failure/defect → `failed` / `run_failed`, winning explicit user cancellation → `cancelled` / `run_cancelled`. Completion means the response finished, not proof the user's task succeeded; normal stop/length can qualify. Missing finish, tool-calls, unfinished/non-assistant results remain neutral. Idle, error events, tool/model steps, shell completion/nonzero exit and disconnect do not imply outcomes. Abandoned teardown/interruption without user cancellation is neutral; final non-abort assistant errors/defects take precedence over cancellation. Reconciliation polls every 250 ms and can take about 500 ms after an event, not hard real-time.
- Mid-run attachment captures the active owner, not historical settlement. Stable ownership order rejects late A cancellation after B owns the session; joined callers yield one outcome. Viewed terminal bases persist until new work/external busy; unviewed observed terminals remain 5 seconds, then are removed. Reload abandons ownership.
- Live cue submission is at most once, **not exactly-once GUI animation**. Service and transport each buffer at most 32 live events with 2-second expiry; ACK waits are at most 500 ms. The service seals `transport.connection()` at event time: offline, unACKed-registration or prior-generation events become authoritative snapshot-only state. Overflow, expiry or lost ACKs drop cues, never replaying on reconnect. Registration/snapshots can show Completed/Failed/Cancelled without historical cues. Cancellation never celebrates; existing focused failure/attention priority remains unchanged. No public Schema/SDK or Numen Rust renderer changes are needed.
- Summary uses only the existing title, sanitized, trimmed and capped at 160 UTF-8 bytes. The adapter makes no LLM calls and copies no raw prompts or error files. **Titles and status are shared automatically for eligible chats and can be sensitive**; opt out to prevent reporting. `--no-bubble` hides display only, not protocol/state metadata.
- `DJINN_NUMEN_DISABLED=1` explicitly opts out; legacy `DJINN_NUMEN=0` also disables reporting. Missing `DJINN_NUMEN` means true; missing `DJINN_NUMEN_DISABLED` means false. Both accept exactly `true/yes/on/1/y` or `false/no/off/0/n`, case-sensitive with no trimming. Malformed enablement becomes false; malformed disablement becomes true. Effective enablement is the parsed enable flag AND NOT the parsed disable flag, consistently in CLI and backend.
- `DJINN_NUMEN_SOCKET=/absolute/private/events.sock` overrides the endpoint and must match Numen's `--socket`; it requires an existing user-owned private directory/socket. Defaults match Numen: nonempty `XDG_RUNTIME_DIR/numen/events.sock`, otherwise the UID-scoped temporary path below.
- Discovery neither starts Numen nor opens familiars, publishes conversation history, reads raw prompts, changes Wisp/transcripts or makes model calls. Menu Open can bootstrap Numen; the adapter retries connection by default every 1 second. Refresh the target picker after it connects.
- Admission is bounded to 16 current sessions and 32 distinct session IDs per backend/transport lifetime, including removed IDs. It fails closed rather than reusing an ID for another chat. Restart the backend and explicitly reattach to reset lifetime capacity.

Not supported: Rust-native `ask`/`session run`, `--mini`, external attach, remote web clients, the current core v2 API, or arbitrary background/child sessions. No OS process/session scanning is performed. See the optional sibling [Djinn adapter guide](../djinn/docs/numen.md); Djinn is not a dependency of Numen.

An already-running older **Djinn UI needs one restart with the updated Djinn binary** to pick up default-on discovery. Rebuilding alone does not update that running UI; open the chat again and explicitly reattach if the backend instance changed. This is separate from the Numen runtime-settings service upgrade described below; no app restart or home installation is performed by this documentation update. For a source fallback, launch Bun from Djinn's `clients/djinn-ui/packages/opencode` so `bunfig.toml` loads the OpenTUI/Solid preload; no enablement flag is needed.

### Local macOS SDK workaround

The selected `MacOSX27.0.sdk` currently causes a linker failure reporting unknown architecture `arm64e.x1`. This command-scoped workaround has been verified locally:

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo build --locked
```

Use the same `SDKROOT=...` prefix for `cargo test`, `cargo clippy`, or `cargo run` on this machine when needed. This is a local SDK issue, not a portable prerequisite: do not hardcode this path in repository build configuration or change system settings.

### Local debug installation

From this checkout, install or refresh the current debug build:

```sh
cargo install --path . --debug --locked --offline --root "$HOME/.local" --force
```

This installs `~/.local/bin/numen`; keep that directory on `PATH`. Apply the
command-scoped SDK prefix above on machines that need the macOS workaround.
`--offline` assumes dependencies are already downloaded. `--force` refreshes an
existing same-version installation after source changes.

On the validation machine, the older Homebrew Numen command was unlinked (not
uninstalled) because `/opt/homebrew/bin` precedes `~/.local/bin`. No shell files
were edited. `brew link numen` restores the Homebrew command if desired.

In an already-open zsh terminal, run `rehash` once if it cached the old command:

```sh
rehash
command -v numen # Expected: ~/.local/bin/numen
numen menu
```

Installing does not restart a running service. To load new code, explicitly run
`numen quit`, then `numen menu` and reopen the attached familiar. Future menu and
control commands no longer need the checkout's full executable path.

## API / Interfaces

### Read-only doctor

`numen doctor` prints a human report; `numen doctor --json` emits one JSON report. Global `--socket PATH` selects the endpoint in either form. It reports its **own client executable and embedded build**, separately from the **responding service PID, executable and embedded build**. Replacing a file at the same path does not replace a running service.

```sh
numen doctor
numen doctor --json
numen --socket /absolute/private/events.sock doctor --json
```

JSON fields are `client: {executable, build}`, `socket`, `service` (object or null), `service_state`, and `build_comparison`. Build fields are `version`, `fingerprint`, `source_fingerprint`, `target`, and `profile`. Service fields and hotkey status/reason codes are specified in [Protocol](PROTOCOL.md#read-only-diagnostics). `familiar_count` counts logical registry entries, including unavailable pins; `source_count` counts registered protocol sources, not native windows or OS processes. No raw argv, environment or activity summaries are included.

The recorded-build comparison (`build_comparison`) is `match`, `different`, or `unknown`. A match means the recorded covered inputs match, **not guaranteed identical executables**. Different does not mean newer/older. Package version or executable path alone is insufficient. The source FNV-1a-128 fingerprint covers `Cargo.toml`, `Cargo.lock`, `build.rs`, `build_support.rs`, and all files under `src/` and `assets/`. The build fingerprint adds compiler identity, target/profile, optimization/debug settings, Cargo cfg/features/profile environment and encoded Rust flags. Flags/compiler details stay hashed; no Git metadata, timestamp or new dependency is used. This is not hermetic SDK/linker/global-config/wrapper/dependency-content provenance or a security digest. Cargo's normal mtime/cache limitations still apply; use a clean rebuild if cache validity is questioned. See [Architecture](ARCHITECTURE.md#recorded-build-identity).

Doctor makes **one bounded read-only `companion.service` query**, with private-socket checks and existing 500 ms connection/write/ACK bounds. It does not start a service, signal any process, create directories/logs, reveal bubbles, edit settings, register a source/hotkey or change source TTL/liveness. Missing/refused sockets yield `service_state: "not_running"` and exit **0**. Offline, legacy, unsupported diagnostics and different builds are informational (0); unsafe paths, other transport errors, timeout, malformed/contradictory current diagnostics and duplicate JSON keys fail (1). Legacy services without `diagnostics` yield `legacy` and unknown build/hotkeys; future diagnostics versions yield `unsupported_diagnostics` and unknown comparison rather than guessed field meanings.

**Deployment confirmed:** the main agent installed the latest debug binary with `cargo install --path . --debug --locked --offline --root ~/.local --force`. Bare `numen` resolves to `~/.local/bin/numen` and doctor works. No home configuration changed and no user apps were restarted. The updated doctor works against an old service immediately. An old running service cannot retroactively report new fields: **one user-chosen Numen restart** with the updated binary adds diagnostics. No Djinn restart is needed for this change; doctor never triggers the restart itself.

`check-assets` validates the embedded atlas without opening a window. `run` starts with the undecorated, draggable `main` view and **starts the socket listener by default**; IPC can open additional views. `listen` runs the activity service without a GUI or asset decoding and prints legacy `main` presentation changes, not a raw audit. It supports discovery but not graphical lifecycle commands. `publish` also needs no GUI or asset decoding.

`font-license` prints the full embedded SIL Open Font License 1.1 text for the bundled fallback. `listen`, `publish`, `status`, `focus`, `bubbles`, `check-assets`, and `font-license` do not initialize fonts.

### Menu, startup and shutdown

`menu [--no-start] [--fzf PATH]` requires stdin and stderr TTYs and installed fzf (already installed on the validation machine). Use Wisp **run/summon**, not exec. It lists clearly labeled running/waiting familiars and presets, with distinct `familiar:` / `preset:` identities. Tab multi-selects up to 16; Enter advances to commands. Command Escape/Back retains the familiar selection; target Escape/Back discards the **entire** pending batch. Ctrl-R refreshes the current stage. One accepted action exits the menu so the Wisp Kitty floater can close.

Commands are the capability intersection of **all** selected rows: presets support only Open; active familiars support Info, Go to Mouse, Settings, Attach and Close as advertised. A mixed selection may have no common command: Back is offered with an explanation, never silent partial execution. Open/Attach fetch fresh target pages separately for each selection, excluding other owners and earlier pending choices while allowing one's own current attachment. Empty lists offer Refresh/Back; only connected publishers appear, including supported Djinn chats discovered by default. If empty, open a Chat tab in an updated Djinn UI, check reporting is not opted out and the sockets match, allow reconnect, then Refresh. Backend mutation revalidates the whole batch; retryable errors appear in the menu header. With only one Djinn preset, opening more copies requires repeated invocations; a batch applies once to each actual selection.

Merely entering the menu or discovering sources never spawns a service or writes files. Choosing Open while offline starts the current executable as detached `run --empty`, unless `--no-start` requests a visible error instead. It creates no unattached `main`; source updates alone keep the registry empty. Startup uses a private 0600 `service.log` beside the socket and a five-second control-readiness wait, not GPU readiness. Concurrent starts reuse the winning graphical listener. Existing headless, outdated or unsafe services are never replaced or killed: stop an old foreground service manually once or choose another `--socket`. **Direct CLI `open` still requires an already-running graphical service.**

`service-info` queries `{api_version, pid, service_instance, graphical, lifecycle, navigation}` plus additive `settings`, `familiar_count`, `source_count` and `diagnostics` in current services. The menu pins `service_instance` and seals each RPC with it: a restart rejects stale selections before dispatch, rather than acting on reused familiar IDs. Close/reopen the menu after a service restart. `quit` sends `companion.quit`: `{"quitting":true}` means scheduled, not completed. Current graphical services exit their event loop, release all views/hotkey and clean up their owned socket; headless/legacy handlers without a shutdown callback reject it. No source applications are terminated and no reported PID is signaled. Closing the last view retains the service; `quit` stops it globally. Cancelling Open after bootstrap may leave an empty resident service: use `quit` when desired.

```sh
~/Projects/numen/target/debug/numen menu
~/Projects/numen/target/debug/numen menu --no-start
~/Projects/numen/target/debug/numen service-info
~/Projects/numen/target/debug/numen quit
# Prints TOML only; no write:
~/Projects/numen/target/debug/numen wisp-action
# Optional user-run setup AFTER delivery; see WISP.md before installing:
~/Projects/numen/target/debug/numen wisp-action --install
```

The generated **Summon** entry is reached through `wisp palette`; it has **no key** and adds no WM-prefix/static hint binding. It stores absolute executable/socket argv, so Numen need not be on PATH. Installation is opt-in and preserves existing catalog text with two backups on changes; [WISP.md](WISP.md) explains conflict refusal, concurrent-edit preservation and recovery.

Global `--socket PATH` works before or after the subcommand. Default: nonempty `$XDG_RUNTIME_DIR/numen/events.sock`, otherwise the standard temporary directory plus `numen-UID/events.sock`. Paths are limited to 100 bytes. Use a user-owned private parent (0700); the server creates only its leaf directory if its ancestor exists and never chmods an existing directory. The socket is user-owned, mode 0600. See [Protocol](PROTOCOL.md) for safety and lifecycle details.

| `run` option | Behavior |
| --- | --- |
| `--manual` | Disables the socket; use with `--animation` for a fixed diagnostic selection. |
| `--empty` | Starts a graphical service with zero familiars and no `main`; conflicts with `--manual` and `--demo`. |
| `--animation NAME` | Initial expression, replaceable by live input; default `idle`. Names, in keyboard order: `idle`, `working`, `waiting`, `succeeded`, `failed`, `review`, `waving`, `running-left`, `running-right`. |
| `--scale NUMBER` | Finite 0.5–4, default 1.5; scales the whole scene, including bubble and font. Default canvas is 96 × 104 hidden, 384 × 248 revealed. |
| `--move-speed NUMBER` | Finite 40–2000, default 360 desktop logical points/second; independent of font, sprite and scene scales. |
| `--sprite-scale NUMBER` | Finite 0.25–2, default 0.5; scales only the native 192 × 208 character, independently of bubble/text. Use 1 for full-size artwork. Canvas height follows the scaled sprite. |
| `--no-bubble` | Hard privacy disable: ignores every reveal, stays sprite-only and skips fonts/global Option+I; global Option+M remains eligible. Does not redact wire/state metadata. |
| `--always-bubble` | Opt-in persistent bubble for debugging/previews; conflicts with `--no-bubble`. |
| `--bubble-duration SECONDS` | Finite 0.25–30, default 5; lifetime of notifications and explicit reveals. |
| `--no-hotkey` | Disables both global Option+I and Option+M registrations, not focused local shortcuts or CLI controls. |
| `--font-family NAME` | Explicit installed family, exact Normal weight/style/stretch (Regular); missing/invalid selection errors rather than falling back. Conflicts with `--font-file`. |
| `--font-file PATH` | Explicit TTF/OTF override; uses face 0 (also index 0 for a TTC collection). Missing/invalid files error rather than falling back. |
| `--font-size NUMBER` | Finite 8–32, default 12 logical canvas units; scales with `--scale` and display density, independently of `--sprite-scale`. |
| `--demo` | Disables the socket and cycles all nine expressions, three seconds per selection. |
| `--paused` | Starts with animation and motion paused; navigation requests wait for resume. Conflicts with `--demo`. Bubble deadlines still run. |
| `--opaque` | Black diagnostic background instead of transparency. |
| `--normal-window-level` | Disables the default always-on-top request. |
| `--focus` | Opts into requesting keyboard focus at startup. |
| `--exit-after SECONDS` | Finite 0.1–86400; closes automatically for bounded smoke tests. |

With keyboard focus, **Tab** queues next-conversation for that familiar and resumes live control. **1–9** enter manual expression override; **L** resumes its latest live base without switching or replaying cues. **Space** toggles only that view's pause flag; expression changes preserve pause but may change the first frame. **Q/Esc/native close** closes only that live view, not all of Numen. Closing the last view leaves IPC and global hotkeys alive with zero windows; `open` can create another later. `--manual`/`--demo` remain single-view, socket-free sessions: Q/Esc/native close exits the process, and Tab/L cannot enable IPC. On macOS a normal left-click release or right-click opens the native menu; held left movement starts native dragging after the click slop. Other platforms retain immediate left-drag/focus and right-click Preview cycling. **Ctrl-C** quits the service, including from a focused view or the launching terminal.

**1–9** (and right-click on non-macOS) also reveal the Preview bubble; **Tab/L** reveal when resuming live control. A registered global handler owns Option+I even when the pet is focused, preventing duplicate local delivery. If global registration is disabled/unavailable, focused local **Option+I** is the fallback. Demo cycling does not automatically reopen the bubble; use `--always-bubble` for persistent previews.

The macOS global shortcut uses Carbon registration via `global-hotkey` 0.9, not an EventTap or all-keystroke capture. Registration failure warns but does not stop the app. Another Numen process may own the chord: use `bubbles --socket PATH` to target the desired service. Option+I is reserved while registered and may override a macOS dead-accent shortcut; opt out with `--no-hotkey`. This non-media Carbon key should not require Accessibility permission, but actual background-focus chord acceptance remains pending; no synthetic OS keyboard input was used.

Option+I and Option+M have independent registration guards and press latches: repeats are ignored until that key's matching release. A collision warns and enables that key's local/CLI fallback without disabling the other. Focused local **exact Alt+M** moves only the focused view when global M is unregistered; successful global M suppresses duplicate local delivery, independently of global I. M/navigation requires successful pointer initialization in a live macOS service; it is unavailable in manual/demo, on other platforms or after pointer initialization failure. Callbacks only queue bounded intent; the main GUI loop applies the latest registry and skips closing/failed views. Pause is preserved, so queued movement waits for resume.

Both guards share one main-thread Carbon manager and a process-global capture-free dispatcher with two weak targets. Cleanup releases Rust callbacks and warns on unregister failure. Native unregister is best-effort: the public crate can lose its native handle on an OS `UnregisterEventHotKey` error (a pre-existing rare limitation); process exit supplies OS cleanup, not a guarantee of leak-free native failure handling.

`bubbles` sends v1 `companion.reveal` without producer registration and prints status from the ACK. Acceptance does not guarantee a displayed frame (notably with `--no-bubble`). It changes neither selection nor producer state/liveness and never requests focus. Headless `listen` rejects it with `no graphical familiar`.

`status [--offset N] [--familiar ID]` prints mode, exact focus, availability and four activities per page, including summaries. `focus [--familiar ID] --source SOURCE --activity ACTIVITY [--instance INSTANCE]` requires exactly one live match not owned by another familiar. `focus [--familiar ID] --next` cycles eligible live records, including terminals, in lexical `(source, instance, activity)` order with wraparound. These controls work with `run` or `listen` without registration. Omitted `--familiar` always means `main`; explicit unknown IDs error. After `main` closes, legacy status has null focus without recreating it; focus requires an existing familiar. CLI selection does not cancel GUI expression override; use L or Tab.

### Native familiar menu (macOS)

A normal **left-click release** opens an AppKit `NSMenu`; **right-click** opens the same menu instead of cycling Preview. Number keys **1–9** are unchanged. This GUI menu is separate from `numen menu`: the existing terminal/fzf/Wisp flow remains available. Source updates never open this menu and grant no source-execution authority.

| Action | Scope / availability |
| --- | --- |
| Show status | Timed reveal of the clicked view; disabled by `--no-bubble`. |
| Go to mouse | Clicked view only, with live navigation and an available pointer reader; preserves pause. |
| Pause / Resume | Only the clicked view's animation/motion pause state. |
| Settings | Three existing fields and preset sets below; one field per selection, live service only. |
| Close familiar | Only the clicked familiar, using the existing bounded close/retry path; last Close keeps the service. |
| Quit Numen (all familiars) | Normal root shutdown of the entire Numen service, never Djinn or another source app. |

Settings checkmarks use canonical `f64` registry values, not renderer `f32` rounding. Nonpreset values appear as checked, disabled **Custom (number)** rows. Settings selection queues `FamiliarControl::settings` nonblockingly through the shared 32-entry channel → canonical Store → complete snapshot; queue acceptance is not a committed edit and there is no optimistic projection. In manual/demo sessions Settings is unavailable (no IPC), as is navigation; Pause/Resume, permitted status reveal, Close and Quit remain usable.

**Example:** release a left-click on a familiar → Settings → Text size → Large (16). Only that familiar changes after its authoritative snapshot arrives. Choose Close familiar to leave Numen resident, or Quit Numen (all familiars) to stop the service.

**Gesture and tracking edge cases:** five desktop logical points of slop distinguish click from held drag. Native drag retains and revalidates the original `NSEvent` `LeftMouseDown` and native window; a mouse-up coordinate change cannot begin drag without the physical button still down. CursorLeave cancels click eligibility only, retaining a held drag (including at the left edge). Held/native-drag state polls every 20 ms to recover lost mouse-up. Pending gestures/menu tracking hold geometry and suspend motion clocks; opening/dismissing alone preserves cues, Preview/manual override, pause and navigation. An actual drag commits the new hidden anchor and cancels navigation.

`NSMenu` tracking is synchronous: Rust GUI ticks are suspended, then clocks are rebased. A 25 ms native `NSTimer` cancels tracking on clicked-target removal, external service quit or `--exit-after`; ordinary source changes do **not** dismiss it. Latest registry snapshots and both familiar ID and WindowId are revalidated after tracking. Stale/failed/closing per-view actions are ignored; service-wide Quit does not depend on the clicked target surviving. Native menu support is macOS-only; other platforms retain the previous mouse controls.

### Runtime presentation settings

`settings --familiar ID1 ID2 [--sprite-scale N] [--font-size N] [--bubble-duration SECONDS]` atomically edits **explicit 1–16 unique existing IDs**. At least one setting flag is required; any nonempty subset is allowed. No implicit all or settings-get command: `familiars` rows include complete `settings` values (`sprite_scale`, `font_size`, `bubble_duration`). Finite inclusive bounds are **0.25–2**, **8–32** logical canvas units, and **0.25–30 seconds**, respectively.

```sh
./target/debug/numen settings --familiar main build-pet --sprite-scale .5 --font-size 16 --bubble-duration 5
./target/debug/numen settings --familiar build-pet --font-size 14
./target/debug/numen familiars
```

Active familiars also offer **Settings** when every selected row advertises `settings`. This capability requires the new graphical settings constructor, independently of navigation; headless/legacy services reject edits. Menu Settings chooses **one field/preset per invocation** for the whole selection:

| Field | Presets |
| --- | --- |
| Sprite size | Small 0.25, Default 0.5, Large 1, XL 2 |
| Text size | Small 10, Default 12, Large 16, XL 20 |
| Bubble duration | Short 2, Default 5, Long 10, XL 30 seconds |

Field Back/Esc returns to commands; preset Back/Esc returns to fields. Ctrl-R discards the pending choice and refreshes. No write occurs until a concrete value is selected; IDs/capabilities are rechecked and the request is sealed to the original service generation, without bootstrap. Common values show preset names and numbers, differing values show **Mixed**, common custom values show **Unknown (number)**, and absent values show **Unknown**, never guessed defaults. One accepted edit exits.

Actual startup `run` options seed Main and immutable defaults for future Opens, including `run --empty`. Editing Main does not change future defaults. Settings preserve attachment, cues, source TTL, Preview/manual override, pause, player and navigation. Font/sprite edits redraw selected views and reconcile caches/layout. Sprite resizing preserves physical foot-center where possible, adjusts saved hidden restoration and clamps to monitor bounds without focus. Duration changes affect **future accepted reveals/notifications only**: they do not reveal hidden bubbles, retime current deadlines, reset quiet/history state or override `--no-bubble` / `--always-bubble`.

**One explicit restart after deployment:** rebuilding does not upgrade an already-running older Numen service. When ready, run these yourself using the rebuilt binary (full path if needed, same `--socket` on both):

```sh
~/Projects/numen/target/debug/numen quit
~/Projects/numen/target/debug/numen menu
```

Then choose Djinn preset → Open to reattach the existing chat. **No Djinn restart is needed** for this Numen update; the adapter reconnects. A legacy service without quit support must be stopped manually. Numen never auto-quits/replaces it. Once updated, runtime edits need **no restart**. There is no persistence, character/variant picker, placement control or autostart setting; existing native dragging remains available, not a new positioning feature.

The same one-time explicit Numen restart enables Option+M and the native click menu in an older running service; rebuilding alone leaves it unchanged. Use the rebuilt full path above: an older Homebrew `numen` may not be current. Restart only when explicitly chosen by the user, never automatically. This update adds no persisted settings and requires no Djinn restart or Wisp/home changes. The existing live user process was left untouched.

### Familiar discovery and lifecycle

One service supports **16 familiars**. Bindings are exclusive exact `(source, instance, activity)` tuples, including disconnected waiting bindings. Preset IDs and active familiar IDs are distinct.

| Command | Contract |
| --- | --- |
| `presets` | Offline JSON: `{"presets":[{"id":"djinn","name":"Djinn","capabilities":["open"]}]}`. |
| `familiars --offset N` | Four records/page: `id`, `preset`, `attachment`, `available`, optional `summary`, `capabilities`, complete `settings`. GUI capabilities: `info`, `attach`, `close`, plus `settings` with live settings support and `go-to-mouse` with navigation; headless/legacy handlers: `[]`. |
| `targets --offset N [--familiar ID]` | Four connected protocol-advertised activities/page with `claimed_by` and `eligible`; an owner may reselect its own target. The menu filters eligible entries; supported Djinn chats publish here by default unless opted out, without OS scanning. |
| `open --source NAME --activity A [--instance I] [--id ID] [--preset djinn]` | Opens an attached view in an already-running graphical service; no auto-bootstrap. |
| `attach --familiar ID --source NAME --activity A [--instance I]` | Retargets an existing familiar, never executes/stops the source. |
| `close --familiar ID1 ID2` | Atomically removes selected familiars, not their applications. |
| `go-to-mouse --familiar ID1 ID2` | Explicit 1–16 unique existing IDs; no implicit all. ACK data `{"moving":["ID1","ID2"]}` means accepted, not arrived. Requires graphical navigation support (currently macOS). |

Open/Attach alternatively accept `--batch FILE` or `--batch -` (stdin): a **root JSON array**, one flattened target per selection. Use item field `id`, not `familiar`:

```json
[{"id":"build-pet","preset":"djinn","source":"build","instance":"worker-2","activity":"task"}]
```

Attach requires `id` and does not use `preset`; Open defaults `preset` to `djinn` and can generate `id`. Duplicate owners/targets/IDs, unknown presets, unavailable targets or ambiguous omitted instances reject the entire batch without changes. Attach cannot swap targets currently owned by another familiar, even when both owners are in the batch. Revalidation happens at mutation. ACK data contains `opened`, `attached` or `closed` ID arrays: accepted does not mean rendered. Asynchronous native/GPU creation can fail and remove only the bad view.

IDs are stable only for the service lifetime. Generated `familiar-N` IDs increase monotonically without growing generated-ID history; closed IDs are not reused. Explicit-ID history is capped at 256 plus reserved `main` (257 total); omit `id` to continue generating IDs. No persistent profiles exist; restart resets IDs.

```sh
./target/debug/numen presets
./target/debug/numen familiars --offset 0
./target/debug/numen targets --offset 0 --familiar main
# Choose an eligible connected target not already held by main:
./target/debug/numen open --source build --instance worker-2 --activity task --id build-pet
./target/debug/numen status --familiar build-pet
./target/debug/numen bubbles --familiar build-pet
./target/debug/numen go-to-mouse --familiar main build-pet
./target/debug/numen attach --batch attachments.json
./target/debug/numen close --familiar build-pet
```

`publish` requires either `--state` (the six semantic states in [Protocol](PROTOCOL.md)) or `--demo`. Defaults: `--source diagnostic`, `--activity task`, a generated PID/time-based instance, and `--duration 3` seconds per state. It holds activity while connected, sends heartbeats at approximately one-second intervals, then removes the activity and disconnects. `--attention` is valid only with `--state waiting`; `--reason` supplies optional detail. Demo publishes seven steps: working, waiting requiring attention, succeeded, working, failed, cancelled, idle. Connection, validation, or ACK errors exit nonzero. Socket I/O is bounded, but synchronous stderr logging can still block; this command is not an agent callback or best-effort SDK.

## Data Flow

When visible, the thought bubble shows a clipped `source / activity / instance` identity line, a producer-supplied summary wrapped by measured width to at most three lines with `…` (U+2026) truncation, and the current base status: **Idle**, **Working**, **Needs your input**, **Waiting on dependency**, **Completed**, **Failed**, **Cancelled**, or **Unavailable**. Font metrics and available space determine the line count; larger fonts can reduce it without resizing the sprite. No selected conversation shows **Choose a conversation** / **Not following**; a missing summary falls back to the activity ID. Manual/demo expressions visibly say **Preview**, not agent activity. Text follows the current base, not a transient animation cue: a completed conversation can correctly say Completed while the pet rests.

`BubbleVisibility` observes selected identity, displayed status and summary. First selection or a new focus reveals for the full duration; an initially empty No conversation presentation does not. Status/summary changes can notify, including Unavailable, but heartbeats, reason-only changes, cue expiry and background updates do not. Automatic updates while visible repaint without extending its deadline. After expiry, a 750 ms quiet period suppresses automatic notifications; suppressed changes are cached, not queued. Explicit reveal or a new focus bypasses quiet time and refreshes the full deadline. The hide timer runs even while paused or occluded.

The default `Typeface` prefers installed **SauceCodePro Nerd Font Regular**, discovered by `fontdb` in standard system/user font directories. This preference matches the owner's configured Kitty family; Numen does **not** parse Kitty configuration or read its font size at runtime, and has no hardcoded machine-specific font path. If the default preferred face cannot be loaded, it uses embedded **Source Code Pro Regular**, an OFL-licensed base font without Nerd Font icons. Explicit overrides never silently substitute the bundled face. See [font provenance and limits](assets/fonts/README.md).

Supply metadata with `publish --summary 'Investigating retry backoff'`. Summaries must be nonblank, at most 160 UTF-8 bytes, and contain no control characters or bidi embedding/isolate controls. The CLI trims surrounding whitespace and repeats the summary on every state, including `publish --demo`. Numen makes no LLM calls, reads no transcripts, and automatically copies no raw prompts. The optional Djinn adapter reuses the existing chat title as described above.

**Privacy:** even a short explicit summary may be sensitive and visible to nearby people or screen sharing. `run --no-bubble` hides only the on-screen bubble; summaries remain in protocol/state and diagnostic output. Producers must choose safe metadata.

Publishers → Unix socket → canonical source/activity store → per-familiar binding, cue and summary cache → complete latest-only familiar snapshots → independent expression controllers, players and pixels surfaces. External waiting maps to `review`; attention waiting maps to `waiting`. Terminal records retain identity but rest after fresh success/failure one-shots unless that same activity has resumed work. Background work is never restored into another familiar. The outer App routes native window IDs to views; source/activity IDs appear in logs and window titles.

Graphical startup validates the embedded `assets/djinn/` PNG/manifest through the generic `SpriteAtlas` and caches RGBA frames. The temporary global default is character ID `djinn`, display name **Djinn**, so all tools currently render Djinn as a fallback. Source names are producer identities, not character identities. The future plan is a different global default plus an explicit application-source `djinn` → character `djinn` mapping; that mapping is not implemented. No external extraction is needed. The local, gitignored `djinn-artifacts.zip` retains its original generation metadata unchanged as provenance. See [Architecture](ARCHITECTURE.md), [Protocol](PROTOCOL.md), and [Modes](MODES.md).

## Edge Cases

- Go to mouse uses hard deadline gates to sample the live pointer and step each active pursuer at most roughly 60 Hz; unrelated redraws/source events cannot accelerate it. Contact uses cached physical sprite alpha from the renderer's exact pixel-center sampler, excluding discarded source pixels, bubbles, transparent holes and padding. The planned pose is held for actual native-position feedback before arrival. It plays one `succeeded` jump, then resumes the latest attached emotes, with no ongoing follow. Reissuing restarts intent. Multiple selected views may overlap at the pointer; there is no smart placement.
- Navigation owns the visual pose while source changes continue updating status/summary bubbles. Expired source cues are not replayed afterward. Local arrival never publishes agent success or changes bindings/source liveness. Actual native drag, digits, non-macOS right-click cycle, L, Tab and close cancel without success; macOS menu opening/dismissal and Option+I/reveal-all do not cancel. Space pauses both movement and animation, not bubble deadlines.
- Occlusion alone does not suspend pursuit or its success animation: overlapping familiars can still arrive and complete. Explicit pause freezes both; invalid/zero-sized native geometry waits for restoration. Fractional motion accumulates rather than nudging a device pixel on every event. Refused/clamped movement or stable-target non-convergence cancels in bounded time with a warning, never fake success.
- `succeeded`, `failed`, and `waving` play once, then return to `idle`; other clips loop. Manual running expressions animate in place; Go to mouse also moves the native window.
- The native window starts with a 96 × 104 logical sprite-only canvas at default half-size, expands to 384 × 248 while revealed (sprite at `(144, 144)`), then shrinks back: no full hidden bubble input area remains. `--sprite-scale 1` gives 192 × 208 hidden or 384 × 352 revealed. Native atlas cells remain 192 × 208. Expansion preserves the sprite anchor when possible and clamps to current monitor bounds near edges. Collapse restores the saved hidden position; an explicit left-drag gesture clears that saved anchor and commits the current location instead. No immediate position query is treated as confirmation that an asynchronous window manager finished moving. Positioning is best-effort on unsupported platforms.
- The framebuffer matches physical window pixels for 1:1 GPU presentation. Actual physical scene scale is `min(physical_width / logical_width, physical_height / logical_height)`; only sprite artwork is nearest-resampled at that scale times `--sprite-scale`, using pixel centers. The whole canvas/text is not nearest-upscaled. At physical scene scale 2, a half-size sprite has its native 192 × 208 physical pixels. Aspect-ratio differences can leave centered padding. Resize updates both buffer and surface; display-scale changes request redraw and reconcile physical dimensions.
- `fontdue` rasterizes glyphs at `font_size × physical scene scale`, using real advances, kerning and line metrics, with grayscale coverage alpha-blended onto ivory. Unicode and Nerd icons render when the selected face supports them; missing glyphs use that face's U+FFFD, then `?`, then `.notdef`. There is no full script shaping, color emoji, or fallback-font chain. `fontdb`/`fontdue` replace the removed `font8x8` bitmap dependency. When no conversation is selected, the identity says **No conversation selected**, not the app/character names.
- Selected font data is limited to 32 MiB with a bounded read; system metadata discovery by `fontdb` is outside that cap. The glyph cache holds at most 512 entries at one exact physical pixel size and clears on size/DPI changes. Fonts load once at bubble-capable GUI initialization even while initially hidden. `--no-bubble` skips font discovery/loading entirely, even for a nonexistent family or invalid file; CLI syntax, conflicts and numeric validation still apply.
- Removal, snapshot omission, disconnect and expiry retain the selected identity and last summary with Unavailable status. A replacement record with absent/null summary clears that summary; changing focus replaces the cache, never borrowing another chat's text. Background updates cannot change the bubble. Metadata-only updates repaint even while paused or on the same animation, without restarting it or requesting focus.
- Only initial `main` can request startup focus with explicit `--focus`; subsequently opened views never do. Animation/reveal never requests focus. macOS uses accessory activation policy, with no Dock icon.
- Views share an `Arc<SpriteAtlas>` and parsed font (`Typeface::fork`), with independent glyph caches, Window/Pixels, Player, pause, visibility and geometry. Native/GPU failures close only the failed live view and retry registry removal if needed; shared font initialization remains fatal. Global exit deadlines run even with zero views.
- Always-on-top, transparency, and native drag depend on desktop support. GPU logs do not prove correct visual composition or focus behavior.
- The latest-only headless output slot and detached stdout writer keep an unread pipe from blocking the service or `--exit-after`. Slow readers may miss intermediate presentations; the latest pending value is retained. Process shutdown terminates a blocked writer and may truncate the last diagnostic line.
- Active socket collisions refuse startup. Only safely identified, user-owned stale sockets whose connection is refused are replaced. Normal drop removes only the owned inode; abrupt termination can leave a stale socket for recovery at next startup.
- Only bootstrap `main` auto-selects the first unclaimed live activity once (lexical among initial snapshot records). Exact pins survive removal, disconnect and expiry and remain exclusively claimed while waiting, without fallback. A new instance needs explicit selection/next. There is no OS-focus inference.
- Completion is not idle or disconnect. Only new success/failure transitions of the focused conversation can cue; repeated terminal states, snapshots and selection of historical terminal records do not replay outcomes. Focused attention clears/drops cues rather than queuing them. Background attention, failure and success never take over.
- Movement commits the hidden sprite anchor so bubble expiry does not snap back to the pre-command position. Pointer reading is read-only: fresh macOS `CGEvent` snapshots from `CombinedSessionState`, no cursor warp, event posting, event tap, permission request or OS process scan. Other platforms currently advertise no navigation capability.
- Menu labels/summaries are sanitized and never shell-interpolated. fzf default-options/options-file/default-command environment variables are removed; unoffered or duplicate output identities are rejected, with a 64 KiB output cap.
- Next: broader adapter coverage and an unrelated runner. Other [mode choices](MODES.md), app-signaled following, different per-view characters/additional options, persistent preferences, general installation/autostart, packaging, gaze and roaming remain future work.

## Validation

### Current user report and doctor validation status

**USER-REPORTED current desktop familiar workflow working:** “I verified everything working fine.” This includes the relevant click/menu/settings/hotkey workflow; it is hands-on user feedback, not a new automated run or a claim that every checklist scenario below was exercised. Mixed-DPI/monitor crossings, real OS collision/failure cases, Wisp palette → Kitty/external handoff and other untested cases remain pending. Earlier statements below about manual acceptance describe the scope of those historical test runs, not a rejection of this current user report.

**Confirmed final doctor validation: 307 passed — 270 binary unit + 7 build-fingerprint integration + 30 CLI tests; one existing visual-preview test ignored.** Formatting, strict all-target Clippy, offline locked tests/build and diff checks passed with the command-scoped macOS 26.5 SDK workaround. Contradictory known hotkey state/reason pairs and duplicate raw ACK keys were rejected in passing tests. Actual Cargo panic/debug-assertions override regression checks produced a different recorded build fingerprint with the same source fingerprint, then restored the baseline.

The installed debug binary passed the live private-socket harness:

| Case | Confirmed result |
| --- | --- |
| Offline endpoint | No directory created; no service started. |
| Empty graphical service with `--no-hotkey` | Both keys `disabled` / `no_hotkey`; settings/navigation capability reporting independent of hotkey registration. |
| Native service with `--no-bubble` | Actual Option+M registration: `registered` / null reason; Option+I `disabled` / `no_bubble`. |
| Headless service | Both keys `unavailable` / `headless`; logical `familiar_count: 1`, not a native window count. |
| Same client/service build | Recorded build comparison `match`, not executable identity or age proof. |
| Fixture producer connect/disconnect | `source_count` changed 0 → 1 → 0; doctor queries did not register or mutate sources, and zero native familiars were auto-created. |

All test-owned services exited 0 and cleaned up their sockets. At validation time, `numen doctor` against the default user socket returned `not_running` and started nothing; this is an observation, not a permanent machine-state guarantee. Installation and bare-command resolution are confirmed above. No hotkey presses or synthetic OS input were used; these checks add no actual native-input verification claim and do not expand the USER-REPORTED workflow acceptance.

These are confirmed main-agent results, not commands rerun by this documentation edit. This edit ran no commands/tests, install, app controls, restart or commits. The **278 menu, 255 hotkey and 246 settings totals below are historical**, separate from the current 307-test result.

### Native click-menu validation

Confirmed after the latest poll/click-race fix with the command-scoped macOS 26.5 SDK workaround: **278 tests passed (257 unit + 21 CLI), with one existing visual-preview test ignored**. `cargo fmt --check`, `cargo test --offline --locked`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo build --offline --locked` and `git diff --check` all passed. The updated Numen binary was built. These are reported main-agent results, not commands rerun by this documentation edit.

An independent temporary AppKit/winit window using the **production `context_menu.rs` popup adapter** showed and automatically cancelled the native menu three times on the main thread, returning after **380 / 378 / 353 ms**. Cancellation used an `Arc<AtomicBool>` and the native 25 ms `NSTimer` in event-tracking mode; the harness exited 0 without errors. This validates native popup construction/placement API use, timer-driven cancellation and repeated tracking cleanup **only**, not physical click/drag or item selection. No synthetic OS keys/mouse, hotkeys or existing-user-app controls were used.

Actual hands-on menu Close versus Quit, Esc dismissal, item selection, focus, mixed-DPI and physical dragging remain manual. Historical source-handoff/settings/Option+M evidence below is separate, not proof of these interactions. No app restart or home configuration change was performed; this documentation edit ran no tests or app controls and left the existing user process untouched.

- [x] Input-free production-adapter native popup build/placement and timer autocancellation, three successive tracking/cleanup runs.
- [ ] Physical click/release/drag, item selection, Close versus Quit, Esc, focus and mixed-DPI acceptance.

### Final Option+M validation

The full Rust suite passed: **255 tests (234 unit + 21 CLI)**, with one existing visual-preview test ignored. Formatting, strict all-target Clippy and build passed. These are reported validation results, not checks rerun by this documentation edit.

A bounded private empty-GUI harness with **zero views** verified actual native Carbon registration and normal-exit cleanup:

| Case | Observed registration |
| --- | --- |
| `--no-bubble` | M registered; I not registered |
| Normal | Both I and M registered |
| `--no-bubble --no-hotkey` | Neither registered |
| Following fresh process | M registered again |

All four processes exited with code 0 and removed their own sockets. No synthetic OS keys, actual key presses, input taps or permission requests were used; no existing user service was controlled or restarted. Collision behavior was tested deterministically, not with real OS collisions; both native registrations succeeded in the normal case. This proves registration and normal-exit cleanup, **not background key delivery or motion via the hotkey**. Actual Option+M OS-chord/background-focus acceptance remains manual. Earlier native CLI-driven motion evidence below is historical and separate. The rare native-unregister failure limitation remains best-effort as documented above.

Runtime-settings validation: **246 automated tests passed (225 unit + 21 CLI)**, with one existing visual-preview test ignored. Formatting, strict all-target Clippy, debug build and diff checks passed with the command-scoped SDK workaround above.

Private-socket native settings smoke tests passed on Apple M2 Pro/Metal, both animated and `--paused`. Read-only CoreGraphics bounds confirmed independent resize of an existing window (216 × 234 → 288 × 312 points), with the unselected window unchanged. Shown/hidden scale edits and a rapid scale burst retained native window identity and restored the original hidden foot-center. A duration edit left the active two-second reveal intact; the next reveal used 0.25 seconds. The real fzf Settings flow edited text size, an atomic multi-view patch succeeded, an invalid mixed-ID batch changed nothing, unavailable views remained editable, and a later Open inherited original startup defaults. Three native views initialized; test-owned services exited successfully and removed their sockets. No running user service, home configuration, global input or pointer position was changed. Hands-on mixed-DPI crossings, visual typography/composition and focus preservation remain manual checks.

Prior Numen validation: **206 unit tests and 18 CLI integration tests passed (224 automated tests)**, plus formatting, strict clippy, debug build and diff checks. These checks were not rerun for this documentation/menu-text update; Numen behavior is unchanged. Real fzf PTY runs verified empty native startup from Open, source-target selection, attached familiar creation without Main, detached service survival, multi-familiar Info, cancellation/Back, and graceful quit with socket cleanup. The scoped Open/bootstrap path passed again after service-generation guarding; tests also reject stale commands against reused IDs after restart. Temporary action catalogs were parsed by installed Wisp; no home catalog was edited. Actual Wisp palette → Kitty handoff remains manual. Earlier native lifecycle/navigation and compositor regression evidence is preserved; font tests use the bundled fixture without OS fonts.

Separately reported Djinn adapter validation covers unit/integration checks, package typechecks and headless real Rust wire cues/reconnect behavior; final focused-test counts are not fixed here. Interop is opt-in with `NUMEN_TEST_BINARY=/absolute/path/to/numen` (prefer an explicit path; `1` is a developer-local test convenience); see the [Djinn guide](../djinn/docs/numen.md#checks) for package-local Bun commands. These checks were not rerun for this documentation update.

Real interactive idle-chat handoff smoke passed on 2026-10-28: an isolated source Djinn TUI advertised its actual selected chat, and the real fzf Preset Open/target flow attached a native familiar. Numen initialized the Apple M2 Pro/Metal view and reported the chat title/idle state. TUI exit retained the unavailable exact pin and summary; familiar Close retained the zero-window service; graceful quit exited and removed the socket. The fixture database contained zero conversation messages. Test processes were stopped, with no installed-binary or home/Wisp configuration changes. This used `--no-hotkey --normal-window-level` and targeted PTY inputs only; live model-outcome animations, visual/focus/hotkey acceptance and the actual Wisp palette → Kitty launcher remain unverified.

Real fzf PTY validation selected Preset Open, auto-started an empty native GUI, registered a diagnostic source, selected its target and opened exactly one familiar with **no Main**. The menu exited while the detached service remained alive. A second real fzf menu multi-selected two familiars for Info without changing source JSON. An existing-empty-GUI test exercised target Escape, command Escape and familiar Escape with no mutation and retained selection. Graceful quit ACKed and removed the socket; a separate native zero-window quit passed. The installer touched only temporary test catalogs; actual installed `wisp actions list` parsed the generated Numen row. No home Wisp configuration was edited, no real Kitty launcher/WM-prefix handoff was exercised, and no global OS keyboard/pointer input was injected: only scripted PTY terminal inputs. Actual `wisp palette` → Kitty handoff remains manual.

Actual dragging between monitors and all hands-on focus/compositor behavior remain unverified. At default scene scale 1.5, the requested logical window is 144 × 156 hidden and 576 × 372 revealed; physical dimensions additionally depend on display density and window-system rounding.

Tests cover CLI validation, atlas integrity, animation timing, protocol/store lifecycle, socket safety, display control, publisher roundtrips, EOF draining, and unread-stdout deadline behavior. Development checks include `cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings`, and bounded GPU smoke runs such as `cargo run --locked -- run --demo --exit-after 5` (apply the SDK prefix above when needed). GPU logs establish rendering activity, not desktop acceptance. Pending:

- [x] Native multi-view control/render smoke: three diagnostic sources/views, targeted/global reveal, independent lifecycle, last-close zero-window service and later Open.
- [x] Two-window native navigation smoke: live pointer reading, running art, confirmed arrival, success once, latest-source restoration and source-state preservation.
- [x] Real Djinn idle-chat → fzf Open → native Metal familiar smoke, exact unavailable pin on source exit, retained service after Close and graceful socket cleanup; no prompts/model turns.
- [x] Private-fixture live settings smoke: independent native resizing, hidden foot-center restoration, future-only duration edits, real fzf text-size edit, atomic batch validation and defaults inheritance, including paused rendering.
- [ ] Hands-on moving-pointer pursuit, monitor crossings (mixed DPI/negative coordinates), forced occlusion, edge-clamped/refused movement, cancellation, and pause/resume. Verify that pointer movement after arrival does not restart following.
- [ ] Hands-on per-window input/pause controls, native close gestures, injected native/GPU failure isolation, and focus preservation across multiple views.
- [ ] Inspect sprite alpha edges over light/dark backgrounds: no dark fringe or opaque rectangle; compare `--opaque`. Expect nearest-sampled sprite art but antialiased text.
- [ ] Inspect short/long summaries, measured wrapping and `…`, clipped identities, supported Unicode/Nerd icons and missing-glyph replacement at several scales/font sizes. Check reduced line counts at larger sizes, terminal status while resting, activity-ID fallback, and unfocused/Unavailable/Preview labels.
- [ ] Manually check explicit missing family/file errors and invalid-file errors (no silent fallback), plus automatic bundled fallback on a system without the preferred Nerd Font installed.
- [ ] Change only the selected summary while paused and while the animation is unchanged: text must update without restarting animation or stealing focus. Switch chats and disconnect: no other chat's summary may leak into the bubble.
- [ ] Compare `run --no-bubble`: only the default 96 × 104 logical sprite remains; `--sprite-scale 1` gives 192 × 208. Confirm metadata is still present in status/headless output; assess screen-sharing privacy with non-sensitive sample summaries.
- [ ] macOS: test left-release/right-click menu, dismissal, each scoped action and explicit Close versus Quit; verify 1–9 Preview, Space and Q/Esc per-view close. Check five-point held drag, original-event handling, left-edge leave, lost mouse-up recovery and mixed-DPI focus/geometry. Non-macOS: retain left-drag/right-cycle behavior.
- [ ] Exercise two conversations: background attention/outcomes must not interrupt the selected conversation. Check Tab/CLI selection, ambiguous selection errors, retained unavailable focus after removal/disconnect/expiry, and terminal selection without historical cues. Test manual override and L/live resume without changing conversation, including while paused.
- [ ] Keep another app focused during startup without `--focus` and during demo transitions; verify it retains focus. Test explicit `--focus` separately.
- [ ] Drag between monitors with different display scales; inspect text clarity after rerasterization, nearest-scaled sprite art, padding, and clipping at several `--scale`/`--sprite-scale`/`--font-size` values. Changing only sprite scale must not resize bubble/text at the same physical scene scale. Synthetic 2× previews are not actual monitor-DPI acceptance.
- [ ] Check the default always-on-top request and compare `--normal-window-level` against ordinary windows.
- [ ] Press actual Option+I with another app focused: verify timed reveal/hide without activation, including paused/occluded windows. Check quiet time, explicit refresh, edge clamping/restoration and visible-window dragging. Registration or CLI smoke logs do not validate the OS chord. No synthetic OS input or Accessibility prompt was used.
- [x] Bounded private zero-view native Carbon registration and normal-exit cleanup: all four cases in Final Option+M validation passed, including M registration in a following fresh process. Deterministic collision tests passed; no real OS collision was exercised.
- [ ] Hands-on actual Option+M from another focused app: all eligible current views, repeat latch/release, pause/resume, zero views/later Open and no activation. Check focused-only M fallback and real OS collision behavior separately. Registration smoke does not prove key delivery or hotkey-driven motion.

## Related files

- [`src/doctor.rs`](src/doctor.rs), [`src/diagnostics.rs`](src/diagnostics.rs), [`src/build_info.rs`](src/build_info.rs): report, live process facts and recorded comparison; [`build.rs`](build.rs), [`build_support.rs`](build_support.rs): embedded fingerprint inputs and limitations.
- [Roadmap](ROADMAP.md): delivered menu/bootstrap foundation and remaining adapter coverage.
- [Djinn adapter guide](../djinn/docs/numen.md): optional sibling integration, not a Numen dependency.
- [Wisp](WISP.md): opt-in palette setup, menu flow, installer safety and recovery.
- [Architecture](ARCHITECTURE.md): delivered service/renderer boundaries and next milestones.
- [Protocol](PROTOCOL.md): initial local v1 messages, limits, sequencing, and lifecycle.
- [Modes](MODES.md): per-familiar shared-focused semantics, multi-view bindings and future mode choices.
- [Naming](NAMING.md): selected name and rationale.
- [`src/main.rs`](src/main.rs): CLI and startup; [`src/window.rs`](src/window.rs): desktop events and rendering.
- [`src/context_menu.rs`](src/context_menu.rs): typed declarative action catalog, native AppKit menu/tracking cancellation and retained original mouse-down for drag.
- [`src/motion.rs`](src/motion.rs), [`src/pointer.rs`](src/pointer.rs): pure bounded pursuit and read-only macOS pointer snapshots; coordinate contract in [Architecture](ARCHITECTURE.md#navigation-and-coordinate-contract).
- [`src/animation.rs`](src/animation.rs): expressions and player; [`src/assets.rs`](src/assets.rs): atlas validation and frame cache.
- [`src/bubble.rs`](src/bubble.rs): physical-pixel CPU composition, measured wrapping, status labels and preview generation.
- [`src/visibility.rs`](src/visibility.rs), [`src/hotkey.rs`](src/hotkey.rs): notification deadlines/quiet policy and independently owned macOS reveal/navigation registrations.
- [`src/typography.rs`](src/typography.rs), [font assets README](assets/fonts/README.md): font selection, metrics, raster/cache limits and fallback provenance/license.
- [`src/protocol.rs`](src/protocol.rs), [`src/activity.rs`](src/activity.rs), [`src/ipc.rs`](src/ipc.rs), [`src/display.rs`](src/display.rs): wire validation, reduction, transport, and live display.
