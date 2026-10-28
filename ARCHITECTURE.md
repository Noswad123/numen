# Numen architecture

## Overview

Delivered: the Rust/macOS-first multi-familiar foundation, Go to mouse, familiar-first fzf menu, opt-in Wisp action installer, menu Open empty bootstrap and graceful graphical quit. Each view represents one exact activity, not aggregate work. Native lifecycle/navigation and real fzf PTY smoke tests passed; actual Wisp palette → Kitty handoff remains manual. No automatic Djinn adapter exists. The additive [local v1 protocol](PROTOCOL.md) is not a stable public SDK; other platforms/compositors and further hands-on desktop acceptance remain unvalidated.

Sources report semantic activity, not artwork or executable scripts. The process keeps transport, activity reduction, personality mapping, and rendering separate. Live state is ephemeral; no durable history, broker, or preference store is required or implemented.

## API / Interfaces

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | CLI/startup, diagnostic publisher/headless output, discovery (`presets`, `familiars`, `targets`), scoped controls and Open/Attach/Close including file/stdin batches. |
| `src/menu.rs` | Familiar-first fzf selection, shared offline preset catalog, capability intersection, fresh per-selection targets and one atomic lifecycle request. |
| `src/bootstrap.rs` | Read-only `probe` / `ServiceInfo`; menu-only `ensure_graphical`, detached current-executable `run --empty`, private log and bounded readiness. |
| `src/wisp_action.rs` | Print-only TOML by default; opt-in append installer with durable snapshot/live-inode backups and no-clobber publication/recovery. |
| `src/protocol.rs` | v1 request parsing, activity/state types, identifiers and payload limits. |
| `src/ipc.rs` | Private sockets, inode-safe ownership, bounded `Publisher`, nonblocking worker/ACKs. `Server::start` serves headless presentations; `start_graphical` supplies complete familiar snapshots, targeted reveal, optional navigation and graceful quit, with optional Main bootstrap. Callbacks must return promptly; drop stops/joins the worker. |
| `src/activity.rs` | Canonical source/activity `Store`, sequences/tombstones/liveness, plus per-familiar exact bindings, cues and retained summary caches; atomic lifecycle mutation and paged discovery. `FamiliarPresentation { id, preset, presentation }` wraps `Presentation { base, cue, following? }`. |
| `src/display.rs` | One-slot `Mailbox` and `LiveDisplay`: semantic expression mapping, terminal-base-to-idle mapping, cue deduplication and manual override. |
| `src/window.rs` | Outer `App` reconciles complete registry snapshots, routes native window IDs and owns one hotkey/global exit deadline; each `FamiliarView` owns native/GPU resources, input, pause, player, visibility and geometry. |
| `src/motion.rs` | `Navigator::step_raster` returns `Move`, `Arrived` or `Blocked` from cached physical sprite alpha; `MotionClock` hard-gates sampling, and `NativeMotion` accumulates fractional intent. Source-mask stepping remains test-only. |
| `src/pointer.rs` | `PointerReader::new/position`, currently macOS only: fresh read-only `CGEvent` snapshots using a retained `CombinedSessionState` source. |
| `src/visibility.rs` | `BubbleVisibility`: initially hidden, selected-text change detection, explicit reveal, hide deadline and 750 ms automatic-notification quiet time. |
| `src/hotkey.rs` | `GlobalRevealHotkey`: main-thread macOS Carbon Option+I registration via `global-hotkey` 0.9, ID filtering, press/release latch and owned cleanup. |
| `src/animation.rs` | Nine `Animation` expressions and timed `Player`; expression vocabulary is separate from wire states. |
| `src/assets.rs` | Generic `SpriteAtlas` validation and cached RGBA frames; default character constants and `Clip` durations/loop policy. `frame(animation, index)` requires a valid clip frame index. |
| `src/bubble.rs` | `text`, `dimensions(show, sprite_scale)`, and `Renderer::compose`: physical-pixel CPU RGBA composition, focused identity/summary/status, Preview labels, measured text and independently scaled sprite placement. |
| `src/typography.rs` | `Typeface::load(family, file)`, `bundled`, `measure`, `line_metrics`, `kern`, and cached `glyph`: exact installed Regular selection via `fontdb`, single-face rasterization via `fontdue`, embedded fallback and license. |

