# Wisp integration

## Overview

Numen owns the familiar-first fzf menu; Wisp's existing binary is already a generic launcher and needs **no source changes**. An optional Numen **Summon** action appears through the Wisp **palette** (`wisp palette`). The generated action has **no key**: no WM-prefix/static hint binding is automatically added.

Menu, installer, menu Open empty bootstrap and graceful graphical quit are delivered. **No actual home Wisp configuration was edited.** Installation below is an opt-in user command after delivery, not an action performed by this documentation update. No user profile or backups are automatically Git-committed.

## API / Interfaces

### Native macOS menu is separate

The GUI familiar now has its own AppKit `NSMenu`: **normal left-click release** opens it, and **right-click** opens the same menu instead of Preview cycling. The terminal/fzf `numen menu` and Wisp Summon entry below remain unchanged and available; no Wisp reinstall/binding is needed. Digits **1–9** remain Preview controls. Non-macOS has no native menu and retains left-drag/right-cycle.

Native actions are **Show status** (disabled by `--no-bubble`), **Go to mouse** (clicked view only, live pointer/navigation required), **Pause/Resume** (clicked view), **Settings**, **Close familiar**, and explicitly **Quit Numen (all familiars)**. Close uses the existing scoped retry path and last-close retains Numen; Quit performs normal service-wide root shutdown, never stopping Djinn. Q/Esc remain per-view close, not Quit. Sources cannot execute actions or auto-open the menu on updates.

Native Settings shares the three field/preset sets documented below. A choice submits one field through the nonblocking 32-entry `FamiliarControl::settings` queue → canonical Store → authoritative snapshot, not optimistic local state. Checks/accurate **Custom (number)** rows use canonical `f64` values, not renderer `f32`. Manual/demo have no settings IPC/navigation, but retain pause, permitted reveal and Close/Quit.

**Example:** left-click release → Settings → Text size → Large (16), then reopen → Close familiar to retain the service, or Quit Numen (all familiars) to exit all views. Opening/dismissal preserves cues, Preview/manual override, pause and navigation. Pending gestures/menu hold geometry and suspend motion clocks; held movement beyond five logical points commits native drag using the retained/revalidated original `LeftMouseDown` event, cancels motion and rebases the hidden anchor. CursorLeave cancels click only, not held drag; mouse-up coordinates alone cannot start drag. Held/native-drag state polls every 20 ms for lost mouse-up.

