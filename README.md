# Numen

An independent, event-driven familiar that gives visible expression to what your tools are doing.

**Status:** multi-familiar foundation, macOS Go to mouse, familiar-first fzf menu, opt-in Wisp action installer, menu Open empty-service bootstrap, and graceful graphical quit are delivered. Only the Djinn preset is available; targets come from connected protocol sources, not automatic application/session adapters. No actual home Wisp catalog was edited. Wisp palette → Kitty handoff and further hands-on desktop acceptance remain manual checks. See [Wisp setup and limitations](WISP.md).

Numen means divine power or presence: an unseen, animating presence in the machine. The intended CLI spelling is `numen`; command and package availability have not yet been checked.

## Overview

Inspired by the pet feature in the Codex app, but generalized beyond a single application: the companion is its own process. Tools publish semantic activity through the initial local v1 protocol. In the implemented **shared-focused** mode, one visible familiar represents **one selected conversation (activity)**, never an aggregate of every conversation. Background activities remain tracked and queryable but cannot steal its expression or completion cues. The included publisher is an interactive diagnostic, not a stable public SDK or automatic integration.

The source reports **what is happening**; the companion chooses **how to express it**.

Each bubble starts **hidden**, with independent status/summary notification timers (five seconds by default). On macOS, **Option+I (Alt+I)** is registered once per process and reveals all enabled current views without taking focus. `numen bubbles` is the socket alternative; `bubbles --familiar main familiar-1` targets only that subset. Cross-independent-process broadcast is not implemented.

For example, a source reports `waiting` with the reason `permission_required`, not `play-sad-genie-animation`. Different personalities and rendering surfaces can express the same activity differently.

## Product principles

- Independent of Djinn; Djinn is the first integration, not the owner.
- Local-first, lightweight, and optional.
- Useful ambient awareness without becoming another notification feed.
- No model calls or tokens required for basic behavior.
- Future automatic adapters must not impede source execution when the companion is absent or slow; the diagnostic publisher reports errors instead.
- Personality and artwork are separate from the activity contract.
- Observe logical activities, not just operating-system PIDs.

## Example usage

Requires Rust 1.89 or newer (edition 2024) and a graphical desktop for `run`. Rendering uses winit 0.30.13, pixels 0.17.2, and image 0.25.8.

```sh
cargo run --locked -- check-assets
cargo run --locked -- run
cargo run --locked -- run --bubble-duration 3 # Timed notifications and reveals
cargo run --locked -- run --no-hotkey # No global registration; focused local Option+I remains
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

### Local macOS SDK workaround

The selected `MacOSX27.0.sdk` currently causes a linker failure reporting unknown architecture `arm64e.x1`. This command-scoped workaround has been verified locally:

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo build --locked
```

Use the same `SDKROOT=...` prefix for `cargo test`, `cargo clippy`, or `cargo run` on this machine when needed. This is a local SDK issue, not a portable prerequisite: do not hardcode this path in repository build configuration or change system settings.

## API / Interfaces

`check-assets` validates the embedded atlas without opening a window. `run` starts with the undecorated, draggable `main` view and **starts the socket listener by default**; IPC can open additional views. `listen` runs the activity service without a GUI or asset decoding and prints legacy `main` presentation changes, not a raw audit. It supports discovery but not graphical lifecycle commands. `publish` also needs no GUI or asset decoding.

`font-license` prints the full embedded SIL Open Font License 1.1 text for the bundled fallback. `listen`, `publish`, `status`, `focus`, `bubbles`, `check-assets`, and `font-license` do not initialize fonts.

### Menu, startup and shutdown

`menu [--no-start] [--fzf PATH]` requires stdin and stderr TTYs and installed fzf (already installed on the validation machine). Use Wisp **run/summon**, not exec. It lists clearly labeled running/waiting familiars and presets, with distinct `familiar:` / `preset:` identities. Tab multi-selects up to 16; Enter advances to commands. Command Escape/Back retains the familiar selection; target Escape/Back discards the **entire** pending batch. Ctrl-R refreshes the current stage. One accepted action exits the menu so the Wisp Kitty floater can close.

