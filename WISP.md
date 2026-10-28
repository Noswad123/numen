# Wisp integration

## Overview

Numen owns the familiar-first fzf menu; Wisp's existing binary is already a generic launcher and needs **no source changes**. An optional Numen **Summon** action appears through the Wisp **palette** (`wisp palette`). The generated action has **no key**: no WM-prefix/static hint binding is automatically added.

Menu, installer, menu Open empty bootstrap and graceful graphical quit are delivered. **No actual home Wisp configuration was edited.** Installation below is an opt-in user command after delivery, not an action performed by this documentation update. No user profile or backups are automatically Git-committed.

## API / Interfaces

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
3. Choose a command supported by **every** selected row. Presets only Open; active familiars offer Info, Go to Mouse, Attach and Close according to advertised capabilities. Mixed types can have no common command: the menu explains this and offers Back, never partial skipping.
4. Open/Attach chooses one target separately per selection from fresh pages of **current connected protocol sources only**, including opt-in Djinn interactive chats. Other owners and earlier pending choices are excluded; one's own current attachment can be reselected. Empty lists offer Refresh/Back. There is no OS-process scan or blanket Djinn application/session discovery.
5. Submit one batch; the server atomically revalidates targets. Errors appear in the header for retry/refresh/Back. One accepted action exits the menu so the Wisp Kitty floater can close.

Command Escape/Back returns to familiars with selection retained. Target Escape/Back discards the **entire pending batch** and returns to commands. Familiar Escape exits. Ctrl-R refreshes the current stage. No hidden Open/Attach mutations occur on cancellation. Only one Djinn preset exists: opening additional copies takes repeated menu invocations, not duplicate preset rows; a multi-selection batch applies once per actual selection.

The menu pins the discovered `service_instance` and includes it on every discovery/action RPC. If Numen restarts while the menu is open, close and reopen the menu: it will not repin old selections to a new service whose familiar IDs may have been reused. The server checks that precondition before any dispatch. This is a generation guard, not an authentication secret; socket ownership/permissions remain the access boundary. Direct one-shot CLI commands and publishers can omit the guard for compatibility.

Merely entering menu/discovery never spawns or writes files. Choosing Open while offline calls `ensure_graphical` by default, launching the current executable as detached `--socket PATH run --empty` using `setsid`, null stdin/stdout and a private 0600 `service.log` beside the socket. Readiness is bounded to five seconds and does not acknowledge GPU initialization. No unattached Main is created, including when sources arrive. `--no-start` reports an error instead. Concurrent starts reuse the UDS-bind winner. Existing headless/outdated/unsafe services are never replaced or killed; stop an old foreground service manually once or choose another socket. Failure cleanup owns/kills only the invocation's child handle, never another PID. **Direct CLI Open does not auto-start.**

### Optional Djinn targets

With a Djinn source build containing the adapter, run `DJINN_NUMEN=1 djinn -s REF` or `DJINN_NUMEN=1 djinn session chat REF`. Open an existing chat in the Chat tab (idle is eligible), then choose Djinn preset → Open in Numen's menu. If Open bootstraps Numen, allow the adapter's default one-second reconnect retry and Refresh. Global dashboard/history do not publish new targets; this is not an all-sessions picker.

`DJINN_NUMEN_DISABLED=1` overrides enablement. For a custom socket, set `DJINN_NUMEN_SOCKET` to the same absolute path captured by the action's Numen `--socket`; the adapter requires the existing private directory/endpoint. Defaults match Numen's XDG-runtime/UID-temporary path. Enabling the adapter neither starts Numen nor installs/changes Wisp configuration.

