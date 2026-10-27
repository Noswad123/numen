# Numen architecture

## Overview

Delivered: a Rust/macOS-first desktop familiar with embedded Djinn animations, local Unix-socket activity service, diagnostic publisher, headless listener, explicit conversation selection, and GUI live input. One visible familiar represents one focused activity, not aggregate concurrent work. There is no automatic Djinn application adapter yet. The [initial local v1 protocol](PROTOCOL.md) is implemented, but is not a stable public SDK. Linux/compositor behavior and manual desktop acceptance remain unvalidated.

Sources report semantic activity, not artwork or executable scripts. The process keeps transport, activity reduction, personality mapping, and rendering separate. Live state is ephemeral; no durable history, broker, or preference store is required or implemented.

## API / Interfaces

| Module | Responsibility |
| --- | --- |
| `src/main.rs` | clap commands `run`, `listen`, `publish`, `status`, `focus`, `check-assets`, `font-license`; validation, startup, diagnostic publisher timing and headless output. |
| `src/protocol.rs` | v1 request parsing, activity/state types, identifiers and payload limits. |
| `src/ipc.rs` | Private socket paths, inode-safe ownership, bounded synchronous `Publisher`, nonblocking `Server` worker and ACKs. `Server::start` callbacks run on the worker and must return promptly; drop stops and joins it. |
| `src/activity.rs` | Per-source `Store`, sequences/tombstones, snapshots, liveness, exact `FocusTarget`, paged `Status`, and focused `Presentation { base, cue, following? }` with retained summary context. |
| `src/display.rs` | One-slot `Mailbox` and `LiveDisplay`: semantic expression mapping, terminal-base-to-idle mapping, cue deduplication and manual override. |
| `src/window.rs` | winit loop, IPC wake events, input/dragging, title/log context, pixels surface, demo and bounded exit. |
| `src/animation.rs` | Nine `Animation` expressions and timed `Player`; expression vocabulary is separate from wire states. |
| `src/assets.rs` | Generic `SpriteAtlas` validation and cached RGBA frames; default character constants and `Clip` durations/loop policy. `frame(animation, index)` requires a valid clip frame index. |
| `src/bubble.rs` | `text`, `dimensions`, and `Renderer::compose`: physical-pixel CPU RGBA composition, focused identity/summary/status, Preview labels, measured text and sprite placement. |
| `src/typography.rs` | `Typeface::load(family, file)`, `bundled`, `measure`, `line_metrics`, `kern`, and cached `glyph`: exact installed Regular selection via `fontdb`, single-face rasterization via `fontdue`, embedded fallback and license. |