Commands are the capability intersection of **all** selected rows: presets support only Open; active familiars support Info, Go to Mouse, Attach and Close as advertised. A mixed selection may have no common command: Back is offered with an explanation, never silent partial execution. Open/Attach fetch fresh target pages separately for each selection, excluding other owners and earlier pending choices while allowing one's own current attachment. Empty lists offer Refresh/Back and explain that no application adapter exists. Backend mutation revalidates the whole batch; retryable errors appear in the menu header. With only one Djinn preset, opening more copies requires repeated invocations; a batch applies once to each actual selection.

Merely entering the menu or discovering sources never spawns a service or writes files. Choosing Open while offline starts the current executable as detached `run --empty`, unless `--no-start` requests a visible error instead. It creates no unattached `main`; source updates alone keep the registry empty. Startup uses a private 0600 `service.log` beside the socket and a five-second control-readiness wait, not GPU readiness. Concurrent starts reuse the winning graphical listener. Existing headless, outdated or unsafe services are never replaced or killed: stop an old foreground service manually once or choose another `--socket`. **Direct CLI `open` still requires an already-running graphical service.**

`service-info` queries `{api_version, pid, service_instance, graphical, lifecycle, navigation}`. The menu pins `service_instance` and seals each RPC with it: a restart rejects stale selections before dispatch, rather than acting on reused familiar IDs. Close/reopen the menu after a service restart. `quit` sends `companion.quit`: `{"quitting":true}` means scheduled, not completed. Current graphical services exit their event loop, release all views/hotkey and clean up their owned socket; headless/legacy handlers without a shutdown callback reject it. No source applications are terminated and no reported PID is signaled. Closing the last view retains the service; `quit` stops it globally. Cancelling Open after bootstrap may leave an empty resident service: use `quit` when desired.

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
| `--no-bubble` | Hard privacy disable: ignores every reveal, stays sprite-only and skips fonts/global hotkey; does not redact wire/state metadata. |
| `--always-bubble` | Opt-in persistent bubble for debugging/previews; conflicts with `--no-bubble`. |
| `--bubble-duration SECONDS` | Finite 0.25–30, default 5; lifetime of notifications and explicit reveals. |
| `--no-hotkey` | Disables global Option+I registration, not the local shortcut while the pet has keyboard focus. |
| `--font-family NAME` | Explicit installed family, exact Normal weight/style/stretch (Regular); missing/invalid selection errors rather than falling back. Conflicts with `--font-file`. |
| `--font-file PATH` | Explicit TTF/OTF override; uses face 0 (also index 0 for a TTC collection). Missing/invalid files error rather than falling back. |
| `--font-size NUMBER` | Finite 8–32, default 12 logical canvas units; scales with `--scale` and display density, independently of `--sprite-scale`. |
| `--demo` | Disables the socket and cycles all nine expressions, three seconds per selection. |
| `--paused` | Starts with animation and motion paused; navigation requests wait for resume. Conflicts with `--demo`. Bubble deadlines still run. |
| `--opaque` | Black diagnostic background instead of transparency. |
| `--normal-window-level` | Disables the default always-on-top request. |
| `--focus` | Opts into requesting keyboard focus at startup. |
| `--exit-after SECONDS` | Finite 0.1–86400; closes automatically for bounded smoke tests. |

With keyboard focus, **Tab** queues next-conversation for that familiar and resumes live control. **1–9/right-click** enter manual expression override; **L** resumes its latest live base without switching or replaying cues. **Space** toggles only that view's pause flag; expression changes preserve pause but may change the first frame. **Q/Esc/native close** closes only that live view. Closing the last view leaves IPC and the global hotkey alive with zero windows; `open` can create another later. `--manual`/`--demo` remain single-view, socket-free sessions: Q/Esc/native close exits the process, and Tab/L cannot enable IPC. Left-click requests focus and native dragging. **Ctrl-C** quits the service, including from a focused view or the launching terminal.

