# Wisp integration

## Overview

**Current default is unattached-first.** Bare `numen` opens one unattached familiar via existing GUI service or bounded detached `run --empty` bootstrap; repeat for another independent familiar in the same service (16 total max). `numen run` remains foreground with unattached Main. Production Main/new unbound views never attach from register/state/reconnect/expiry; exact bound-pin reconnect behavior is unchanged. Historical attached-only Open and graphical Main auto-selection descriptions below are **SUPERSEDED**; only headless/test compatibility constructors retain the auto rule.

fzf preset **Open** is immediate/unattached, with no target picker. **Open attached…** retains the old picker, whole-batch cancellation and remembered style/position restore; presets advertise `open` and `open-attached`. Both may bootstrap offline unless `--no-start`. Direct `numen open` also permits no target but requires a running GUI; bare launch is the bootstrap convenience. Read-only controls/discovery remain unchanged.

Click **Attach to app / chat** for a supported live protocol app/chat or **Open another familiar** for another unattached pet. The latter is live-service-only/service-scoped, unlike Quit: bounded queue → canonical Store Open with no target → worker graphical/capacity recheck → committed snapshot, no optimistic view, exit or source execution. Attach retains exact tuple/incarnation/stale guards. First manual Attach preserves appearance/location without destination preferences; unattached edits are runtime-only/no saved key, while new explicit edits after attachment can remember the chat. Restore remembered chat style/position with targeted CLI Open or fzf Open attached….

Root v1 `unattached_open` reflects actual multi-familiar support; legacy missing means false. Launcher/default fzf Open checks the flag and seals `service_instance`. An old service needs one explicit manual Numen restart after deployment, then `numen`, never auto-kill/replacement or a Djinn restart. No OS scan, dummy PIDs/sources or app launches. Unbound labels are **Unattached**, with bubble **Unattached familiar** / **Click for actions** / **Unattached**; Preview unchanged.

