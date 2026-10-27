# Numen

An independent, event-driven familiar that gives visible expression to what your tools are doing.

**Status:** standalone Rust desktop rendering spike implemented, with bundled Hex animations. macOS is the initial target; Linux is unvalidated, especially compositor-dependent behavior. The event-driven service and Djinn integration remain proposed.

Numen means divine power or presence: an unseen, animating presence in the machine. The intended CLI spelling is `numen`; command and package availability have not yet been checked.

## Overview

Inspired by the pet feature in the Codex app, but generalized beyond a single application: the companion is its own process. The goal is for Djinn, build runners, test runners, and other tools to publish semantic activity updates; the companion will interpret those updates and emote accordingly. Today, expressions are selected locally through CLI options, keyboard, or demo mode; there is no publishing API yet.

The source reports **what is happening**; the companion chooses **how to express it**.

For example, a source reports `waiting` with the reason `permission_required`, not `play-sad-genie-animation`. Different personalities and rendering surfaces can express the same activity differently.

## Product principles

- Independent of Djinn; Djinn is the first integration, not the owner.
- Local-first, lightweight, and optional.
- Useful ambient awareness without becoming another notification feed.
- No model calls or tokens required for basic behavior.
- No impact on source execution when the companion is absent or slow.
- Personality and artwork are separate from the activity contract.
- Observe logical activities, not just operating-system PIDs.

## Example usage

Requires Rust 1.89 or newer (edition 2024) and a graphical desktop for `run`. Rendering uses winit 0.30.13, pixels 0.17.2, and image 0.25.8.

```sh
cargo run --locked -- check-assets
cargo run --locked -- run
cargo run --locked -- run --demo
cargo run --locked -- run --animation waving --exit-after 5
# After a debug build:
./target/debug/numen run
```

### Local macOS SDK workaround

The selected `MacOSX27.0.sdk` currently causes a linker failure reporting unknown architecture `arm64e.x1`. This command-scoped workaround has been verified locally:

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo build --locked
```

Use the same `SDKROOT=...` prefix for `cargo test`, `cargo clippy`, or `cargo run` on this machine when needed. This is a local SDK issue, not a portable prerequisite: do not hardcode this path in repository build configuration or change system settings.

## API / Interfaces

`check-assets` validates the embedded atlas and prints clip information without opening a window. `run` opens one undecorated, draggable window.

| `run` option | Behavior |
| --- | --- |
| `--animation NAME` | Initial expression; default `idle`. Names, in keyboard order: `idle`, `working`, `waiting`, `succeeded`, `failed`, `review`, `waving`, `running-left`, `running-right`. |
| `--scale NUMBER` | Finite 0.5–4, default 1.5; sizes the window relative to a 192 × 208 logical-pixel cell. |
| `--demo` | Cycles all nine expressions, three seconds per selection. |
| `--paused` | Starts with the current frame frozen; conflicts with `--demo`. |
| `--opaque` | Black diagnostic background instead of transparency. |
| `--normal-window-level` | Disables the default always-on-top request. |
| `--focus` | Opts into requesting keyboard focus at startup. |
| `--exit-after SECONDS` | Finite 0.1–86400; closes automatically for bounded smoke tests. |

While focused, **1–9** select expressions in the order above, **Space** toggles pause (also suspending demo cycling), and **Q/Esc** quits. Left-click requests focus and starts native dragging; right-click cycles expressions. **Ctrl-C** in the launching terminal is a fallback quit path.

## Data Flow

The binary embeds `assets/hex/spritesheet.png` and its manifest, validates and caches RGBA frames, then advances a timed animation player and presents its selected frame. No external asset extraction is needed; the original ZIP remains local and gitignored. See [Architecture](ARCHITECTURE.md) for module boundaries and the proposed event service.

## Edge Cases

- `succeeded`, `failed`, and `waving` play once, then return to `idle`; other clips loop. Running expressions animate in place, not across the desktop.
- PixelPerfect nearest sampling snaps sprite scaling to physical integer multiples, not filtered scaling. Fractional window sizes can leave padding; `--scale` is not a promise of fractional sprite magnification.
- Startup leaves focus alone unless `--focus` is supplied; animation changes never request focus. macOS uses accessory activation policy, with no Dock icon.
- Always-on-top, transparency, and native drag depend on desktop support. GPU logs do not prove correct visual composition or focus behavior.
- No socket, persistence, installation, autostart, focus following, multiple pets, gaze, or desktop roaming yet. Next: socket plus diagnostic producer, then Djinn integration.

## Validation

Verified locally on Apple M2 Pro / Metal: debug build, formatting, strict clippy, and all **13 unit tests** pass using the SDK workaround below the usage examples. A 28-second transparent demo visited all nine expressions and exited cleanly; an opaque, paused smoke test also exited cleanly. These checks do not replace the desktop acceptance steps below.

Unit tests cover CLI validation, atlas integrity, animation timing, and expression selection. Development checks include `cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings`, and bounded GPU smoke runs such as `cargo run --locked -- run --demo --exit-after 5` (apply the SDK prefix above when needed). GPU initialization/draw logs can establish rendering activity, not desktop acceptance. Visual, focus, and drag checks remain pending:

- [ ] Inspect transparent alpha-edge smoothness over light and dark backgrounds: no dark fringe or opaque rectangle; compare `--opaque`. Expect nearest-sampled pixels, not filtered smoothing.
- [ ] Click to focus; test 1–9, Space, right-click, Q/Esc, and native left-drag.
- [ ] Keep another app focused during startup without `--focus` and during demo transitions; verify it retains focus. Test explicit `--focus` separately.
- [ ] Move between monitors with different display scales; inspect integer scaling, padding, and clipping at several `--scale` values.
- [ ] Check the default always-on-top request and compare `--normal-window-level` against ordinary windows.

## Related files

- [Architecture](ARCHITECTURE.md): delivered renderer, proposed protocol, and next milestones.
- [Naming](NAMING.md): selected name and rationale.
- [`src/main.rs`](src/main.rs): CLI and startup; [`src/window.rs`](src/window.rs): desktop events and rendering.
- [`src/animation.rs`](src/animation.rs): expressions and player; [`src/assets.rs`](src/assets.rs): atlas validation and frame cache.