Rust minimum is 1.89, edition 2024; rendering uses winit 0.30.13, pixels 0.17.2 and image 0.25.8. Socket handling uses standard Unix facilities and a worker thread, not Tokio. See [README](README.md#api--interfaces) for command flags and controls.

## Data Flow

```text
diagnostic publisher / future adapters
  -> newline-delimited JSON over private Unix socket
  -> request validation -> connection-scoped source/activity store
  -> per-familiar exclusive binding + base/cue + summary cache
     -> complete Vec<FamiliarPresentation> -> latest-only mailbox -> winit wake
         -> App reconciliation -> independent FamiliarViews -> pixels surfaces -> compositor
     -> legacy main presentation -> headless latest-only mailbox -> detached JSON stdout writer

Option+I Carbon callback / companion.reveal worker callback
  -> RevealNotifier / bounded RevealMailbox -> coalesced winit RevealBubbles
  -> App::apply_reveals -> selected views' visibility deadlines + native geometry
```

`run` starts the service by default. `--manual` and `--demo` disable socket creation. `listen` uses the same service without a window or asset decoding. `publish` also bypasses asset decoding: it registers, sends state changes, holds each with one-second heartbeats, then removes its activity and disconnects. It is an interactive diagnostic that returns errors, not automatic best-effort integration.

### Service and reduction

The worker polls every 25 ms, with one bounded read and at most eight messages per client per tick. Nonblocking I/O and bounded input/output buffers prevent one peer from blocking the service. Connection capacity includes unregistered peers. Registration, EOF drain, and liveness deadlines bound incomplete clients; details are in [Protocol](PROTOCOL.md).

The store tracks all producers once and holds up to 16 familiar bindings. Ownership is exclusive by exact `(source, instance, activity)`, including disconnected waiting pins. Only bootstrap `main` auto-chooses the first unclaimed live activity once, lexically among initial snapshot records. Pins persist through removal/disconnect/expiry without fallback or OS-focus inference. Focus requires one live match not owned by another familiar; next cycles eligible lexical live records, including terminals, with wraparound. Unavailable pins start at the first eligible record. A new producer instance requires explicit selection.

`main` remains the legacy default scope, not an alias for whichever view remains. Closing it does not recreate it: legacy status reports null focus, explicit unknown IDs error, and focus requires a valid existing ID. Generated `familiar-N` IDs are lifetime-monotonic without generated-history growth. Closed IDs cannot be reused; explicit history is bounded to 256 plus reserved `main` (257 total). No persistent profiles exist; restart resets IDs.

Open/Attach resolve and revalidate each flattened target before any batch mutation. Duplicate IDs/targets, unknown presets, unavailable or ambiguous targets reject the whole batch. Attach disallows taking another familiar's target even if that owner is also moving in the batch (no swaps). Close validates every ID before removing any. Sources are never executed or stopped. Only preset `djinn` exists. Direct CLI/wire Open needs a running graphical service; only menu Open can bootstrap it. Headless discovery is supported but lifecycle is not.

### Menu, bootstrap and quit

Wisp's existing generic launcher needs no Wisp-source changes: an opt-in `kind = "summon"` catalog entry runs absolute argv `[executable, "--socket", resolved_absolute_socket, "menu"]`. Leading `--socket` avoids Wisp's work-directory heuristic; no Numen PATH install is required. The entry has no key and is selected through `wisp palette`, not an automatically installed WM-prefix binding. See [Wisp](WISP.md) for installation and two-backup recovery details.

`menu` requires stdin/stderr TTYs. Sanitized labels are display-only; typed `familiar:` / `preset:` IDs drive selection. fzf options/command environment overrides are removed; output is capped at 64 KiB and unoffered/duplicate identities are rejected. The menu intersects capabilities across the whole selection, preserves selection on command Back, discards all pending targets on target Back, and exits after one accepted command. Each target picker reads fresh four-row pages, filters other owners and locally reserved earlier choices, then submits one batch for server revalidation. Discovery alone performs no startup or filesystem writes.

Only choosing Open offline calls `ensure_graphical` (unless `--no-start`). It reuses compatible GUI services, otherwise launches the current executable with `--socket PATH run --empty`, null stdin/stdout, private 0600 `service.log` stderr and `setsid`. Readiness is bounded to five seconds and means control readiness, not GPU initialization. The UDS bind selects a concurrent-start winner; callers reuse it. Startup failure kills/reaps only the child handle created by that invocation, never a metadata PID or another listener. Headless/outdated/unsafe existing services are errors, never replacement permission. Stop an old foreground service manually or choose another socket.

`Server::start_graphical(path, on_views, on_reveal, optional_on_go_to_mouse, on_quit, bootstrap_main)` is the current GUI API. Old constructors remain test compatibility helpers. `bootstrap_main = false` selects `Store::empty()`: no `main` is created even as sources arrive. `run --empty` conflicts with manual/demo. `companion.service` returns API version 1, PID, unique per-listener `service_instance` and graphical/lifecycle/navigation booleans without source changes or liveness refresh; it has no shutdown flag. The menu pins that instance and includes it on every RPC. The worker rejects mismatched optional root preconditions before event dispatch, callback or liveness mutation; old one-shot clients/producers may omit it. A restarted service cannot inherit a stale menu selection merely by reusing familiar IDs. The generation marker is not an authentication secret.

`companion.quit` requires the graphical shutdown callback. The worker schedules it after flushing the ACK and client EOF (or 500 ms after flush), ignoring later pipelined input. A coalesced `AppEvent::QuitService` exits the root event loop even at zero windows; hotkey/server drop releases registration, worker and owned socket, and all views are dropped. ACK `quitting: true` means scheduled, not completed. No source app is stopped and no reported PID is killed. Per-view Close still retains the service after last-close; cancelled menu Open may leave an empty resident service for explicit `quit`.

`Presentation.base` is only that focused record, including terminal identity; it is absent when the pin is unavailable. Only fresh focused success/failure transitions can create 1.5-second cues. Repeated terminal states do not replay; active failure cues outrank success cues, and focused attention drops cues rather than deferring them. Registration, snapshots and explicit selection never replay historical outcomes. Background updates remain queryable without affecting presentation. Idle and disconnect never imply completion.

Controls work without registration and do not refresh producer liveness. Status/focus/next accept optional `familiar`; reveal accepts optional `familiars`. `familiar.list` and `companion.targets` provide four-record pages; GUI capabilities are `info`, `attach`, `close`, plus `go-to-mouse` only with a navigation callback, while headless/legacy rows advertise `[]`. Targets are connected protocol-advertised activities with `claimed_by`/`eligible`, not OS scans or an actual Djinn adapter. See [Protocol](PROTOCOL.md) for response shapes. All prior 8 KiB framing, 16-source/24-client and transport bounds remain. GUI `FamiliarControl::next(id)` and `close(id)` use a nonblocking 32-entry channel drained at most eight times per worker tick. ACK confirms acceptance, not rendering; async native/GPU creation can fail after an Open ACK.

### Live display and output

The GUI mailbox holds one pending complete familiar snapshot and requests one wake until consumed; later snapshots replace the slot, including lifecycle removals. Each view's `LiveDisplay` maps external waiting to `Review`, attention waiting to `Waiting`, work to `Working`, and fresh success/failure cues to one-shot art. Terminal bases map to idle without losing identity. After a one-shot it uses only that same activity's current base: ordinarily idle, or working if already resumed. The bubble shows identity, summary and base status independently of cues; IDs also appear in logs and window titles.

`BubbleVisibility::observe` compares `bubble::text(view, None)`, independently of animation/Preview state. First selection and changed focused identity reveal; an initial empty presentation does not. Selected status/summary changes notify, but heartbeat, background, reason-only and cue-expiry changes do not. Automatic changes while visible never extend the deadline. Expiry starts 750 ms quiet time anchored to that deadline; suppressed text is cached, not deferred. Explicit reveal/new focus bypass quiet time and refresh the full duration. Default duration is five seconds (`--bubble-duration`, finite 0.25–30). `--always-bubble` is persistent/debug opt-in and conflicts with `--no-bubble`; the latter overrides every reveal.

Global registration occurs on the main thread after winit event-loop creation and is skipped with `--no-hotkey` or `--no-bubble`. The callback posts through `RevealNotifier` into the bounded mutex-protected `RevealMailbox`, waking the GUI when the queue was empty. Only a matching Pressed event reveals, deduplicated until matching Released. Carbon `RegisterEventHotKey` is used for KeyI, not EventTap, all-keystroke capture or media keys. Errors warn and preserve fallback. Drop unregisters and detaches the weak callback target, releasing its proxy; the crate's OnceCell retains only a capture-free dispatcher.

The outer App applies global Option+I and no-argument `bubbles` to all actual current views. Targeted IPC reveal preserves its subset, never accidentally becoming broadcast: `RevealMailbox` holds at most 32 requests × 16 IDs; an explicit all request subsumes pending subsets. The global hotkey is registered once per process, including while no live views remain. Cross-process broadcast is not implemented; use `--socket PATH` to choose a service. The chord may replace a macOS dead-accent shortcut; `--no-hotkey` retains focused local fallback (which reveals the focused view).

Optional `Presentation.following` contains `{source, instance, activity, summary?, available}`. The store caches only the selected summary and retains it with identity through removal, snapshot omission, disconnect and expiry, even when both base and cue are absent. Selecting another activity replaces the cache, including with no summary; background updates cannot alter it. A present replacement record with omitted/null summary clears the old value. The default unfocused presentation omits `following`. GUI metadata updates always request repaint, including while paused or on the same animation, without resetting the player or requesting focus.

Keyboard 1–9 and right-click enter manual expression override while live views continue updating. L resumes the selected conversation's latest base without changing conversation or replaying an old cue. Tab queues a next-conversation request and resumes live control. Neither enables IPC in a session started with `--manual` or `--demo`. CLI focus changes the service pin but does not clear a GUI expression override. Pause is preserved across expression changes, but selecting an expression resets to its first frame even while paused.

1–9/right-click reveal that view after selecting Preview; L and successfully queued Tab also reveal it. Demo cycling does not auto-reveal. Notifications/reveals never request focus. Only explicit left-click or initial `main` with startup `--focus` may request it; later Open never does.

Q/Esc/native close removes only the addressed live familiar through `FamiliarControl`. Closing the last view leaves the service/hotkey alive at zero windows for later Open. Ctrl-C quits the service. Manual/demo remain socket-free single-view sessions whose close exits the process. Native creation/render/resize failures discard only the failed live view; pending registry closes retry on queue saturation and accepted closes await snapshot removal without resubmission. Failed IDs stay suppressed until removal is acknowledged. Shared font initialization remains fatal as before.

Headless output is presentation changes (`base`, `cue`, and optional `following`), not an audit of every request. A latest-only slot and detached stdout writer isolate unread stdout from the service and exit deadline. Slow consumers may miss intermediate views; the latest pending value is retained. The writer is deliberately not joined: process shutdown may truncate its last diagnostic line. This does not make all CLI I/O nonblocking: publisher stderr logging can still block.

### Navigation and coordinate contract

`Server::start_graphical` installs its optional navigation callback only after successful macOS `PointerReader` initialization, advertising `go-to-mouse` alongside GUI `info`, `attach`, `close`. The test compatibility constructor `start_with_navigation` also installs navigation; old `start_with_familiars` retains lifecycle/reveal but rejects navigation. Headless/legacy handlers reject it and advertise no GUI capabilities. Core store capabilities stay static; the server adds navigation only when the callback exists, never based on source capability claims. Worker callbacks enqueue bounded UI work, not OS operations.

`familiar.go-to-mouse {ids}` → validation of all explicit IDs → callback → bounded target-only mailbox (32 requests × 16 IDs) → coalesced winit wake → newest complete registry applied first → selected views' fresh `Navigator`s. Pending target sets coalesce; there is no implicit all-motion request. Closing/failed views are skipped. ACK `moving` records acceptance, not arrival, and no source/binding/liveness mutation occurs.

Physics uses global desktop **logical points**, with Quartz's main-display top-left origin and negative coordinates allowed. `pointer.rs` documents the pairing: winit's global physical `outer_position()` is divided by that window's scale factor, then movement uses `set_outer_position(LogicalPosition)`. There is no extra Y flip or mixed-DPI cursor rescaling. `sprite_geometry` derives source-pixel mapping from actual `inner_size`, centered letterboxing, bubble offset and sprite scale, then divides by backing scale. Coordinate/letterbox tests in `window.rs` are evidence of this mapping, not actual monitor-crossing acceptance.

While actively seeking, hard root/per-view `MotionClock` gates bound pointer reads and movement to roughly 60 Hz; early GUI events cannot accelerate physics or advance a seeking pose. `--move-speed` is finite 40–2000, default 360 desktop points/second. The engine clamps elapsed time to 50 ms, aims at a visible raster anchor, retains horizontal heading for vertical travel, and sweeps at most 4096 samples toward first contact. `SpriteProjection` is shared with composition: cached `RasterMask` alpha uses the same physical bounds and pixel-center source samples. Only actual painted sprite alpha ≥32 counts, not invisible downsampled source features or bubble/window padding. Pose selection/advance precedes planning; the pose is held until fresh native feedback can confirm contact. Fractional intent accumulates in `NativeMotion` instead of per-event native nudges. Actual-position stalling and stable-target non-convergence terminate with a warning, not success.

Seeking owns running art; confirmed arrival owns one `Succeeded` jump, then restores the latest attached base emote. Source metadata continues updating the bubble; expired source cues are consumed rather than replayed afterward. Arrival is local, never agent success. No ongoing follow remains; a reissued command restarts intent. Movement clears the saved hidden anchor, committing the new location rather than snapping back on bubble expiry. Multiple views may overlap at the pointer; no placement/collision policy is added.

Native left-drag, digits, right-click cycle, L, Tab and close cancel without success. Reveal-all/Option+I does not. Space pauses motion and animation; requests while paused wait for resume, while bubble deadlines continue. Occlusion alone does not suspend pursuit or celebration, allowing multiple familiars to converge and complete behind one another; ordinary background rendering may still skip occluded surfaces. Invalid/zero-sized native geometry suspends motion until restored. Pointer snapshots do not warp/post input, install event taps, request permissions or scan OS processes. Non-macOS navigation is not currently supported.

### Scene composition

The native window starts at a 96 × 104 logical sprite-only canvas, expands while revealed to 384 × 248 (384 × 144 bubble above the character at `(144, 144)`), and shrinks on hide; no full hidden input area remains. Native atlas cells remain 192 × 208. `--sprite-scale` is finite 0.25–2, default 0.5, affecting artwork only. `dimensions(show, sprite_scale)` rounds sprite bounds upward; shown width is 384, height 144 plus sprite height, otherwise sprite bounds only. `--sprite-scale 1` gives 192 × 208 hidden or 384 × 352 shown. Expansion preserves the sprite anchor when possible and clamps to current monitor bounds near edges. Collapse restores the saved hidden position; an explicit left-drag clears that anchor and commits the current location. This avoids mistaking delayed programmatic window-manager moves for user dragging. Platforms without positioning support are best-effort.

`--scale` remains finite 0.5–4, default 1.5, sizing the entire logical scene including the font. Default requested window size is therefore 144 × 156 hidden or 576 × 372 revealed in logical window units. CPU composition targets actual physical dimensions; the pixels framebuffer and surface match them for 1:1 GPU presentation, not full-canvas nearest upscaling. `Resized` updates both buffer and surface; `ScaleFactorChanged` requests redraw, which reconciles physical dimensions. The centered, aspect-preserving physical scene scale is `min(physical_width / logical_width, physical_height / logical_height)`. Sprite artwork alone is nearest-resampled using pixel centers at scene scale times sprite scale. A half-size character at physical scene scale 2 reproduces the native 192 × 208 pixels.

Low-level `Renderer::new(typeface, font_size)` retains sprite scale 1 for native-unit composition and asset tests. GUI initialization applies the CLI option with `.with_sprite_scale(options.sprite_scale)` and uses the same option for `dimensions` and window sizing. Low-level sprite scales are clamped to 0.25–2 (nonfinite values use 0.5); CLI values outside the finite range are rejected.

`Renderer` rasterizes text at `font_size × physical scene scale` (default 12 logical units, CLI range finite 8–32), independently of sprite scale. `fontdue` supplies real advances, pair kerning, line/baseline metrics and grayscale glyph coverage blended onto ivory. Identity/status clipping and summary wrapping use measured width, with U+2026 `…` truncation. Available vertical space and line height permit up to three summary lines; a larger font can reduce the line count without changing sprite dimensions. The unfocused identity reads “No conversation selected”, not the app/character name. `fontdb` and `fontdue` replace the removed `font8x8` bitmap dependency.

At bubble-enabled GUI initialization, `Typeface::load` prefers installed **SauceCodePro Nerd Font Regular** using `fontdb` system/user-directory discovery. The preference matches the owner's Kitty configuration, but runtime never parses Kitty or copies its font size, nor hardcodes a machine font path. Failure to load the default preference selects embedded OFL **Source Code Pro Regular**, a base font without Nerd icons. `--font-family NAME` instead requires an exact Normal weight/style/stretch family face; `--font-file PATH` directly loads TTF/OTF face 0 (TTC index 0). These options conflict, and explicit missing/invalid selections error rather than falling back. `--no-bubble` bypasses discovery/loading entirely, even for an unusable family/file; CLI validation still applies. Headless/control commands, `check-assets`, and `font-license` never initialize fonts. `font-license` prints the full embedded SIL OFL 1.1 text.

Fonts load/parse once for bubble capability, even while hidden. Views share the parsed `Arc<Font>` via `Typeface::fork`, but each has an independent 512-entry glyph cache at one physical pixel size, cleared on size/DPI change. The atlas is shared as `Arc<SpriteAtlas>`; Window/Pixels, Player, pause, visibility and geometry are private per view. Startup options are inherited, not independently configurable per view. Unicode/Nerd icons depend on the face; missing glyphs use U+FFFD, then `?`, then `.notdef`. No full shaping, color emoji or fallback chain exists. Font bytes are capped at 32 MiB; system metadata scanning is outside that cap. See [font assets](assets/fonts/README.md).

The bubble reads current base status, not cue state or the player: Idle, Working, Needs your input, Waiting on dependency, Completed, Failed, Cancelled, or Unavailable. Thus Completed remains correct after a one-shot settles to idle, with no historical replay. Missing summary falls back to activity ID; no focus shows Choose a conversation / Not following. Manual/demo overrides explicitly say Preview (Animation preview when unfocused).

The binary embeds `assets/djinn/` PNG and manifest, validates metadata and image dimensions, and caches RGBA frames through `SpriteAtlas`. The temporary global default has ID `djinn` and name Djinn: all tools currently use this fallback. Producer source IDs are not character IDs. A different global default and an explicit application-source `djinn` → character `djinn` mapping are planned, not implemented. The binary never reads the local, gitignored `djinn-artifacts.zip`; its original generation metadata remains unchanged as provenance. Validation rejects missing/duplicate clips, duplicate rows, invalid dimensions, frame counts, timing or loop policies, and malformed/mismatched PNG data.

Frame deadlines use `WaitUntil`, not continuous idle polling. The next wake merges hide, exit, frame, demo and active-motion deadlines. Explicit pause freezes movement and animation, while hide/exit remain scheduled; occluded navigation/celebration can continue independently of drawing. Seeking poses advance only on gated motion ticks. Quiet-period completion needs no wake. Success, failure and waving art play once then settle to idle; only the focused activity's current base resumes afterward. A selection cursor preserves demo/right-click order after one-shots.

## Edge Cases

- Transparent rendering uses `PostMultiplied` presentation with `REPLACE` blending to preserve straight-alpha bytes. `PixelPerfect` presents the physical-size framebuffer 1:1; only the sprite is nearest-resampled during CPU composition. Aspect-ratio differences may leave padding. Opaque diagnostics blend over black.
- Startup focus is opt-in. macOS accessory activation avoids a Dock icon; left-click explicitly requests focus and native drag. Timers and live expression changes never request focus. Transparency, stacking and dragging still depend on compositor support.
- Socket parent must be user-owned/private (use 0700), not a symlink. Only the leaf directory is created, never missing ancestors or chmod of an existing directory. Socket mode is 0600. Active collisions and unsafe paths refuse startup; only an owned, unchanged socket with a refused connection is stale-replaced. Drop unlinks only the owned inode. Abrupt termination may leave a recoverable stale socket.
- Treat received strings as untrusted data, never code or asset paths. Default payloads carry identifiers/state rather than prompts, tool output or credentials. Model memory, execution policy and authorization remain outside Numen.
- Summaries are explicit producer metadata, not generated text: Numen makes no model calls, reads no transcripts and automatically copies no raw prompts. Even a short summary may be sensitive on screen. `--no-bubble` hides only the bubble, not wire/state or headless/status output. A future Djinn adapter is expected to reuse existing title/short-intent metadata, not add summarization calls; it is not implemented.

## Example usage

```sh
cargo run --locked -- run
# Separate terminal:
cargo run --locked -- publish --state working --source build --summary 'Investigating retry backoff' --duration 5
cargo run --locked -- publish --state waiting --attention --reason permission_required
cargo run --locked -- publish --demo
# While producers are connected:
./target/debug/numen status
./target/debug/numen bubbles # Timed reveal, no selection/focus change
./target/debug/numen focus --source djinn --activity chatid --instance producer-1
./target/debug/numen focus --next
./target/debug/numen familiars --offset 0
./target/debug/numen targets --familiar main --offset 0
# Separate eligible connected target required; graphical service only:
./target/debug/numen open --source build --instance worker-2 --activity task --id build-pet
./target/debug/numen bubbles --familiar build-pet
./target/debug/numen go-to-mouse --familiar main build-pet
./target/debug/numen close --familiar build-pet
# Alternatives to the graphical listener:
cargo run --locked -- listen --exit-after 5
cargo run --locked -- run --manual --animation review --paused --always-bubble --exit-after 5
cargo run --locked -- check-assets
```

Use only one listener per socket. Global `--socket PATH` can appear before or after the subcommand; see [Protocol](PROTOCOL.md) for defaults and wire examples.

### Validation and delivery

Verified: 206 unit and 18 CLI integration tests pass (224 total), plus formatting, strict clippy, debug build and diff checks. Real fzf PTY tests verified empty native GUI bootstrap, attached Open without Main, detached service survival, multi-familiar Info with unchanged source JSON, cancellation/selection retention and graceful quit/socket cleanup. The scoped bootstrap/Open path was rerun after generation guarding, and reused-ID stale-request rejection has dedicated tests. A temporary catalog parsed through installed `wisp actions list`; no home catalog was edited. Prior native lifecycle/navigation evidence remains valid. Scripted PTY inputs are terminal-only, not global OS key/pointer injection. Actual Wisp palette → Kitty handoff and the remaining [manual checklist](README.md#validation) are unverified.

Preserve the command-only [macOS 26.5 SDK workaround](README.md#local-macos-sdk-workaround) for the local macOS 27.0 SDK linker issue; do not put its machine-specific path into build configuration or change system settings.

Next: a Djinn adapter, then an unrelated runner. Future adapters must isolate publishing from source execution, recover via snapshots and tolerate absence/slowness. The diagnostic publisher is not that adapter. [Other modes](MODES.md), app-signaled following, different per-view characters/options, persistent preferences, general installation/autostart, packaging, Linux validation, gaze and roaming remain future work.

Numen's contract remains independent of Djinn internals. Wisp is a separate generic launcher surface; the Numen menu adapter does not integrate renderers or prove graphical transparency support.

## Related files

- [README](README.md): commands, SDK workaround and acceptance checklist.
- [Wisp](WISP.md): palette/menu adapter, opt-in installer and recovery; [`src/menu.rs`](src/menu.rs), [`src/bootstrap.rs`](src/bootstrap.rs), [`src/wisp_action.rs`](src/wisp_action.rs).
- [Protocol](PROTOCOL.md): complete initial local v1 wire contract and lifecycle.
- [Modes](MODES.md): focused definition, mode choices and recommendations.
- [`src/main.rs`](src/main.rs), [`src/ipc.rs`](src/ipc.rs), [`src/protocol.rs`](src/protocol.rs), [`src/activity.rs`](src/activity.rs).
- [`src/display.rs`](src/display.rs), [`src/window.rs`](src/window.rs), [`src/animation.rs`](src/animation.rs), [`src/assets.rs`](src/assets.rs).
- [`src/motion.rs`](src/motion.rs), [`src/pointer.rs`](src/pointer.rs): pure pursuit and macOS coordinate/snapshot contract; `window.rs` supplies actual geometry and position confirmation.
- [`src/bubble.rs`](src/bubble.rs): CPU thought-bubble composition and visual-preview test.
- [`src/visibility.rs`](src/visibility.rs), [`src/hotkey.rs`](src/hotkey.rs): transient policy/timers and global shortcut ownership/latch.
- [`src/typography.rs`](src/typography.rs), [font assets README](assets/fonts/README.md): installed/bundled selection, metrics, rasterization/cache bounds and provenance.
- [`tests/cli.rs`](tests/cli.rs), [`Cargo.toml`](Cargo.toml), [`assets/djinn/manifest.json`](assets/djinn/manifest.json).