**1–9/right-click** also reveal the Preview bubble; **Tab/L** reveal when resuming live control. A registered global handler owns Option+I even when the pet is focused, preventing duplicate local delivery. If global registration is disabled/unavailable, focused local **Option+I** is the fallback. Demo cycling does not automatically reopen the bubble; use `--always-bubble` for persistent previews.

The macOS global shortcut uses Carbon registration via `global-hotkey` 0.9, not an EventTap or all-keystroke capture. Registration failure warns but does not stop the app. Another Numen process may own the chord: use `bubbles --socket PATH` to target the desired service. Option+I is reserved while registered and may override a macOS dead-accent shortcut; opt out with `--no-hotkey`. This non-media Carbon key should not require Accessibility permission, but actual background-focus chord acceptance remains pending; no synthetic OS keyboard input was used.

`bubbles` sends v1 `companion.reveal` without producer registration and prints status from the ACK. Acceptance does not guarantee a displayed frame (notably with `--no-bubble`). It changes neither selection nor producer state/liveness and never requests focus. Headless `listen` rejects it with `no graphical familiar`.

`status [--offset N] [--familiar ID]` prints mode, exact focus, availability and four activities per page, including summaries. `focus [--familiar ID] --source SOURCE --activity ACTIVITY [--instance INSTANCE]` requires exactly one live match not owned by another familiar. `focus [--familiar ID] --next` cycles eligible live records, including terminals, in lexical `(source, instance, activity)` order with wraparound. These controls work with `run` or `listen` without registration. Omitted `--familiar` always means `main`; explicit unknown IDs error. After `main` closes, legacy status has null focus without recreating it; focus requires an existing familiar. CLI selection does not cancel GUI expression override; use L or Tab.

### Familiar discovery and lifecycle

One service supports **16 familiars**. Bindings are exclusive exact `(source, instance, activity)` tuples, including disconnected waiting bindings. Preset IDs and active familiar IDs are distinct.

| Command | Contract |
| --- | --- |
| `presets` | Offline JSON: `{"presets":[{"id":"djinn","name":"Djinn","capabilities":["open"]}]}`. |
| `familiars --offset N` | Four records/page: `id`, `preset`, `attachment`, `available`, optional `summary`, `capabilities`. GUI capabilities: `info`, `attach`, `close`, plus `go-to-mouse` only with an installed navigation callback; headless/legacy handlers: `[]`. |
| `targets --offset N [--familiar ID]` | Four connected protocol-advertised activities/page with `claimed_by` and `eligible`; an owner may reselect its own target. The menu filters eligible entries; no OS scan or actual Djinn adapter. |
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

Supply metadata with `publish --summary 'Investigating retry backoff'`. Summaries must be nonblank, at most 160 UTF-8 bytes, and contain no control characters or bidi embedding/isolate controls. The CLI trims surrounding whitespace and repeats the summary on every state, including `publish --demo`. Numen makes no LLM calls, reads no transcripts, and automatically copies no raw prompts. A future Djinn adapter is expected to reuse an existing title/short-intent field; it is not implemented.

**Privacy:** even a short explicit summary may be sensitive and visible to nearby people or screen sharing. `run --no-bubble` hides only the on-screen bubble; summaries remain in protocol/state and diagnostic output. Producers must choose safe metadata.

Publishers → Unix socket → canonical source/activity store → per-familiar binding, cue and summary cache → complete latest-only familiar snapshots → independent expression controllers, players and pixels surfaces. External waiting maps to `review`; attention waiting maps to `waiting`. Terminal records retain identity but rest after fresh success/failure one-shots unless that same activity has resumed work. Background work is never restored into another familiar. The outer App routes native window IDs to views; source/activity IDs appear in logs and window titles.

Graphical startup validates the embedded `assets/djinn/` PNG/manifest through the generic `SpriteAtlas` and caches RGBA frames. The temporary global default is character ID `djinn`, display name **Djinn**, so all tools currently render Djinn as a fallback. Source names are producer identities, not character identities. The future plan is a different global default plus an explicit application-source `djinn` → character `djinn` mapping; that mapping is not implemented. No external extraction is needed. The local, gitignored `djinn-artifacts.zip` retains its original generation metadata unchanged as provenance. See [Architecture](ARCHITECTURE.md), [Protocol](PROTOCOL.md), and [Modes](MODES.md).