**Earlier unattached-first delivery:** the debug binary was installed and installed help/Djinn interop passed; read-only doctor found an older reachable service and left it untouched. See [historical evidence](README.md#unattached-first-validation-status). This does not establish installation or desktop acceptance of the Palette/direct-exec migration below.

**Current Wisp contract:** the native prefix menu is user-facing **Palette**. Option+Space compositor bindings both show Palette and enter mode; **n → Numen** dispatches the generated direct `exec` action. It opens one unattached familiar, reuses/bootstrap-starts the graphical service, and repeated launches add independent views up to 16. Click **Attach to app / chat** for a supported live protocol target. This default needs neither Kitty nor fzf.

Numen's explicit `numen menu` remains an advanced familiar-first TTY/fzf interface, not the default launch. Wisp's explicit secondary `wisp menu` is its Kitty/fzf action selector; both Wisp interfaces share one catalog/dispatcher. Earlier Summon/no-key/terminal-default descriptions are superseded. This docs-only edit performs no setup, installation, restart, GUI controls or commits; [current local status](#wisp-integration-validation) distinguishes on-disk edits from deployment.

## API / Interfaces

### Wisp Palette versus CLI menu

- `wisp palette [show|close|text]` is the native passive helper (`show` by default). It never daemon-delegates, captures keys or enters mode itself. The compositor must also enter its Wisp mode/submap. `wisp hint` remains a compatibility alias.
- Generated bindings show/close Palette; Space stays in mode and never auto-opens the CLI menu. Escape closes/cancels; action keys close Palette, dispatch `wisp action <id>` and leave mode.
- CLI Menu selection calls `launchActionDirect(cfg, id)` **locally**, using its own resolved catalog—the one the picker displayed—not an ID-only request to a potentially different daemon catalog. The daemon may launch the Menu surface, but does not dispatch its selection. Standalone `wisp action` delegation policy, canonical resolution, overlay locks, failure recovery and exec supervision are unchanged.
- `wisp menu` explicitly opens Kitty/fzf. Its client delegates through the stable existing request `run --id palette --title 'Wisp Menu' --path '' --dir '' --layout '' -- /usr/bin/env -- <client context assignments> <client self> _palette` (argv schematic). The wrapper carries the full client-resolved WISP context plus caller delegation/fzf settings, including explicit empties when unset. `/usr/bin/env` has first argument `--`, preventing target inference; the final executable is the client, not the daemon. This supports older/different-path daemons without changing the strict ordinary-child Hyprland context guard. The earlier direct-client-self-only payload is superseded. See [full context list](../wisp/README.md#api--interfaces). No new protocol or daemon restart is needed merely for the rename.
- Internal `_palette`, surface ID `palette`, mode `run`, `WISP_HINT_*`, helper names and PID/overlay state intentionally remain unchanged. See [Wisp README](../wisp/README.md#actions-and-palette) for helper configuration and platform details.

### Native macOS menu is separate

**Native chat picker implemented; automated/native input-free verification confirmed, physical selection pending.** The click menu now offers **Change attachment** for a bound familiar or **Attach to app / chat** when unbound, without changing the Wisp/fzf flow or requiring Wisp reinstallation. It lists eligible **live exact `(source, instance, activity)`** targets directly, excluding other owners even when their pins are disconnected/reserved. Self-current stays enabled/checked. Choices are direct through 16, otherwise paged in groups of 16 across the full 512-target bound. Bounded sanitized labels with unique ordinals/full-tuple tooltips distinguish duplicate titles/instances; no process drill-down is required.

Example: left-click release → Change attachment → select an eligible chat/page. An unavailable current pin is disabled context; **No compatible live chats** is disabled when none qualify. **Chat list updating—reopen menu** is disabled on busy/unready cache or applied-view mismatch. Reopen to retry; the tracked menu is frozen, and its action index keeps the original owned choices, never a reordered memo.

This is an internal GUI control, not a new wire command/capability. Root familiar ID/WindowId/applied binding and available fresh-memo checks precede worker-authoritative service instance, attachment generation, exact tuple, source-registration incarnation and live ownership checks. A → B → A and same-tuple re-registration make old choices stale; ordinary metadata changes need no activity-sequence match. Failure warns/recommends reopen and leaves the attachment unchanged. Shared 32-entry `try_send` acceptance is not commit, with no optimistic view changes; immutable Arc memo reads use `try_lock` (512 targets/16 bindings), not GUI UDS, fzf or file I/O. CLI/fzf Attach retains its existing wire/root-generation behavior.

Retarget keeps current appearance/location/pause/Preview/manual override/navigation and clears the old cue as existing Attach does. It does not load destination saved preferences into this existing familiar (Open only); later explicit settings/drags remember the new logical key, never live bindings. Always-present internal runtime binding is independent of preference/Forget epochs, so Forget does not invalidate chat choices and saving need not be disabled. Manual/demo have no attachment control; other action gates remain unchanged. No source launch/resume/cancel, auto-Open or source-driven popup is added. See [internal API/data flow](PROTOCOL.md#native-chat-picker-internal-interface).

**Main-agent checks confirmed: 386 passed (342 unit + 7 build-fingerprint + 37 CLI), one existing visual-preview test ignored**; formatting, strict all-target Clippy, offline `--locked` tests/build and diff checks passed with SDK 26.5. **NativePopupInputFree** production-adapter fixtures passed: 4 choices **393 ms**, 512 choices **374 ms**, unavailable/0 choices **378 ms**, exit 0/no errors. Native build, pagination/tooltips and cancellation retention/cleanup were covered. Time was measured at `show` return before canceller join, requiring ≥250 ms, not inferred from helper join delay. Stub `FocusTarget` and exact preset table were used, not the service/root Store; pure/IPC tests cover the full contract, combined memo bounds, target-only publication, unbound/stale generations and retarget/source-animation state preservation. Runtime reviewers reported no bugs after correcting the failed-state fixture to `attention: false`. Wiring is complete with one App service/control lifetime, applied-binding/memo comparison and worker service-instance guarding; no root snapshot service-stamp rewrite is required.

No actual OS input/item selection/drag/hotkeys/user-app controls occurred in the picker fixtures. Physical **NativeChoiceSelected** remains manual; the USER-REPORTED older workflow does not verify it. **Historical picker installation:** the earlier installed-binary check observed `not_running` and passed 1 interop test / 46 assertions. See [earlier installation/interop evidence](README.md#unattached-first-validation-status). Home Wisp configuration was untouched by those fixtures, not by the later manual on-disk migration.

The GUI familiar has its own AppKit `NSMenu`: **normal left-click release** opens it, and **right-click** opens the same menu instead of Preview cycling. This is separate from Wisp Palette and the advanced terminal/fzf `numen menu`. Digits **1–9** remain Preview controls. Non-macOS has no native menu and retains left-drag/right-cycle.

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

Generated TOML has `id = "numen"`, `title = "Numen"`, `kind = "exec"`, `key = "n"` and exactly three argv elements:

```text
[absoluteExecutable, "--socket", absoluteResolvedSocket]
```

Paths are captured from the current executable and resolved socket when generated. Leading `--socket` avoids Wisp's work-directory heuristic. The argv array preserves quoting, and **Numen need not be installed on PATH**. Wisp applies `os.ExpandEnv` to every action argument, so Numen rejects any `$` in the executable or socket path, including a literal dollar sign, before catalog lookup or writes. Use paths without `$`; TOML quoting does not disable that expansion. Regenerate if the binary or socket path changes; a different existing `numen` entry is refused for manual resolution. There is no `--replace` flag.

## Data Flow

Default: compositor Option+Space → show passive Palette **and** enter mode → **n** → close Palette / `wisp action numen` / exit mode → direct bare Numen → graphical service reuse or bounded empty bootstrap → one generation-sealed unattached Open → native view. Repeat to add views (16 total); click Attach for an explicit supported app/chat. No terminal intermediary or source execution is involved.

### Advanced explicit terminal menu

Launch `numen menu` in a terminal, or explicitly wrap it with Wisp `run`/`summon` (never the generated default `exec`):

1. `wisp run --id numen-menu -- numen menu` → Numen's terminal menu.
2. Select clearly labeled Running/Waiting familiar instances or Preset rows. Internal `familiar:` / `preset:` IDs keep the two types distinct. Tab multi-selects up to 16; Enter advances.
3. Choose a command supported by **every** row. Presets offer Open and Open attached…; active familiars offer Info, Go to Mouse, Settings, Attach and Close as advertised. No-common-command selections get explanation/Back, never partial skipping.
4. Open immediately submits unattached items. Only Open attached…/Attach chooses one target per selection from fresh connected-protocol pages, excluding other owners and earlier pending choices while allowing self-current. Empty lists offer Refresh/Back. There is no OS scan or blanket session discovery.
5. Submit one batch; the server atomically revalidates targets. Errors appear in the header for retry/refresh/Back. One accepted action exits the menu so the Wisp Kitty floater can close.

Command Escape/Back returns to familiars with selection retained. Target Escape/Back discards the **entire pending batch** and returns to commands. Familiar Escape exits. Ctrl-R refreshes the current stage. No hidden Open/Attach mutations occur on cancellation. Only one Djinn preset exists: opening additional copies takes repeated menu invocations, not duplicate preset rows; a multi-selection batch applies once per actual selection.

The menu pins the discovered `service_instance` and includes it on every discovery/action RPC. If Numen restarts while the menu is open, close and reopen the menu: it will not repin old selections to a new service whose familiar IDs may have been reused. The server checks that precondition before any dispatch. This is a generation guard, not an authentication secret; socket ownership/permissions remain the access boundary. Direct one-shot CLI commands and publishers can omit the guard for compatibility.

### Global Go to Mouse (not a Wisp binding)

On a live navigation-capable macOS service, **Option+M** sends all current eligible familiars in that service to the live pointer without activation/focus stealing. This is Numen's Carbon shortcut, not a Wisp catalog key. It works with zero views as a no-op and remains available after later Open. Familiars run left/right, stop on sprite contact, celebrate once and restore the latest attached source expression; no ongoing follow or source outcome is added. Pause is preserved and movement waits for resume. The callback only queues bounded intent; the main GUI loop resolves the latest registry and skips closing/failed views.

Global Option+I reveal-all is unchanged. Independent **I/M/C** guards and matching-release latches share one main-thread Carbon manager and a capture-free dispatcher with three weak slots. Collisions do not disable other keys. `--no-hotkey` disables all three globals, retaining supported focused local I/M/C and existing CLI controls; `--no-bubble` disables I only. Focused exact Alt+M/Alt+C moves only that view when its global key is absent. M requires pointer initialization; C does not. Manual/demo and unsupported platforms have no M/C movement support.

**Option+C is Numen's global random-corner control, not a Palette/catalog binding.** Each current eligible view runs to an independently sampled corner of its current usable display; no source/preference mutation or arrival celebration is added. See [canonical controls](README.md#random-corners-optionc) and [final installed registration/interop checks, 429-test/native evidence and manual limits](README.md#optionc-validation-and-deployment). Installed private C/M registration and normal release/reacquisition passed; physical delivery remains manual. Final installed doctor found the default service `not_running` and started nothing: use bare `numen`, then Option+C. If an older service is running later, only an explicit user-chosen Numen restart loads the new code; installation does not upgrade it in place. No user Numen controls were used. The authorized AeroSpace reload is already active, with main alt-c removed and shift-alt-c preserved; no further reload is needed.

Broadcast is GUI-only and service-local, never cross-process: menu Go to Mouse still uses selected rows and CLI `go-to-mouse --familiar ID1 ID2` still requires explicit IDs. No wire implicit-all request was added. Native unregister is best-effort; see [cleanup limitation](ARCHITECTURE.md#live-display-and-output).

Merely entering menu/discovery never spawns or writes files. Choosing Open while offline calls `ensure_graphical` by default, launching the current executable as detached `--socket PATH run --empty` using `setsid`, null stdin/stdout and a private 0600 `service.log` beside the socket. Readiness is bounded to five seconds and does not acknowledge GPU initialization. No unattached Main is created, including when sources arrive. `--no-start` reports an error instead. Concurrent starts reuse the UDS-bind winner. Existing headless/outdated/unsafe services are never replaced or killed; stop an old foreground service manually once or choose another socket. Failure cleanup owns/kills only the invocation's child handle, never another PID. **Direct CLI Open does not auto-start.**

### Optional Djinn targets

With an updated Djinn build, run plain `djinn` and open a Chat tab, `djinn -s REF`, or `djinn session chat REF`; discovery is on by default without an enablement export. An existing idle chat is eligible. Then choose Djinn preset → **Open attached…** → the chat in Numen's menu. If Open attached… bootstraps Numen, allow the adapter's default one-second reconnect retry and Refresh. Plain **Open** creates an unattached familiar without a chat picker. Global dashboard/history do not publish new targets; only explicit current Chat selections and previously observed busy chats (plus bounded terminal retention) are reported, not all sessions or Djinn OS processes.

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

Edits affect the selected live views; production live services now also remember explicit appearance edits and completed manual drags. Original startup defaults remain immutable fallback values, even after editing Main or starting empty; saved style overrides them on explicit Open. Source bindings/cues/TTL, Preview, pause, player and navigation are preserved. Font/sprite edits redraw and resize around the physical foot-center where possible, including hidden-position restoration, without focus. Duration changes affect future accepted reveals/notifications only, not current deadlines, hidden visibility, quiet/history or disabled/always policies. Named profiles, character/variant picker, positioning controls and autostart remain future work.

**After deploying the new binary, explicitly restart an older running Numen service once:** rebuilding alone cannot add Settings to it. When ready, run `numen quit`, then `numen menu` (use the same `--socket` as needed), then Djinn preset → **Open attached…** → the existing chat. No Djinn restart is needed; its adapter reconnects. Legacy services without quit support require manual stop. Nothing automatically quits/replaces the user's service. Once updated, runtime settings edits require no restart.

This same one-time user-explicit restart enables Option+M, the native click menu and saved preferences: use `numen quit`, then `numen menu` → Djinn preset → **Open attached…** → the existing chat. These commands assume `numen` resolves to the updated installed binary in `~/.local/bin`, not an older Homebrew binary. Never restart automatically. No Djinn restart or Wisp reinstallation is required. The main agent confirmed the latest saved-preferences build installed into `~/.local/bin/numen`; no running user service was restarted or home preference files touched during development. Production live startup uses the preference path below.

### Saved preferences and Wisp boundaries

Offline menu Open still launches ordinary production `run --empty`, now with saved preferences enabled by default; merely entering menu/discovery still starts/writes nothing. Default is nonempty **absolute** `$XDG_CONFIG_HOME/numen/preferences.json`, else absolute `$HOME/.config/numen/preferences.json`; relative configured roots fail, and missing config ancestors are not recursively created. Config/HOME/dotfile ancestor symlinks are permitted, not the private owned 0700 leaf or owned single-link regular 0600 file. The leaf/lock may be created at startup; missing JSON waits until the first remembered edit. This is **not the Wisp actions catalog** and needs no Wisp installer change.

For total opt-out, explicitly start `numen run --empty --no-preferences`, then use `numen menu --no-start`: runtime Settings still works, with all preference resolution/read/write bypassed. An explicit `run --preferences-file /absolute/private/numen/preferences.json` overrides the default and conflicts with opt-out. These are `run` flags, not `menu` or generated Wisp-action options. Manual/demo/headless do no preference I/O.

The key is `(source,activity)` only, assuming stable logical activity IDs across instances. Only sprite scale/font size/duration and optional manual foot-center logical x/y are persisted, never titles/prompts, pins/open windows/autostart/cues/pause/navigation. Open loads saved style/placement or startup fallback; Attach/focus/next keep current appearance/location and associate future edits with the new key. Source/reconnect events never preference-create or retarget views. Same-key instances remain independent; remembered edits never autoapply to siblings. Go to Mouse/layout/resize/clamp/close/source events do not save placement.

The native click menu adds **Forget saved preferences** only for a writable binding. It freezes/rechecks that binding; it is separate from fzf Settings and requires no Wisp reinstallation. CLI equivalent: `numen preferences forget --familiar main build-pet`; wire `familiar.preferences.forget {ids}` returns `{forgotten: IDs}` after whole-batch validation. It removes style and placement, preserves **current** settings/location, invalidates shared-key pending drags even for absent records, and does not immediately resave. Close keeps the record; later new explicit Settings/drag can remember again. The `preferences` capability is distinct from Settings/navigation.

ACKs/queue acceptance are not durable-save guarantees. `numen doctor [--json]` reads only the service's cached optional preferences status, never the file; old missing fields are unknown. Strict 128-record/256-KiB documents, read-only lock conflicts, disabled saves on unsafe/malformed/future files, bounded background I/O and preservation-first snapshot/live/stage recovery are detailed in [Protocol](PROTOCOL.md#saved-preferences) and [Architecture](ARCHITECTURE.md#saved-preferences-storage-and-recovery). Publication is not atomic CAS; backups are not securely erased by Forget. Do not confuse preference recovery with Wisp catalog recovery below.

## Edge Cases

### Installer safety and recovery

- No `--replace` or automatic legacy migration exists. An old `summon`, no-key or `menu` entry conflicts: review the preview and manually update the existing stanza, preserving unrelated edits. Another action whose key trims and ASCII-case-folds to `n` (for example `N`, ` n ` or tab-padded `n`) is rejected **before any writes**, even if an identical Numen entry is already present. Resolve that conflict manually; idempotence does not bypass the guard. The generated key remains exactly `n`.
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

For standalone use, run `numen`, then click Attach to app / chat or Open another familiar. Optional Wisp setup generates the same bare-launch behavior under native Palette key **n**.

Use the intended updated Numen executable (absolute path if not installed); these are **manual setup commands**, not actions performed by this edit:

```sh
# Inspect the catalog location (read-only):
wisp actions path
# Preview generated TOML only:
numen wisp-action
# Opt in after reviewing the preview and installer safety above:
numen wisp-action --install
# If needed, explicitly name the inspected resolved regular-file target:
numen wisp-action --install --file /absolute/resolved/actions.toml
# Read back; review/generate compositor bindings separately:
wisp actions list
wisp bindings aerospace # Or: wisp bindings hyprland
# After explicitly applying/reloading reviewed bindings: Option+Space, then n.
# Standalone rendering only, NOT key capture or mode entry:
wisp palette show
wisp palette close
```

For a custom service socket, include global `--socket /absolute/private/events.sock` when generating/installing the action. Regenerate after moving the binary or changing that path and resolve any differing existing entry manually.

```sh
# Direct interactive menu, without installing anything:
numen menu
numen menu --no-start
# Explicit advanced TTY handoff (requires Kitty/fzf):
wisp run --id numen-menu -- numen menu
# Explicit empty startup is also available:
numen run --empty
# In another terminal:
numen service-info
numen quit
```

### Wisp integration validation

**Final installation and direct-exec reuse smoke confirmed; authorized AeroSpace reload now confirmed.** The earlier pending-reload status below is superseded by the current local status. The following are supplied results, not commands rerun by this docs-only edit. The smoke used no user app/service restart or global input; the later authorized compositor reload is separate.

#### Code and installed-command checks

- Fresh Numen suite after the normalized-key guard fix: **417 passed (363 unit + 7 build-fingerprint + 47 CLI)**, one visual test ignored; formatting and strict offline locked Clippy green with command-scoped SDK 26.5. An earlier full attempt hit an unrelated fixture socket-permission failure; its unchanged rerun passed before the final guard-fix checks.
- Wisp: `CGO_ENABLED=0 go test ./...` and `CGO_ENABLED=0 go vet ./...` green; **14 Python headless helper tests** and bash/zsh syntax checks passed.
- Updated Wisp, native helper and completions installed via `CGO_ENABLED=0 make install`; updated debug Numen installed via the SDK 26.5 Cargo install workflow.
- Installed opt-in Wisp/Numen integration: **1 pass**, verifying `exec`, key `n`, exact three-element argv/no `menu`, temporary installation/idempotence and resolution without a terminal. Installed Wisp help distinguishes Palette/Menu; `actions list` reports Numen `exec n`; `palette text` includes `n Numen`.
- Installed `numen wisp-action --install --file ~/.config/wisp/actions.toml` reported **already installed**, idempotently with no writes.
- After the final Menu selection fix, Wisp tests, vet, installation and the installed opt-in Numen integration (**1 pass**) were rerun successfully. Fake-daemon A/client-B tests cover the same ID with different exec commands and a B-only action, verifying local dispatch without a daemon action request. Independent final review: **READY**, no concrete blockers.
- Final installed Djinn interoperability rerun: `NUMEN_TEST_BINARY=/Users/jdawson/.local/bin/numen bun test test/numen/interoperability.test.ts --timeout 30000` → **1 pass, 46 assertions**.

#### Installed native exec smoke (reuse, not offline bootstrap)

Final evidence after the Menu fix: `/var/folders/4x/vb2wqbds6_x1sh38zt3l17vc0000gq/T/opencode/wn-ymim71uo/evidence.json`. The main agent reran `installed-wisp-numen-smoke.py` with `/opt/homebrew/bin/python3`; result **`passed: true`**. The earlier `wn-rxi8u_vo` run below is no longer the final smoke.

- An owned private graphical service was **prestarted** with `--empty --no-hotkey --no-preferences --normal-window-level`. The actual installed generator produced an `exec`/`n`/three-argv action into a temporary catalog.
- Palette `text`, `show`, default show and compatibility `hint` aliases agreed with `WISP_HINT_BACKEND=stdout`. This checks command/text parity, not native Palette rendering or key capture.
- Two noninteractive actual `wisp action numen` calls, with `WISP_BACKEND=invalid` and an empty PATH directory (no Kitty/fzf), opened **one then two unattached native Metal views** on Apple M2 Pro in the same prestarted private service; sources, targets and activities remained zero.
- Wisp's exec log recorded launch/completion for both calls; Store polling confirmed the Opens. Wisp's launch ACK alone means process start, not a committed Open or rendered view.
- The test-owned service quit 0, removed its socket and left no test processes or preferences. This validates the installed direct-exec **reuse** path, not offline bare bootstrap, physical Option+Space/N delivery, native menu selection or focus acceptance.

**Earlier smoke provenance only:** `/var/folders/4x/vb2wqbds6_x1sh38zt3l17vc0000gq/T/opencode/wn-rxi8u_vo/evidence.json` recorded PID 37472, instance `37472-1793309709067776000-0`, and installed binary SHA-256 values Wisp `9c5b6147ebfab82f1c2492bb745cc059e1bf2c01ffcaaf569f9ee10a1b9cb416`, Numen `fe62bed17258bda3234d0008394dbd0c90faaada8f4e88a1ec82b551e5cf0df5`. These are not asserted as the final rerun's PID, instance or hashes.

#### Local configuration, backups and remaining manual step

The old Numen catalog entry and local dotfiles were manually migrated; AeroSpace's on-disk Wisp mode has `n`. Kitsune prefix Space explicitly preserves its CLI picker via `wisp menu`, unlike generated Wisp bindings whose Space stays in mode. The offline dotfiles contract test and `aerospace reload-config --dry-run --no-gui` passed.

**Historical status, now superseded:** the migration initially deferred reload because unrelated music-workspace edits would also apply. The main agent subsequently performed the authorized actual AeroSpace reload: Palette key **n** is active, main-mode **alt-c** code-workspace binding is removed, and **alt-shift-c** move-to-code is preserved. Dotfiles `wisp-catalog` tests passed with that contract. **No further AeroSpace reload is needed.** Physical Option+Space → N, Option+C delivery, attachment and focus remain manual. This documentation edit reloads nothing. Rollback text: `/var/folders/4x/vb2wqbds6_x1sh38zt3l17vc0000gq/T/opencode/numen-option-c-aerospace-rollback.txt`; review the whole configuration and release Numen's C registration before restoring the old binding.

Full-text snapshots (not metadata-preserving backups) are `/var/folders/4x/vb2wqbds6_x1sh38zt3l17vc0000gq/T/opencode/wisp-palette-before-{actions,aerospace,kitsune}.toml`, where braces name the three files. Prior installer `.numen-backup` / `.numen-displaced` artifacts remain untouched; do not confuse those with these text snapshots or discard their possible late edits.

**Historical old Summon UI-to-wire evidence only — NOT proof of new native exec.** Private fixture `/var/folders/4x/vb2wqbds6_x1sh38zt3l17vc0000gq/T/opencode/wh-FbeDK3/evidence.json` records the then-installed `~/.local/bin/wisp`: revision `ef423b4c3327a5d90289d6311adcb652e09d6345`, `vcs.modified=true`, **not** checkout HEAD `844cd1af5a51609f2db9d720c02b19cb3718d6c2`. It exercised the old outer CLI palette and real fzf, then a recorded fallback Kitty substitute executing the **original `/bin/sh -lc` wrapper**, real Numen menu and real fzf under a private PTY against a **mock private Numen UDS**. The fixture used checkout `target/debug/numen`, not installed Numen. In current terminology that outer CLI interface is `wisp menu`, not native Palette.

- Cancel exited 0 with discovery only and no mutations. Open and repeat Open each sent exactly `{"version":1,"type":"familiar.open","items":[{"preset":"djinn"}],"service_instance":"fixed"}` with **no target**, reported `Accepted open: fixture-id`, and exited 0.
- Metadata survived: `WISP_ID=numen`, `WISP_ROLE=summon`, title `wisp:numen: Numen`, layout `floating`. All three stdio streams were TTYs at 100×38; quoted/Unicode argv survived the wrapper.
- The owned socket was mode 0600 in an owned 0700 parent; it was removed and child processes reaped. No fake fzf, global input, real Kitty/WM, user-app/service controls, preference changes or home catalog writes were used. There was **no graphical rendering**: desktop focus/reuse and actual Wisp palette → Kitty acceptance remain manual.

Historically, Wisp's opt-in `cmd/wisp/numen_integration_test.go` passed with the then-installed Numen for the old no-key Summon argv, temporary install/idempotence and cwd/path handling; the Go suite also passed then. That old expectation is superseded by the current `exec`/`n`/three-argv test above. It skips unless `NUMEN_TEST_BINARY` names an absolute executable; see [Wisp testing](../wisp/README.md#numen-integration-test).

Historical path-guard checks: **415 passed (362 unit + 7 build-fingerprint + 46 CLI)**, one visual-preview test ignored; formatting and strict offline locked all-target Clippy passed with scoped SDK 26.5. Unit tests reject dollar-containing executable/socket paths; CLI coverage verifies rejection before lookup/writes. The `$` guard is retained in the migration. These checks were not rerun by this docs-only edit.

Historically, the path-guard build was installed at `/Users/jdawson/.local/bin/numen` using the scoped SDK 26.5 debug Cargo workflow. Installed preview and the then-current Wisp contract test passed; no running service was restarted. That installation is not evidence that the new migration is installed.

**Historical home observation:** earlier read-only inspection found no Numen entry in `/Users/jdawson/.config/wisp/actions.toml` and changed nothing then. This is superseded by the current manually migrated on-disk entry above, not a statement about current machine state. All older no-home-edit claims below apply only to their original test runs.

### Validation boundaries (historical)

**Final saved-preferences main-agent confirmation:** 367 passed (323 unit + 7 build-fingerprint integration + 37 CLI), one existing visual-preview test ignored; formatting, strict all-target Clippy and offline locked tests/build green with the command-scoped SDK workaround. Five private Apple Metal GUI service cycles (`np-rFEeeD/evidence.json`, read-only `CGWindowBounds`) passed seeded style/native foot restore, durable style edits retaining placement, source-instance restart/explicit Open with no auto-views, durable Forget/current view unchanged, next-restart fallback/no Open write, corrupt-file preservation/runtime edit and full opt-out. Every service quit 0 and removed its socket. See [exact observed values](README.md#saved-preferences-validation-status).

Placement was preseeded; **no physical OS drag-save end-to-end verification**, OS input or hotkey registrations occurred in this smoke. Pure tests cover drag capture/delivery/polling, stale bindings and queue-full handling; physical drag saving, mixed-DPI restoration and focus remain manual. Neither default user service nor home preferences were modified. Current headless/old-constructor diagnostics explicitly report disabled preferences without I/O; an older binary's missing field remains unknown. Publication-race fixes preserve changed backups through private-quarantine/inode verification, no-clobber prepublication rollback, revision-targeted flush and sticky capacity warnings; not atomic CAS, arbitrary malicious same-UID protection or indefinite old-FD retention. Prior USER-REPORTED working desktop workflow and historical results below remain separate. **Latest-build installation confirmed:** main-agent `cargo install --path . --debug --locked --offline --root ~/.local --force` succeeded with scoped SDK 26.5; installed `numen preferences forget --help` / `numen run --help` verified new command/flags. Only the installed binary/Cargo installation metadata changed in the user-authorized debug workflow; no running user service was restarted or home preference files touched. This documentation edit executes no commands, installation, restarts or commits and changes no Rust/home/Wisp configuration.

Confirmed after the latest poll/click-race fix with the command-scoped macOS 26.5 SDK workaround: **278 passed (257 unit + 21 CLI), one existing visual-preview test ignored**. `cargo fmt --check`, `cargo test --offline --locked`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo build --offline --locked` and `git diff --check` all passed; the updated Numen binary was built. These are reported main-agent results, not checks rerun by this documentation edit.

An independent temporary AppKit/winit window used the production `context_menu.rs` popup adapter on the main thread for three successful popup/autocancellations (**380/378/353 ms**), using `Arc<AtomicBool>` and the native 25 ms event-tracking `NSTimer`. It exited 0 without errors. This validates native popup construction/placement API use, timer cancellation and repeated tracking cleanup only, not physical click/drag/item selection or Wisp palette handoff. No synthetic OS keys/mouse, hotkeys or existing-user-app controls were used. Actual hands-on Close versus Quit, Esc, focus, mixed-DPI and physical dragging remain manual. No app restart or home configuration change occurred; prior source-handoff/settings/Option+M evidence below remains separate. This documentation edit ran no commands/tests or app controls and made no hooks/commits.

Final Option+M validation: the full Rust suite passed **255 tests (234 unit + 21 CLI)**, with one existing visual-preview test ignored; formatting, strict all-target Clippy and build passed. A bounded private zero-view GUI harness verified actual Carbon registration: M only with `--no-bubble`, both I/M normally, neither with `--no-bubble --no-hotkey`, then M again in a following fresh process. All four exited code 0 and removed their own sockets. Collision behavior was tested deterministically, not with real OS collisions; both keys registered successfully in the normal native case. No synthetic OS keys, actual key presses, input taps, permission requests or existing-user-service control/restarts occurred. This proves registration and normal-exit cleanup, not background delivery or hotkey-driven motion. Actual Option+M OS-chord/background-focus acceptance remains manual; historical CLI-driven native motion evidence is separate. These reported checks were not rerun by this documentation edit. This is not Wisp palette → Kitty acceptance, and the documented best-effort native-unregister failure limitation remains.

Prior Numen validation: **224 tests passed (206 unit, 18 CLI)**, plus formatting, strict clippy, debug build and diff checks; not rerun for this documentation/menu-text update. Real fzf PTY tests auto-started an empty native GUI from Preset Open, registered a diagnostic target, opened one familiar without Main, exited the menu while the detached service stayed alive, and multi-selected two familiars for Info with source JSON unchanged. An existing-empty-GUI cancellation test covered target/command/familiar Escape with no mutation and retained selection. After generation guarding was added, the real fzf/bootstrap/Open/detachment/graceful-quit path passed again. Unit tests also reject stale scoped requests against reused IDs after restart. Graceful quit ACK/socket removal and native zero-window quit passed.

Separately reported Djinn adapter checks cover headless real-wire outcomes/reconnects, UI lifecycle tests and package typechecks. A real interactive idle-chat smoke on 2026-10-28 also passed: source Djinn TUI → real fzf Open/target picker → native Metal familiar with title/idle state; source exit retained the unavailable exact pin/summary, last Close retained the service, and quit removed the socket. Its private fixture had zero conversation messages; no model turn, installed-binary or home/Wisp configuration change occurred. This does not validate live outcome animations or the Wisp launcher.

Runtime-settings validation passed 246 automated tests (225 unit + 21 CLI; one existing visual-preview test ignored), formatting, strict Clippy and build checks. A real fzf Settings flow edited a native view on a private Apple M2 Pro/Metal service; independent resize, hidden-anchor restoration, future-only duration, atomic batches and unchanged future-Open defaults passed, including paused views. Test processes cleaned up their sockets; the user's running service and home/Wisp configuration were untouched. This is not Wisp palette → Kitty acceptance. Hands-on mixed-DPI, composition and focus checks remain manual.

Only temporary test catalogs were installed; actual installed `wisp actions list` parsed the Numen row. **No actual home Wisp config was edited, no real Kitty launcher/WM-prefix handoff was tested, and actual Wisp palette → Kitty handoff remains manual.** Scripted inputs were PTY terminal inputs, not global OS key/pointer injection. Earlier positive native GPU Go to Mouse evidence remains unchanged.

## Related files

- [`src/preferences.rs`](src/preferences.rs): saved appearance/manual placement, distinct from the Wisp catalog installer.
- [README](README.md), [Architecture](ARCHITECTURE.md), [Protocol](PROTOCOL.md), [Modes](MODES.md), [Roadmap](ROADMAP.md).
- [`src/context_menu.rs`](src/context_menu.rs): native GUI menu and typed actions, separate from the fzf/Wisp surface.
- [`src/menu.rs`](src/menu.rs): picker, catalog, capability intersection and target batches.
- [`src/bootstrap.rs`](src/bootstrap.rs): discovery and detached empty startup.
- [`src/wisp_action.rs`](src/wisp_action.rs): TOML generation, explicit install, backups and no-clobber recovery.
- [`src/main.rs`](src/main.rs), [`src/ipc.rs`](src/ipc.rs), [`src/protocol.rs`](src/protocol.rs), [`src/activity.rs`](src/activity.rs), [`src/window.rs`](src/window.rs): CLI, wire/store and root GUI lifecycle.