Only the local owning-Worker ordinary interactive path is supported. Chat-tab presence renews every 5 seconds (20-second TTL); leaving Chat removes idle targets, while previously observed busy chats remain until idle. Reload/new backend changes producer instance and requires explicit reattachment. Existing-title summaries can be sensitive. See [adapter scope, capacity, states and privacy](README.md#optional-djinn-interactive-adapter); no OS scanning is involved.

## Edge Cases

### Installer safety and recovery

- Existing catalog must be a user-owned regular file; file symlinks, foreign/special files, malformed TOML, duplicate/conflicting Numen entries and catalogs exceeding 1 MiB are refused. A symlinked **parent directory is allowed**. If the catalog itself is a symlink, inspect it and explicitly pass its resolved regular-file target using `--file`; the installer does not follow it automatically.
- Installation appends while preserving existing bytes/comments/other entries and permissions. An identical entry is a no-op. New catalogs use mode 0600. Missing parent directories are not created. Catalog path resolution supports `~/`, rejects unresolved `$` variables and does no shell evaluation.
- On an existing-catalog change, output prints **Snapshot backup** (stable copy) and **Live inode backup (retains late editor writes)**. Snapshot file **and parent directory** are fsynced before displacement. The actual current catalog is renamed into the live-inode backup and rechecked against the baseline. The completed new file is published by **no-clobber hard link**, not blind rename.
- This is preservation-first, **not atomic compare-and-swap**: the catalog pathname may briefly be absent. An externally recreated path is never overwritten. Displaced concurrent edits remain preserved; failures attempt safe no-clobber restore and report recovery paths. Unsafe displaced nodes are never followed, deleted or restored.
- **Manual rollback:** stop/close editors that may still write through old file descriptors. Inspect the current catalog and **both** printed backups; the live-inode backup may contain newer edits than the snapshot. Preserve/merge those edits, then manually restore the appropriate reviewed copy or remove only the Numen stanza. Do not blindly copy the snapshot over current data or discard the live backup. If installation failed, inspect its reported recovery paths/current pathname before retrying. No automatic Git commits or blind rollback are performed.

### Menu and lifecycle limits

Labels/summaries are sanitized before terminal display and never shell-interpolated. `FZF_DEFAULT_OPTS`, `FZF_DEFAULT_OPTS_FILE` and `FZF_DEFAULT_COMMAND` are removed from fzf's environment. Unknown/duplicate output identities are rejected; selector output is capped at 64 KiB. Offline `presets` and the menu use the same `menu::presets` catalog.

Info reveals existing transient bubbles without selection/source mutation; `--no-bubble` still hard-disables display without redacting wire/state metadata. Font size 12, half-size sprite, Source Code Pro fallback, unique-source bindings, per-selection targets and existing motion rules remain unchanged. The menu does not enable source adapters or add persistent profiles; Djinn's adapter is separately opt-in.

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

Prior Numen validation: **224 tests passed (206 unit, 18 CLI)**, plus formatting, strict clippy, debug build and diff checks; not rerun for this documentation/menu-text update. Real fzf PTY tests auto-started an empty native GUI from Preset Open, registered a diagnostic target, opened one familiar without Main, exited the menu while the detached service stayed alive, and multi-selected two familiars for Info with source JSON unchanged. An existing-empty-GUI cancellation test covered target/command/familiar Escape with no mutation and retained selection. After generation guarding was added, the real fzf/bootstrap/Open/detachment/graceful-quit path passed again. Unit tests also reject stale scoped requests against reused IDs after restart. Graceful quit ACK/socket removal and native zero-window quit passed.

Separately reported Djinn adapter checks (real Rust headless interop: 21 lifecycle assertions; UI lifecycle tests and package typechecks) do not establish an actual interactive GUI or Wisp handoff. No actual interactive Djinn → graphical Numen handoff test was performed.

Only temporary test catalogs were installed; actual installed `wisp actions list` parsed the Numen row. **No actual home Wisp config was edited, no real Kitty launcher/WM-prefix handoff was tested, and actual Wisp palette → Kitty handoff remains manual.** Scripted inputs were PTY terminal inputs, not global OS key/pointer injection. Earlier positive native GPU Go to Mouse evidence remains unchanged.

## Related files

- [README](README.md), [Architecture](ARCHITECTURE.md), [Protocol](PROTOCOL.md), [Modes](MODES.md), [Roadmap](ROADMAP.md).
- [`src/menu.rs`](src/menu.rs): picker, catalog, capability intersection and target batches.
- [`src/bootstrap.rs`](src/bootstrap.rs): discovery and detached empty startup.
- [`src/wisp_action.rs`](src/wisp_action.rs): TOML generation, explicit install, backups and no-clobber recovery.
- [`src/main.rs`](src/main.rs), [`src/ipc.rs`](src/ipc.rs), [`src/protocol.rs`](src/protocol.rs), [`src/activity.rs`](src/activity.rs), [`src/window.rs`](src/window.rs): CLI, wire/store and root GUI lifecycle.