Rust minimum is 1.89, edition 2024; rendering uses winit 0.30.13, pixels 0.17.2 and image 0.25.8. Socket handling uses standard Unix facilities and a worker thread, not Tokio. See [README](README.md#api--interfaces) for command flags and controls.

## Data Flow

```text
diagnostic publisher / future adapters
  -> newline-delimited JSON over private Unix socket
  -> request validation -> connection-scoped source/activity store
  -> exact focused activity base + optional focused transition cue
     -> latest-only mailbox -> winit wake -> LiveDisplay
         -> Player + cached embedded atlas + CPU bubble composition -> pixels -> compositor
     -> headless latest-only mailbox -> detached JSON stdout writer
```

`run` starts the service by default. `--manual` and `--demo` disable socket creation. `listen` uses the same service without a window or asset decoding. `publish` also bypasses asset decoding: it registers, sends state changes, holds each with one-second heartbeats, then removes its activity and disconnects. It is an interactive diagnostic that returns errors, not automatic best-effort integration.

### Service and reduction

The worker polls every 25 ms, with one bounded read and at most eight messages per client per tick. Nonblocking I/O and bounded input/output buffers prevent one peer from blocking the service. Connection capacity includes unregistered peers. Registration, EOF drain, and liveness deadlines bound incomplete clients; details are in [Protocol](PROTOCOL.md).

The store tracks all producers but selects exactly one `(source, instance, activity)`. With no pin yet, the first available activity is chosen; records in an initial snapshot use lexical order. Once chosen, the pin is retained even when unavailable through removal, disconnect or expiry. No background fallback or OS-focus inference occurs. A reconnect with a new instance requires explicit selection. `focus` requires exactly one live match; `next` cycles lexical live records, including terminals, wrapping to the first (also choosing the first when the current pin is unavailable).

`Presentation.base` is only that focused record, including terminal identity; it is absent when the pin is unavailable. Only fresh focused success/failure transitions can create 1.5-second cues. Repeated terminal states do not replay; active failure cues outrank success cues, and focused attention drops cues rather than deferring them. Registration, snapshots and explicit selection never replay historical outcomes. Background updates remain queryable without affecting presentation. Idle and disconnect never imply completion.

`companion.status`, `companion.focus` and `companion.next` can use an unregistered control connection or a registered producer connection, without refreshing producer liveness. Status pages contain four selections, including optional summaries, to keep worst-case ACKs below 8 KiB. There are at most 16 registered sources and 24 total clients (`MAX_SOURCES + 8`), including unregistered/control clients. The GUI uses `FocusControl`: a bounded 32-entry channel with `try_send`, drained at most eight times per worker tick; it never waits for socket I/O on the event thread.

### Live display and output

The GUI mailbox holds one pending presentation and requests one wake until consumed; later updates replace the slot. `LiveDisplay` maps external waiting to `Review`, attention waiting to `Waiting`, work to `Working`, and fresh success/failure cues to one-shot art. Terminal bases map to idle without losing selected identity. After a one-shot it uses only the same focused activity's current base: ordinarily idle, or working if that conversation has already resumed. It never restores other background work. The default on-canvas bubble shows identity, summary and current base status independently of cues; IDs also appear in logs and the window title.

Optional `Presentation.following` contains `{source, instance, activity, summary?, available}`. The store caches only the selected summary and retains it with identity through removal, snapshot omission, disconnect and expiry, even when both base and cue are absent. Selecting another activity replaces the cache, including with no summary; background updates cannot alter it. A present replacement record with omitted/null summary clears the old value. The default unfocused presentation omits `following`. GUI metadata updates always request repaint, including while paused or on the same animation, without resetting the player or requesting focus.

Keyboard 1–9 and right-click enter manual expression override while live views continue updating. L resumes the selected conversation's latest base without changing conversation or replaying an old cue. Tab queues a next-conversation request and resumes live control. Neither enables IPC in a session started with `--manual` or `--demo`. CLI focus changes the service pin but does not clear a GUI expression override. Pause is preserved across expression changes, but selecting an expression resets to its first frame even while paused.

Headless output is presentation changes (`base`, `cue`, and optional `following`), not an audit of every request. A latest-only slot and detached stdout writer isolate unread stdout from the service and exit deadline. Slow consumers may miss intermediate views; the latest pending value is retained. The writer is deliberately not joined: process shutdown may truncate its last diagnostic line. This does not make all CLI I/O nonblocking: publisher stderr logging can still block.

### Rendering

The default logical canvas is 384 × 352: an ivory rounded thought bubble above the unchanged 192 × 208 sprite at `(96, 144)`. `run --no-bubble` restores a 192 × 208 sprite-only canvas. `--scale` sizes the logical window, but CPU RGBA composition targets its actual physical dimensions. The pixels framebuffer and surface match those dimensions for 1:1 GPU presentation, not full-canvas nearest upscaling. `Resized` updates both buffer and surface; `ScaleFactorChanged` requests redraw, which reconciles physical dimensions. The scene is centered with aspect-preserving scale; only sprite artwork is nearest-resampled at that scale, including fractional scales.

`Renderer` rasterizes text at `font_size × physical scene scale` (default 16 logical units, CLI range finite 8–32). `fontdue` supplies real advances, pair kerning, line/baseline metrics and grayscale glyph coverage blended onto ivory. Identity/status clipping and summary wrapping use measured width, with U+2026 `…` truncation. Available vertical space and line height permit up to three summary lines; a larger font can reduce the line count without changing sprite dimensions. The unfocused identity reads “No conversation selected”, not the app/character name. `fontdb` and `fontdue` replace the removed `font8x8` bitmap dependency.

At bubble-enabled GUI initialization, `Typeface::load` prefers installed **SauceCodePro Nerd Font Regular** using `fontdb` system/user-directory discovery. The preference matches the owner's Kitty configuration, but runtime never parses Kitty or copies its font size, nor hardcodes a machine font path. Failure to load the default preference selects embedded OFL **Source Code Pro Regular**, a base font without Nerd icons. `--font-family NAME` instead requires an exact Normal weight/style/stretch family face; `--font-file PATH` directly loads TTF/OTF face 0 (TTC index 0). These options conflict, and explicit missing/invalid selections error rather than falling back. `--no-bubble` bypasses discovery/loading entirely, even for an unusable family/file; CLI validation still applies. Headless/control commands, `check-assets`, and `font-license` never initialize fonts. `font-license` prints the full embedded SIL OFL 1.1 text.

Unicode/Nerd icons depend on selected-face coverage. Missing glyphs resolve within that face to U+FFFD, then `?`, then `.notdef`; no full script shaping, color emoji or fallback-font chain is implemented. Selected font data is capped at 32 MiB, including a bounded file read; `fontdb` system metadata scanning is outside this cap. The glyph cache holds at most 512 entries at one exact physical pixel size and clears when that size changes (including DPI changes). See [font assets](assets/fonts/README.md) for provenance.

The bubble reads current base status, not cue state or the player: Idle, Working, Needs your input, Waiting on dependency, Completed, Failed, Cancelled, or Unavailable. Thus Completed remains correct after a one-shot settles to idle, with no historical replay. Missing summary falls back to activity ID; no focus shows Choose a conversation / Not following. Manual/demo overrides explicitly say Preview (Animation preview when unfocused).

The binary embeds `assets/djinn/` PNG and manifest, validates metadata and image dimensions, and caches RGBA frames through `SpriteAtlas`. The temporary global default has ID `djinn` and name Djinn: all tools currently use this fallback. Producer source IDs are not character IDs. A different global default and an explicit application-source `djinn` → character `djinn` mapping are planned, not implemented. The binary never reads the local, gitignored `djinn-artifacts.zip`; its original generation metadata remains unchanged as provenance. Validation rejects missing/duplicate clips, duplicate rows, invalid dimensions, frame counts, timing or loop policies, and malformed/mismatched PNG data.

Frame deadlines use `WaitUntil`, not continuous GUI polling. Paused, occluded or zero-sized windows do not advance animation; bounded exit remains scheduled. Success, failure and waving art play once then settle to idle; only the focused activity's current base can resume afterward. Advancement carries leftover elapsed time and remains bounded by clip length after long stalls. A separate selection cursor preserves demo/right-click order after one-shots.

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
./target/debug/numen focus --source djinn --activity chatid --instance producer-1
./target/debug/numen focus --next
# Alternatives to the graphical listener:
cargo run --locked -- listen --exit-after 5
cargo run --locked -- run --manual --animation review --paused --exit-after 5
cargo run --locked -- check-assets
```

Use only one listener per socket. Global `--socket PATH` can appear before or after the subcommand; see [Protocol](PROTOCOL.md) for defaults and wire examples.

### Validation and delivery

Verified locally: 71 unit and 7 CLI integration tests pass (78 automated tests), plus formatting, strict clippy, debug build and diff checks. Portable font tests use bundled Source Code Pro. The separately run visual test used the installed preferred face for 1×/2× previews inspected as smooth. Metal startup selected installed SauceCodePro Nerd Font Regular at 1× and 2× display densities. A three-second paused Retina live check used the matching 1152 × 1056 physical framebuffer and verified summary updates without animation restart, CLI selection, unavailable context, no summary logging, and cleanup. License output, an invalid explicit font-file error and `--no-bubble` bypass were checked. Actual mixed-monitor dragging and all focus/compositor behavior still require the [manual checklist](README.md#validation).

Preserve the command-only [macOS 26.5 SDK workaround](README.md#local-macos-sdk-workaround) for the local macOS 27.0 SDK linker issue; do not put its machine-specific path into build configuration or change system settings.

Next: implement a Djinn adapter, then an unrelated build/test runner to prove reuse. Future automatic adapters must bound and isolate publishing from source execution, recover via authoritative snapshots, and tolerate an absent/slow companion. The diagnostic publisher is not that adapter. Exact activity pinning is implemented; [other modes](MODES.md), app-signaled focus following, character mapping/preferences, installation/autostart, packaging, Linux validation, and manual desktop acceptance remain future work. No multiple views, gaze or desktop roaming is implemented. Multiple future views can share one service; one process per pet is not a requirement.

Numen's contract remains independent of Djinn internals. Wisp is a separate floating utility surface, not an implemented renderer integration or evidence of graphical transparency support.

## Related files

- [README](README.md): commands, SDK workaround and acceptance checklist.
- [Protocol](PROTOCOL.md): complete initial local v1 wire contract and lifecycle.
- [Modes](MODES.md): focused definition, mode choices and recommendations.
- [`src/main.rs`](src/main.rs), [`src/ipc.rs`](src/ipc.rs), [`src/protocol.rs`](src/protocol.rs), [`src/activity.rs`](src/activity.rs).
- [`src/display.rs`](src/display.rs), [`src/window.rs`](src/window.rs), [`src/animation.rs`](src/animation.rs), [`src/assets.rs`](src/assets.rs).
- [`src/bubble.rs`](src/bubble.rs): CPU thought-bubble composition and visual-preview test.
- [`src/typography.rs`](src/typography.rs), [font assets README](assets/fonts/README.md): installed/bundled selection, metrics, rasterization/cache bounds and provenance.
- [`tests/cli.rs`](tests/cli.rs), [`Cargo.toml`](Cargo.toml), [`assets/djinn/manifest.json`](assets/djinn/manifest.json).