Synchronous native tracking suspends Rust GUI ticks; a 25 ms AppKit timer cancels on clicked-target removal, external service quit or `--exit-after`, not ordinary source changes. Latest registry and familiar/WindowId are revalidated afterward, ignoring stale/failed/closing per-view actions; service Quit is independent of target existence. See [architecture](ARCHITECTURE.md#native-familiar-click-menu) for adapter/dependency boundaries. No input taps, synthetic keys or new permissions are added.

| Command | Behavior |
| --- | --- |
| `numen menu [--no-start] [--fzf PATH]` | Interactive stdin/stderr TTYs required. Uses installed fzf; launch via Wisp run/summon, not exec. |
| `numen wisp-action` | Prints TOML **only**, with no writes or catalog lookup. |
| `numen wisp-action --install [--file PATH]` | Explicitly appends to the catalog reported by `wisp actions path`, or the supplied path. Parent must already exist. |
| `numen run --empty` | Graphical service with zero initial familiars/no Main; conflicts with manual/demo. |
| `numen service-info` | Queries API version 1, PID, unique `service_instance`, and graphical/lifecycle/navigation booleans; control readiness, not GPU readiness. |
| `numen quit` | Gracefully schedules global graphical service exit; does not stop source apps. |

Generated TOML has `id = "numen"`, `title = "Numen"`, `kind = "summon"` and an argv array equivalent to:

```text
[absoluteExecutable, "--socket", absoluteResolvedSocket, "menu"]
```

Paths are captured from the current executable and resolved socket when generated. Leading `--socket` avoids Wisp's work-directory heuristic. There is no shell interpolation and **Numen need not be installed on PATH**. Regenerate if the binary or socket path changes; a different existing `numen` entry is refused for manual resolution. There is no `--replace` flag.

## Data Flow

1. `wisp palette` → select **Numen** → Wisp Summon launches the terminal menu.
2. Select clearly labeled Running/Waiting familiar instances or Preset rows. Internal `familiar:` / `preset:` IDs keep the two types distinct. Tab multi-selects up to 16; Enter advances.
3. Choose a command supported by **every** selected row. Presets only Open; active familiars offer Info, Go to Mouse, Settings, Attach and Close according to advertised capabilities. Mixed types can have no common command: the menu explains this and offers Back, never partial skipping.
4. Open/Attach chooses one target separately per selection from fresh pages of **current connected protocol sources only**, including supported Djinn interactive chats discovered by default unless opted out. Other owners and earlier pending choices are excluded; one's own current attachment can be reselected. Empty lists offer Refresh/Back: open a Chat tab in an updated Djinn UI, check reporting is not opted out and the sockets match, allow reconnect, then Refresh. There is no OS-process scan or blanket Djinn application/session discovery.
5. Submit one batch; the server atomically revalidates targets. Errors appear in the header for retry/refresh/Back. One accepted action exits the menu so the Wisp Kitty floater can close.

Command Escape/Back returns to familiars with selection retained. Target Escape/Back discards the **entire pending batch** and returns to commands. Familiar Escape exits. Ctrl-R refreshes the current stage. No hidden Open/Attach mutations occur on cancellation. Only one Djinn preset exists: opening additional copies takes repeated menu invocations, not duplicate preset rows; a multi-selection batch applies once per actual selection.

The menu pins the discovered `service_instance` and includes it on every discovery/action RPC. If Numen restarts while the menu is open, close and reopen the menu: it will not repin old selections to a new service whose familiar IDs may have been reused. The server checks that precondition before any dispatch. This is a generation guard, not an authentication secret; socket ownership/permissions remain the access boundary. Direct one-shot CLI commands and publishers can omit the guard for compatibility.

### Global Go to Mouse (not a Wisp binding)

On a live navigation-capable macOS service, **Option+M** sends all current eligible familiars in that service to the live pointer without activation/focus stealing. This is Numen's Carbon shortcut, not a Wisp catalog key. It works with zero views as a no-op and remains available after later Open. Familiars run left/right, stop on sprite contact, celebrate once and restore the latest attached source expression; no ongoing follow or source outcome is added. Pause is preserved and movement waits for resume. The callback only queues bounded intent; the main GUI loop resolves the latest registry and skips closing/failed views.

Global Option+I reveal-all is unchanged. Independent I/M guards and repeat latches (until matching release) share one main-thread Carbon manager and a capture-free dispatcher with two weak targets. A collision warns without disabling the other key. `--no-hotkey` disables both global registrations, retaining focused local I/M and CLI controls; `--no-bubble` disables only global I, not M. Focused exact Alt+M moves only that view if global M is unregistered, suppressed when global M succeeds independently of I. No M/navigation is available after pointer initialization failure, on non-macOS or in manual/demo.

Broadcast is GUI-only and service-local, never cross-process: menu Go to Mouse still uses selected rows and CLI `go-to-mouse --familiar ID1 ID2` still requires explicit IDs. No wire implicit-all request was added. Native unregister is best-effort; see [cleanup limitation](ARCHITECTURE.md#live-display-and-output).

Merely entering menu/discovery never spawns or writes files. Choosing Open while offline calls `ensure_graphical` by default, launching the current executable as detached `--socket PATH run --empty` using `setsid`, null stdin/stdout and a private 0600 `service.log` beside the socket. Readiness is bounded to five seconds and does not acknowledge GPU initialization. No unattached Main is created, including when sources arrive. `--no-start` reports an error instead. Concurrent starts reuse the UDS-bind winner. Existing headless/outdated/unsafe services are never replaced or killed; stop an old foreground service manually once or choose another socket. Failure cleanup owns/kills only the invocation's child handle, never another PID. **Direct CLI Open does not auto-start.**

### Optional Djinn targets

With an updated Djinn build, run plain `djinn` and open a Chat tab, `djinn -s REF`, or `djinn session chat REF`; discovery is on by default without an enablement export. An existing idle chat is eligible. Then choose Djinn preset → Open in Numen's menu. If Open bootstraps Numen, allow the adapter's default one-second reconnect retry and Refresh. Global dashboard/history do not publish new targets; only explicit current Chat selections and previously observed busy chats (plus bounded terminal retention) are reported, not all sessions or Djinn OS processes.

`DJINN_NUMEN_DISABLED=1` explicitly opts out of reporting; legacy `DJINN_NUMEN=0` also works. Missing enablement/disablement defaults are true/false. Both accept exactly `true/yes/on/1/y` and `false/no/off/0/n`, case-sensitive without trimming. Malformed enablement becomes false, malformed disablement true; effective enablement is enable AND NOT disable in both CLI and backend. For a custom socket, set `DJINN_NUMEN_SOCKET` to the same absolute path captured by the action's Numen `--socket`; the adapter requires the existing private directory/endpoint. Defaults match Numen's XDG-runtime/UID-temporary path. Discovery neither starts Numen nor opens familiars, publishes history, reads raw prompts, changes Wisp/transcripts or makes model calls; Numen remains optional.

An already-running older Djinn UI needs **one restart with the updated Djinn binary**, then reopen the chat and explicitly reattach if the backend instance changed. Rebuilding alone does not update that running UI. This is separate from the Numen runtime-settings restart below; this documentation update performs neither restart nor home installation. For source fallback, run Bun from Djinn's `clients/djinn-ui/packages/opencode` so its OpenTUI/Solid preload loads; no enablement flag is needed.

Only the local owning-Worker ordinary interactive path is supported; native Rust ask/session run, mini, external attach, remote and v2 paths remain unsupported. Chat-tab presence renews every 5 seconds (20-second TTL); leaving Chat removes idle targets, while previously observed busy chats remain until idle. Reload/new backend changes producer instance and requires explicit reattachment. Titles/status are shared automatically and may be sensitive; opt out to prevent reporting. `--no-bubble` hides display only, not wire/state metadata. See [adapter scope, capacity, states and privacy](README.md#optional-djinn-interactive-adapter); no OS scanning is involved.

### Runtime Settings menu

Select existing familiars → **Settings** → field → preset. Settings must be advertised by every selection, independently of Go to Mouse; presets/headless/older services cannot edit. One invocation applies one field to all selected IDs atomically, then exits:

| Field | Presets |
| --- | --- |
| Sprite size | Small 0.25, Default 0.5, Large 1, XL 2 |
| Text size | Small 10, Default 12, Large 16, XL 20 |
| Bubble duration | Short 2, Default 5, Long 10, XL 30 seconds |

Field Back/Esc returns to commands; preset Back/Esc returns to fields. Ctrl-R discards and refreshes. No writes occur before a concrete preset; IDs/capabilities are rechecked immediately before the generation-sealed RPC, without bootstrap. Common preset values show names/numbers, differing values show **Mixed**, common custom values show **Unknown (number)** and absent values show **Unknown**. The CLI can instead edit multiple fields atomically: `numen settings --familiar main --sprite-scale .5 --font-size 16 --bubble-duration 5`. Any nonempty flag subset is accepted; explicit 1–16 unique IDs are required, never implicit all. Read complete values through `numen familiars`, not a settings-get command.

Edits are runtime-only per view. Startup defaults remain immutable for later Opens, even after editing Main or starting empty. Source bindings/cues/TTL, Preview, pause, player and navigation are preserved. Font/sprite edits redraw and resize around the physical foot-center where possible, including hidden-position restoration, without focus. Duration changes affect future accepted reveals/notifications only, not current deadlines, hidden visibility, quiet/history or disabled/always policies. No persistence, character/variant picker, placement controls or autostart is added; native dragging already exists.

**After deploying the new binary, explicitly restart an older running Numen service once:** rebuilding alone cannot add Settings to it. When ready, run `numen quit`, then `numen menu` (use the rebuilt full path and same `--socket` as needed), then Djinn preset → Open to reattach the existing chat. No Djinn restart is needed; its adapter reconnects. Legacy services without quit support require manual stop. Nothing automatically quits/replaces the user's service. Once updated, runtime settings edits require no restart.

This same one-time user-explicit restart enables Option+M and the native click menu: use `~/Projects/numen/target/debug/numen quit`, then `~/Projects/numen/target/debug/numen menu` → Djinn preset → Open existing chat. An older Homebrew binary may not be current. The old running service is otherwise unchanged and was left untouched; never restart automatically. No Djinn restart, persisted settings, Wisp reinstallation or home changes are required for this Numen update.

## Edge Cases

### Installer safety and recovery

- Existing catalog must be a user-owned regular file; file symlinks, foreign/special files, malformed TOML, duplicate/conflicting Numen entries and catalogs exceeding 1 MiB are refused. A symlinked **parent directory is allowed**. If the catalog itself is a symlink, inspect it and explicitly pass its resolved regular-file target using `--file`; the installer does not follow it automatically.
- Installation appends while preserving existing bytes/comments/other entries and permissions. An identical entry is a no-op. New catalogs use mode 0600. Missing parent directories are not created. Catalog path resolution supports `~/`, rejects unresolved `$` variables and does no shell evaluation.
- On an existing-catalog change, output prints **Snapshot backup** (stable copy) and **Live inode backup (retains late editor writes)**. Snapshot file **and parent directory** are fsynced before displacement. The actual current catalog is renamed into the live-inode backup and rechecked against the baseline. The completed new file is published by **no-clobber hard link**, not blind rename.
- This is preservation-first, **not atomic compare-and-swap**: the catalog pathname may briefly be absent. An externally recreated path is never overwritten. Displaced concurrent edits remain preserved; failures attempt safe no-clobber restore and report recovery paths. Unsafe displaced nodes are never followed, deleted or restored.
- **Manual rollback:** stop/close editors that may still write through old file descriptors. Inspect the current catalog and **both** printed backups; the live-inode backup may contain newer edits than the snapshot. Preserve/merge those edits, then manually restore the appropriate reviewed copy or remove only the Numen stanza. Do not blindly copy the snapshot over current data or discard the live backup. If installation failed, inspect its reported recovery paths/current pathname before retrying. No automatic Git commits or blind rollback are performed.

### Menu and lifecycle limits

Labels/summaries are sanitized before terminal display and never shell-interpolated. `FZF_DEFAULT_OPTS`, `FZF_DEFAULT_OPTS_FILE` and `FZF_DEFAULT_COMMAND` are removed from fzf's environment. Unknown/duplicate output identities are rejected; selector output is capped at 64 KiB. Offline `presets` and the menu use the same `menu::presets` catalog.

Info reveals existing transient bubbles without selection/source mutation; `--no-bubble` still hard-disables display without redacting wire/state metadata. Font size 12 and half-size sprite remain startup defaults, now editable per view. Source Code Pro fallback, unique-source bindings, per-selection targets and existing motion rules remain unchanged. The menu does not enable source adapters or add persistent profiles; updated Djinn builds default to discovery for the supported local interactive path, independently of Wisp setup.

Close removes only selected views; closing the last retains the service/hotkey. Cancelling Open after bootstrap may leave an empty resident service: use `quit` to stop it. Quit requires the current graphical shutdown callback; headless/legacy handlers without one reject it. ACK `{"quitting":true}` means scheduled, not completed. Callback delivery follows ACK flush and client EOF (or 500 ms), ignores later pipelined input, and coalesces root event-loop exit/drop cleanup of all views, server/socket and hotkey. No source applications or metadata PIDs are killed. Service-info includes `service_instance` but no shutdown flag.

## Example usage

Use the full local debug-binary path; these are **manual setup commands**, not actions already performed:

```sh
# Inspect the catalog location (read-only):
wisp actions path
# Preview generated TOML only:
~/Projects/numen/target/debug/numen wisp-action
# Opt in after reviewing the preview and installer safety above:
~/Projects/numen/target/debug/numen wisp-action --install
# If needed, explicitly name the inspected resolved regular-file target:
~/Projects/numen/target/debug/numen wisp-action --install --file /absolute/resolved/actions.toml
# Read back, then exercise the actual palette handoff manually:
wisp actions list
wisp palette
```

For a custom service socket, include global `--socket /absolute/private/events.sock` when generating/installing the action. Regenerate after moving the binary or changing that path and resolve any differing existing entry manually.

```sh
# Direct interactive menu, without installing anything:
~/Projects/numen/target/debug/numen menu
~/Projects/numen/target/debug/numen menu --no-start
# Explicit empty startup is also available:
~/Projects/numen/target/debug/numen run --empty
# In another terminal:
~/Projects/numen/target/debug/numen service-info
~/Projects/numen/target/debug/numen quit
```

### Validation boundaries

Confirmed after the latest poll/click-race fix with the command-scoped macOS 26.5 SDK workaround: **278 passed (257 unit + 21 CLI), one existing visual-preview test ignored**. `cargo fmt --check`, `cargo test --offline --locked`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo build --offline --locked` and `git diff --check` all passed; the updated Numen binary was built. These are reported main-agent results, not checks rerun by this documentation edit.

An independent temporary AppKit/winit window used the production `context_menu.rs` popup adapter on the main thread for three successful popup/autocancellations (**380/378/353 ms**), using `Arc<AtomicBool>` and the native 25 ms event-tracking `NSTimer`. It exited 0 without errors. This validates native popup construction/placement API use, timer cancellation and repeated tracking cleanup only, not physical click/drag/item selection or Wisp palette handoff. No synthetic OS keys/mouse, hotkeys or existing-user-app controls were used. Actual hands-on Close versus Quit, Esc, focus, mixed-DPI and physical dragging remain manual. No app restart or home configuration change occurred; prior source-handoff/settings/Option+M evidence below remains separate. This documentation edit ran no commands/tests or app controls and made no hooks/commits.

Final Option+M validation: the full Rust suite passed **255 tests (234 unit + 21 CLI)**, with one existing visual-preview test ignored; formatting, strict all-target Clippy and build passed. A bounded private zero-view GUI harness verified actual Carbon registration: M only with `--no-bubble`, both I/M normally, neither with `--no-bubble --no-hotkey`, then M again in a following fresh process. All four exited code 0 and removed their own sockets. Collision behavior was tested deterministically, not with real OS collisions; both keys registered successfully in the normal native case. No synthetic OS keys, actual key presses, input taps, permission requests or existing-user-service control/restarts occurred. This proves registration and normal-exit cleanup, not background delivery or hotkey-driven motion. Actual Option+M OS-chord/background-focus acceptance remains manual; historical CLI-driven native motion evidence is separate. These reported checks were not rerun by this documentation edit. This is not Wisp palette → Kitty acceptance, and the documented best-effort native-unregister failure limitation remains.

Prior Numen validation: **224 tests passed (206 unit, 18 CLI)**, plus formatting, strict clippy, debug build and diff checks; not rerun for this documentation/menu-text update. Real fzf PTY tests auto-started an empty native GUI from Preset Open, registered a diagnostic target, opened one familiar without Main, exited the menu while the detached service stayed alive, and multi-selected two familiars for Info with source JSON unchanged. An existing-empty-GUI cancellation test covered target/command/familiar Escape with no mutation and retained selection. After generation guarding was added, the real fzf/bootstrap/Open/detachment/graceful-quit path passed again. Unit tests also reject stale scoped requests against reused IDs after restart. Graceful quit ACK/socket removal and native zero-window quit passed.

Separately reported Djinn adapter checks cover headless real-wire outcomes/reconnects, UI lifecycle tests and package typechecks. A real interactive idle-chat smoke on 2026-10-28 also passed: source Djinn TUI → real fzf Open/target picker → native Metal familiar with title/idle state; source exit retained the unavailable exact pin/summary, last Close retained the service, and quit removed the socket. Its private fixture had zero conversation messages; no model turn, installed-binary or home/Wisp configuration change occurred. This does not validate live outcome animations or the Wisp launcher.

Runtime-settings validation passed 246 automated tests (225 unit + 21 CLI; one existing visual-preview test ignored), formatting, strict Clippy and build checks. A real fzf Settings flow edited a native view on a private Apple M2 Pro/Metal service; independent resize, hidden-anchor restoration, future-only duration, atomic batches and unchanged future-Open defaults passed, including paused views. Test processes cleaned up their sockets; the user's running service and home/Wisp configuration were untouched. This is not Wisp palette → Kitty acceptance. Hands-on mixed-DPI, composition and focus checks remain manual.

Only temporary test catalogs were installed; actual installed `wisp actions list` parsed the Numen row. **No actual home Wisp config was edited, no real Kitty launcher/WM-prefix handoff was tested, and actual Wisp palette → Kitty handoff remains manual.** Scripted inputs were PTY terminal inputs, not global OS key/pointer injection. Earlier positive native GPU Go to Mouse evidence remains unchanged.

## Related files

- [README](README.md), [Architecture](ARCHITECTURE.md), [Protocol](PROTOCOL.md), [Modes](MODES.md), [Roadmap](ROADMAP.md).
- [`src/context_menu.rs`](src/context_menu.rs): native GUI menu and typed actions, separate from the fzf/Wisp surface.
- [`src/menu.rs`](src/menu.rs): picker, catalog, capability intersection and target batches.
- [`src/bootstrap.rs`](src/bootstrap.rs): discovery and detached empty startup.
- [`src/wisp_action.rs`](src/wisp_action.rs): TOML generation, explicit install, backups and no-clobber recovery.
- [`src/main.rs`](src/main.rs), [`src/ipc.rs`](src/ipc.rs), [`src/protocol.rs`](src/protocol.rs), [`src/activity.rs`](src/activity.rs), [`src/window.rs`](src/window.rs): CLI, wire/store and root GUI lifecycle.
