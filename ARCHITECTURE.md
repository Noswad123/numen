# Numen architecture

## Overview

The delivered implementation is a standalone Rust desktop rendering spike, macOS-first. The local event service described below is still a proposal, not a frozen API or implemented endpoint. Rust and the independent graphical desktop surface are selected; Linux desktop behavior remains unvalidated.

## Delivered rendering spike

### API / Interfaces

- `src/main.rs`: clap CLI (`run`, `check-assets`), option validation, logging, and atlas loading. See [README](README.md#api--interfaces) for flags and controls.
- `src/assets.rs`: `HexAtlas` validates the embedded PNG/manifest and caches frames; `Clip` holds frame durations and looping policy. `frame(animation, index)` requires a valid clip frame index.
- `src/animation.rs`: `Animation` defines nine expressions; `Player` selects, advances, and reports the current frame and next deadline. These expression names are not the proposed wire protocol vocabulary.
- `src/window.rs`: winit application/event loop, native input and dragging, pixels GPU surface, demo timing, and bounded exit.

The project uses Rust 1.89 minimum, edition 2024, winit 0.30.13, pixels 0.17.2, and image 0.25.8. There is no socket, source store, reducer, or preference persistence yet.

### Data Flow

```text
embedded PNG + manifest -> HexAtlas validation / cached RGBA frames
CLI + keyboard / mouse + demo timer -> Player selection and timing
Player frame -> pixels 192 × 208 buffer -> GPU surface -> desktop compositor
```

The atlas is embedded at compile time; runtime does not read the original local, gitignored ZIP. Validation rejects malformed metadata, missing or duplicate clips, duplicate rows, invalid dimensions/frame counts/timings/loop policies, and invalid or mismatched PNG data.

Frame deadlines drive `WaitUntil` rather than continuous polling. Paused, occluded, or zero-sized windows do not advance animation; bounded exit remains scheduled. One-shot success, failure, and waving clips return to idle, carrying leftover elapsed time forward. Advancement is bounded by clip length even after long stalls. A separate selection cursor keeps demo/right-click cycling in order after one-shots finish.

### Edge Cases: alpha, scale, and focus

The current transparent Metal path uses a `PostMultiplied` presentation surface with `REPLACE` blending to preserve straight-alpha PNG bytes. `ScalingMode::PixelPerfect` uses nearest sampling, avoiding filtered straight-alpha edge darkening. Sprite magnification snaps to physical integer multiples; fractional logical window sizes may leave padding. Opaque diagnostics instead use alpha blending over black with an opaque surface.

The window requests always-on-top unless disabled. macOS accessory activation policy avoids a Dock icon, and startup focus is opt-in. Left-click explicitly requests focus and native drag; animation selection and timers never request focus. This is not click-through, focus following, gaze, or desktop roaming. Platform/compositor support still needs manual verification.

### Example usage and validation

`cargo run --locked -- run --demo --exit-after 5` exercises a bounded GPU session; `cargo run --locked -- check-assets` validates assets without a window. Unit tests and clippy complement these checks, but GPU initialization and draw logs do not verify visual alpha, focus retention, or dragging. See the [manual acceptance checklist](README.md#validation).

The local selected macOS 27.0 SDK currently fails linking with unknown architecture `arm64e.x1`. The verified command-scoped macOS 26.5 SDK workaround is documented in [README](README.md#local-macos-sdk-workaround); do not bake a machine-specific SDK path into build configuration or change system settings.

### Related files

[`Cargo.toml`](Cargo.toml), [`src/main.rs`](src/main.rs), [`src/window.rs`](src/window.rs), [`src/animation.rs`](src/animation.rs), [`src/assets.rs`](src/assets.rs), and [`assets/hex/manifest.json`](assets/hex/manifest.json).

## Proposed event service (not implemented)

### Components and boundaries

```text
Djinn -------- adapter ---+
                         |
Build runner - adapter ---+--> Companion process
                         |      |-- Local event ingress
Test runner -- adapter ---+      |-- Per-source activity store
                                |-- Attention policy / state reducer
                                +-- Personality and renderer
```

Begin with one companion process. The renderer may run inside that process; a separate broker or renderer service is not required for the MVP. Preserve conceptual boundaries so rendering can evolve independently.

Adapters translate application-specific events into the shared contract. Sources never choose artwork, supply executable animation scripts, or depend on the companion for successful execution.

## Draft activity contract

Illustrative state update:

```json
{
  "version": 1,
  "type": "activity.state",
  "source": "djinn",
  "instance": "djinn-7f2a",
  "activity": "session-42",
  "sequence": 18,
  "state": "waiting",
  "reason": "permission_required"
}
```

- `source`: stable integration name, such as `djinn` or `build`.
- `instance`: unique producer lifetime; a restarted process gets a new identity.
- `activity`: logical work identity within the instance. A process can own multiple activities.
- `sequence`: monotonically increasing per activity within an instance; discard stale or duplicate updates.
- `state`: shared semantic vocabulary.
- `reason`: optional integration-specific detail, with graceful fallback for unknown reasons.

| State | Meaning |
| --- | --- |
| `idle` | No active work |
| `working` | Work underway |
| `waiting` | Blocked on input or an external dependency |
| `succeeded` | Known successful completion |
| `failed` | Known unsuccessful completion |
| `cancelled` | Intentionally stopped |

Idle is not success. Disconnection is not failure. An adapter must not invent a stronger outcome than its source exposes. User attention is a separate policy dimension: waiting on a network request should not look like waiting for permission.

Before implementation, specify registration, heartbeat, activity removal, snapshot, message-size limits, and error responses alongside this state update. Unsupported major versions must be rejected clearly. Unknown optional fields should not break compatible clients.

## Transport and source lifecycle

Proposed transport: newline-delimited JSON over a Unix-domain socket in a user-private runtime directory, starting on macOS. Keep the contract transport-independent; other platforms may require another transport. This socket does not exist in the rendering spike.

1. Source connects and registers its instance.
2. Source sends an authoritative snapshot of its current activities.
3. Source publishes state changes and periodic heartbeats.
4. Companion marks an instance disconnected on socket closure, or stale after a heartbeat timeout.
5. Reconnect uses a fresh authoritative snapshot to reconcile missing and obsolete activities.

Track connectivity separately from activity outcome. Old snapshots must not trigger repeated completion celebrations. Clean shutdown and expired instances should release their active attention indicators.

Publishing must be best effort and bounded. A slow or absent companion cannot block Djinn or a build. Coalesce replaceable state updates; recover current state through snapshots rather than maintaining an unbounded event queue. No durable event log is required initially.

## Multi-source attention policy

Do not use simple last-event-wins behavior. One pet may represent multiple simultaneous activities.

Proposed default:

1. Respect explicit source/activity selection.
2. Surface unresolved requests requiring user action within the selected scope.
3. Briefly acknowledge new failures and completions without losing pending attention.
4. Otherwise reflect ongoing work.
5. Rest when no tracked work remains.

Define deterministic tie-breaking, debounce rapid transitions, and limit transient animation frequency. Make the represented source identifiable. Do not infer application focus merely from the newest event; focus following requires an explicit signal or platform integration.

## Persistence, privacy, and security

- Persist user preferences, not animation ticks or indefinitely growing activity history.
- Treat live activity state as ephemeral and reconstruct it from producers.
- Restrict socket access to the current user; a local socket is not permission to expose a public network endpoint.
- Default payloads contain state and identifiers, not prompts, tool output, credentials, or full file paths.
- Validate payload sizes, values, and source/activity counts.
- Treat labels as untrusted data; never execute received strings or load arbitrary code/assets from messages.
- Keep model prompts, memory, execution policy, and tool authorization outside the companion.

## Djinn integration

Repository inspection identified an existing SolidJS/OpenTUI interface, plugin slots, UI preferences, and session activity events. Useful starting points relative to the Djinn repository:

- `clients/djinn-ui/packages/tui/src/feature-plugins/system/notifications.ts`: existing handling of status, permission, questions, and errors.
- `clients/djinn-ui/packages/tui/src/feature-plugins/builtins.ts`: possible first UI-side adapter registration.
- `clients/djinn-ui/packages/tui/src/context/data.tsx`: streaming event handling.
- `clients/djinn-ui/packages/tui/src/context/sync.tsx`: additional status synchronization.

Choose one authoritative event path for the first adapter; do not combine overlapping streams blindly. Distinguish foreground sessions from subagents and retain activity identities. A UI-side adapter only observes what the UI receives; use a native/runtime integration later if headless coverage is required.

The companion's own protocol remains independent of Djinn's internal event formats.

## Relationship to Wisp

`../wisp/README.md` describes Wisp as a generic floating utility surface for commands in the Jamal Arcana ecosystem. It is a separate existing project, not an available name for this app.

A terminal-based companion might eventually use Wisp as its host surface. This is an option to validate, not an assumed integration or evidence that Wisp already provides transparent graphical pet rendering.

## Delivery and validation

1. **Delivered:** Rust/macOS-first desktop rendering spike, embedded Hex atlas, nine expressions, local controls, and bounded GPU smoke support. Manual desktop acceptance remains pending; see README.
2. **Next:** finalize the smallest complete protocol; implement socket ingress, source lifecycle, state reducer, and a diagnostic producer.
3. **Then:** connect Djinn, followed by an unrelated build/test producer to prove reuse.
4. **Service validation:** reconnect snapshots, stale updates, crashed producers, multiple sources, and unavailable/slow companion behavior.
5. **Later:** preferences, installation/autostart and packaging, idle resource measurements, and accessibility/reduced-motion behavior beyond the existing pause control. Validate Linux separately.

No multi-pet support, focus following, gaze, or desktop roaming is implemented. Attention selection remains a design decision; keep it independent of renderer details. Command/package availability beyond the local binary still needs checking.