## Edge Cases

- Go to mouse uses hard deadline gates to sample the live pointer and step each active pursuer at most roughly 60 Hz; unrelated redraws/source events cannot accelerate it. Contact uses cached physical sprite alpha from the renderer's exact pixel-center sampler, excluding discarded source pixels, bubbles, transparent holes and padding. The planned pose is held for actual native-position feedback before arrival. It plays one `succeeded` jump, then resumes the latest attached emotes, with no ongoing follow. Reissuing restarts intent. Multiple selected views may overlap at the pointer; there is no smart placement.
- Navigation owns the visual pose while source changes continue updating status/summary bubbles. Expired source cues are not replayed afterward. Local arrival never publishes agent success or changes bindings/source liveness. Native left-drag, digits, right-click cycle, L, Tab and close cancel without success; Option+I/reveal-all does not cancel. Space pauses both movement and animation, not bubble deadlines.
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
- Next: a Djinn adapter, then an unrelated runner. Other [mode choices](MODES.md), app-signaled following, different per-view characters/options, persistent preferences, general installation/autostart, packaging, gaze and roaming remain future work.

## Validation

Verified: **206 unit tests and 18 CLI integration tests pass (224 automated tests)**, plus formatting, strict clippy, debug build and diff checks. Real fzf PTY runs verified empty native startup from Open, source-target selection, attached familiar creation without Main, detached service survival, multi-familiar Info, cancellation/Back, and graceful quit with socket cleanup. The scoped Open/bootstrap path passed again after service-generation guarding; tests also reject stale commands against reused IDs after restart. Temporary action catalogs were parsed by installed Wisp; no home catalog was edited. Actual Wisp palette → Kitty handoff remains manual. Earlier native lifecycle/navigation and compositor regression evidence is preserved; font tests use the bundled fixture without OS fonts.

Real fzf PTY validation selected Preset Open, auto-started an empty native GUI, registered a diagnostic source, selected its target and opened exactly one familiar with **no Main**. The menu exited while the detached service remained alive. A second real fzf menu multi-selected two familiars for Info without changing source JSON. An existing-empty-GUI test exercised target Escape, command Escape and familiar Escape with no mutation and retained selection. Graceful quit ACKed and removed the socket; a separate native zero-window quit passed. The installer touched only temporary test catalogs; actual installed `wisp actions list` parsed the generated Numen row. No home Wisp configuration was edited, no real Kitty launcher/WM-prefix handoff was exercised, and no global OS keyboard/pointer input was injected: only scripted PTY terminal inputs. Actual `wisp palette` → Kitty handoff remains manual.

Actual dragging between monitors and all hands-on focus/compositor behavior remain unverified. At default scene scale 1.5, the requested logical window is 144 × 156 hidden and 576 × 372 revealed; physical dimensions additionally depend on display density and window-system rounding.

Tests cover CLI validation, atlas integrity, animation timing, protocol/store lifecycle, socket safety, display control, publisher roundtrips, EOF draining, and unread-stdout deadline behavior. Development checks include `cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings`, and bounded GPU smoke runs such as `cargo run --locked -- run --demo --exit-after 5` (apply the SDK prefix above when needed). GPU logs establish rendering activity, not desktop acceptance. Pending:

- [x] Native multi-view control/render smoke: three diagnostic sources/views, targeted/global reveal, independent lifecycle, last-close zero-window service and later Open.
- [x] Two-window native navigation smoke: live pointer reading, running art, confirmed arrival, success once, latest-source restoration and source-state preservation.
- [ ] Hands-on moving-pointer pursuit, monitor crossings (mixed DPI/negative coordinates), forced occlusion, edge-clamped/refused movement, cancellation, and pause/resume. Verify that pointer movement after arrival does not restart following.
- [ ] Hands-on per-window input/pause controls, native close gestures, injected native/GPU failure isolation, and focus preservation across multiple views.
- [ ] Inspect sprite alpha edges over light/dark backgrounds: no dark fringe or opaque rectangle; compare `--opaque`. Expect nearest-sampled sprite art but antialiased text.
- [ ] Inspect short/long summaries, measured wrapping and `…`, clipped identities, supported Unicode/Nerd icons and missing-glyph replacement at several scales/font sizes. Check reduced line counts at larger sizes, terminal status while resting, activity-ID fallback, and unfocused/Unavailable/Preview labels.
- [ ] Manually check explicit missing family/file errors and invalid-file errors (no silent fallback), plus automatic bundled fallback on a system without the preferred Nerd Font installed.
- [ ] Change only the selected summary while paused and while the animation is unchanged: text must update without restarting animation or stealing focus. Switch chats and disconnect: no other chat's summary may leak into the bubble.
- [ ] Compare `run --no-bubble`: only the default 96 × 104 logical sprite remains; `--sprite-scale 1` gives 192 × 208. Confirm metadata is still present in status/headless output; assess screen-sharing privacy with non-sensitive sample summaries.
- [ ] Click to focus; test 1–9, Space, right-click, Q/Esc, and native left-drag.
- [ ] Exercise two conversations: background attention/outcomes must not interrupt the selected conversation. Check Tab/CLI selection, ambiguous selection errors, retained unavailable focus after removal/disconnect/expiry, and terminal selection without historical cues. Test manual override and L/live resume without changing conversation, including while paused.
- [ ] Keep another app focused during startup without `--focus` and during demo transitions; verify it retains focus. Test explicit `--focus` separately.
- [ ] Drag between monitors with different display scales; inspect text clarity after rerasterization, nearest-scaled sprite art, padding, and clipping at several `--scale`/`--sprite-scale`/`--font-size` values. Changing only sprite scale must not resize bubble/text at the same physical scene scale. Synthetic 2× previews are not actual monitor-DPI acceptance.
- [ ] Check the default always-on-top request and compare `--normal-window-level` against ordinary windows.
- [ ] Press actual Option+I with another app focused: verify timed reveal/hide without activation, including paused/occluded windows. Check quiet time, explicit refresh, edge clamping/restoration and visible-window dragging. Registration or CLI smoke logs do not validate the OS chord. No synthetic OS input or Accessibility prompt was used.

## Related files

- [Roadmap](ROADMAP.md): delivered menu/bootstrap foundation and remaining adapters.
- [Wisp](WISP.md): opt-in palette setup, menu flow, installer safety and recovery.
- [Architecture](ARCHITECTURE.md): delivered service/renderer boundaries and next milestones.
- [Protocol](PROTOCOL.md): initial local v1 messages, limits, sequencing, and lifecycle.
- [Modes](MODES.md): per-familiar shared-focused semantics, multi-view bindings and future mode choices.
- [Naming](NAMING.md): selected name and rationale.
- [`src/main.rs`](src/main.rs): CLI and startup; [`src/window.rs`](src/window.rs): desktop events and rendering.
- [`src/motion.rs`](src/motion.rs), [`src/pointer.rs`](src/pointer.rs): pure bounded pursuit and read-only macOS pointer snapshots; coordinate contract in [Architecture](ARCHITECTURE.md#navigation-and-coordinate-contract).
- [`src/animation.rs`](src/animation.rs): expressions and player; [`src/assets.rs`](src/assets.rs): atlas validation and frame cache.
- [`src/bubble.rs`](src/bubble.rs): physical-pixel CPU composition, measured wrapping, status labels and preview generation.
- [`src/visibility.rs`](src/visibility.rs), [`src/hotkey.rs`](src/hotkey.rs): notification deadlines/quiet policy and owned macOS global reveal registration.
- [`src/typography.rs`](src/typography.rs), [font assets README](assets/fonts/README.md): font selection, metrics, raster/cache limits and fallback provenance/license.
- [`src/protocol.rs`](src/protocol.rs), [`src/activity.rs`](src/activity.rs), [`src/ipc.rs`](src/ipc.rs), [`src/display.rs`](src/display.rs): wire validation, reduction, transport, and live display.
