# Familiar modes

## Overview

**The first multi-familiar foundation is delivered**, with up to 16 independently bound views in one service; automated checks and a three-view Metal smoke test pass. Each familiar retains **shared-focused semantics**: one exact `(source, instance, activity)`, not aggregate work. “Focused” means selected in Numen, not OS foreground, most recently updated or highest-priority background work. Ownership is exclusive across all familiars, including disconnected waiting pins.

## Delivered menu interaction

There are two separate surfaces: the existing terminal/fzf menu below, and a native macOS familiar menu. **Left-click release** opens the native menu; **right-click** opens the same menu instead of Preview cycling. Held left movement past five logical points starts native drag using the retained/revalidated original `LeftMouseDown` event. Other platforms retain left-drag/right-cycle; **1–9** Preview keys remain unchanged everywhere. Neither menu changes the shared-focused conversation model or gives sources execution authority; source updates never open the native popup.

Native actions are Show status (subject to `--no-bubble`), clicked-only Go to mouse (live pointer/navigation required), clicked-only Pause/Resume, Settings, Close familiar, and explicit **Quit Numen (all familiars)**. Settings shares the existing three fields/preset sets, submits one-field patches through the nonblocking 32-entry `FamiliarControl::settings` channel to the canonical Store, and reflects only authoritative snapshots. Checkmarks/Custom numeric labels use canonical `f64`, not renderer rounding. Manual/demo have no settings IPC/navigation, but permit pause, status reveal where enabled, Close and Quit. Last Close retains a live service; Quit exits all Numen views normally, never Djinn.

Opening/dismissing preserves cues, Preview/manual override, pause and navigation. Pending gestures/tracking hold geometry and suspend motion clocks; actual drag cancels motion and commits the hidden anchor. CursorLeave cancels click only, retaining held drag; mouse-up coordinates cannot initiate drag without physical button-down. A 20 ms held-state poll handles lost mouse-up. Synchronous `NSMenu` tracking suspends Rust GUI ticks, with clocks rebased afterward; a native 25 ms timer cancels on clicked-target removal, external quit or `--exit-after`, not ordinary source changes. Latest registry/familiar/WindowId revalidation ignores stale/failed/closing per-view actions; Quit remains service-wide independent of the clicked target. See [example and deployment](README.md#native-familiar-menu-macos).

The delivered [Numen menu / Wisp adapter](WISP.md) follows **Numen → select one or
more familiars → command**. Open attaches to a connected protocol target, including supported Djinn chats discovered by the optional default-on adapter, not an OS-scanned application. Close removes the familiar, with no separate Hide familiar action.
Disconnected familiars wait rather than disappearing or switching targets.

The familiar picker includes both running instances and available character
presets, clearly distinguished. Open/Attach chooses a target separately for each
selection. Each application instance/open Djinn session gets at most one familiar;
targets owned by another familiar or already chosen in the batch are not duplicate
attachment choices. This is per source/session, not per application type, so
different Djinn sessions can each have their own familiar.

The current target picker lists connected protocol activities directly, including supported Djinn interactive chats, not OS processes or all Djinn sessions. Future adapters should treat a supported app instance as one conversation and list open sessions without a process-to-conversation drill-down. Multiple bubbles may overlap with arbitrary stacking order. Menu Open can bootstrap an empty GUI; direct CLI Open still requires a running service.

`menu [--no-start] [--fzf PATH]` requires stdin/stderr TTYs. Tab selects up to 16 typed `familiar:` / `preset:` rows; Enter opens commands. Capabilities are intersected across the **whole** selection: presets only Open, active rows Info/Go to Mouse/Attach/Close as advertised. Mixed types can have no common command: Back/explanation replaces partial skipping. Command Escape/Back retains selection; target Escape/Back discards the entire pending batch. Ctrl-R refreshes the current stage; one accepted action exits. Only one Djinn preset exists, so open more copies by repeating invocations, not duplicating a preset row. Fresh per-selection target pages exclude other owners and earlier pending choices, allow the current owner to reselect, and offer Refresh/Back when empty. Mutation errors are shown for retry after backend revalidation. The menu pins the service generation and seals RPCs; after a service restart, close/reopen the menu rather than reusing old selected familiar IDs.

## API / Interfaces

| Mode | Scope and selection | Status / recommendation |
| --- | --- | --- |
| `shared-focused` | Each view pins one exact activity across connected producers. | Implemented, including legacy bootstrap `main`; no background takeover. |
| `per-process` | A view attached to one supported application instance, treated as one conversation in the initial attachment UI. | Future design choice when process-level separation is useful; not permission to aggregate every conversation. |
| `per-conversation` | A separate view pinned to each explicitly chosen protocol activity. | Open/Attach and protocol-target menu delivered; supported Djinn interactive discovery available, broader adapters remain future work. |
| `follow-selected` | A view follows an explicit selected-conversation signal from an application. | Future convenience mode; recommend explicit app signals, never inferred OS focus. No signal contract is implemented yet. |

These choices separate **view scope**, **conversation selection**, and **character mapping**. Multiple current views share one canonical activity store and service; each has private bindings/cues/summary cache and rendering state. There is no general mode-selection CLI or OS process scan. The optional supported Djinn interactive adapter discovers Chat targets by default in updated builds; it is not an OS-focus-following mode. Only preset `djinn` is available; per-view runtime settings do not select different characters.

`status [--offset N] [--familiar ID]` returns `mode: "shared-focused"`, exact focus, availability and four activities/page. `focus [--familiar ID] --source SOURCE --activity ACTIVITY [--instance INSTANCE]` requires one live match not owned by another familiar; `focus [--familiar ID] --next` cycles eligible lexical live records, including terminals. Omitted ID always scopes to `main`, never another view; explicit unknown IDs error. Closing `main` leaves legacy status focus null and never recreates it; focus needs an existing familiar. Tab uses bounded `FamiliarControl::next(id)` for its own view. All prior 8 KiB packet bounds remain.

Discovery: offline `presets` shares `menu::presets` as catalog source of truth and lists `djinn` / Djinn with capability `open`; `familiars --offset N` pages active registry rows (including unavailable attachments); `targets --offset N [--familiar ID]` pages connected protocol-advertised activities with `claimed_by` and `eligible`. Owners may reselect their own targets. The menu filters eligible entries and reserves earlier choices locally. GUI familiar capabilities are `info`, `attach`, `close`, plus `go-to-mouse` only with an installed navigation callback (currently after macOS pointer initialization). Headless/legacy handlers advertise `[]`; old `start_with_familiars` omits navigation and rejects it. Preset IDs are distinct from active IDs.

Lifecycle: `open --source NAME --activity A [--instance I] [--id ID] [--preset djinn]`, `attach --familiar ID --source NAME --activity A [--instance I]`, and `close --familiar ID1 ID2`. Open/Attach accept `--batch FILE` or `--batch -` (stdin): root JSON arrays with one flattened `{id, source, instance?, activity}` per selection (Open allows omitted `id` and optional `preset`). Duplicate owners/targets, unavailable or ambiguous matches, duplicate IDs and unknown presets reject the whole batch without changes. Attach cannot swap another familiar's currently owned target even within one batch. Revalidation occurs at mutation; sources are never executed/stopped. Open requires an already-running graphical service; headless discovery is not GUI lifecycle. See [Protocol](PROTOCOL.md) for complete contracts.

Bubble visibility is independent of selection. Global macOS **Option+I (Alt+I)** registers once per process and `numen bubbles` reveals all actual current views without changing selection/focus; `bubbles --familiar ID1 ID2` targets only that subset. The bounded reveal queue preserves subsets (32 requests × 16 IDs), never accidentally broadcasting. No cross-process broadcast exists. Headless `listen` rejects reveal. ACK means accepted, not guaranteed rendered; asynchronous native/GPU creation can fail and remove only the failed view after Open acceptance.

## Data Flow

Go to mouse is a local motion intent, not a conversation mode or source outcome. `go-to-mouse --familiar ID1 ID2` requires 1–16 unique existing IDs and sends `familiar.go-to-mouse {ids}`; missing/empty/duplicate/unknown/closed IDs reject atomically without a callback. ACK `moving` means accepted, not arrived. A bounded target-only queue applies the newest registry first, then starts/restarts just those views, never implicit all. No binding, activity state or liveness changes. Automated checks and a two-window native pursuit/arrival smoke verified the command; the menu offers it only if every selected familiar advertises support.

Global macOS **Option+M** adds GUI-only all-current navigation in the current service, not across processes and not an aggregate conversation mode. It queues bounded all-current intent; the main GUI loop resolves the latest registry and skips closing/failed views. Zero views is a no-op, retaining registration for later Open. CLI/wire still require explicit IDs. Focused local exact Alt+M moves only that view when global M is unregistered; successful M registration suppresses local duplicates independently of I. Pointer initialization must succeed in a live macOS session; manual/demo and non-macOS have no M/navigation. Pause is preserved and intent waits for resume, with no activation/focus stealing.

Each actively seeking view pursues the freshly sampled pointer at roughly 60 Hz, using actual sprite alpha ≥32 rather than bubble/window empty space. Arrival requires actual window position confirmation, plays one success jump and resumes latest attached emotes; there is no ongoing follow. Multiple views may overlap at the pointer without smart placement. Source status/summary changes still reach bubbles while motion owns the visual pose; expired cues are not replayed afterward. Read-only pointer snapshots currently require macOS; there is no OS input injection, permission request or process scan. See [Architecture](ARCHITECTURE.md#navigation-and-coordinate-contract) for global logical-point coordinates, actual-size geometry and bounded physics.

All producers → canonical bounded activity store → per-familiar exact binding, cue and retained summary → complete latest-only `Vec<FamiliarPresentation>` snapshots → outer App reconciliation → independent expression controllers, bubbles and views. The outer App routes native Window IDs; each view owns Window/Pixels, Player, pause, visibility and geometry. An `Arc<SpriteAtlas>` and parsed font are shared; `Typeface::fork` keeps glyph caches independent.

When visible, the thought bubble shows only the selected `source / activity / instance`, its explicit producer summary (or activity ID fallback), and current base status. It never summarizes aggregate work or calls a model. No focus shows Choose a conversation / Not following. Manual/demo overrides visibly say Preview rather than implying agent activity. The optional supported Djinn adapter reuses existing sanitized chat titles; see [setup and privacy](README.md#optional-djinn-interactive-adapter).

The default `BubbleVisibility` starts hidden. First selection/new focus or selected status/summary changes briefly notify for five seconds; initial empty No conversation, heartbeat, reason-only, cue-expiry and background changes do not notify. Automatic visible updates repaint without extending the deadline. Expiry starts 750 ms quiet time for automatic updates; suppressed changes are cached, not queued. Explicit reveal/new focus bypasses quiet time and refreshes the duration. Hide runs even paused/occluded. `--bubble-duration` is finite 0.25–30 seconds; `--always-bubble` opts into persistent debug/preview display and conflicts with `--no-bubble`.

Only bootstrap `main` auto-chooses the first unclaimed live activity once, lexically among initial snapshot records. Other views open with explicit targets. Pins then persist. Background updates cannot inject attention/outcomes into another view. Only fresh success/failure transitions of that familiar's activity cue a one-shot; afterward it rests unless the same activity has resumed work.

Character identity is independent: today's temporary global default is ID `djinn`, name Djinn, using `SpriteAtlas` and embedded `assets/djinn/`. Every tool currently uses that fallback. A different global default plus explicit application-source `djinn` → character `djinn` mapping is planned, not implemented; equal spellings do not establish a mapping.

## Edge Cases

- Discovery/menu entry never spawns or writes files. Choosing offline Open can start detached `run --empty` with no `main`; source updates alone do not create one. `--no-start` shows an error instead. Cancelling target choice sends no Open/Attach mutation but may leave that empty service resident. `quit` gracefully stops the whole current graphical service, including zero-window services; Close remains per-view. Neither stops source applications. Headless/legacy handlers without a shutdown callback reject quit. See [Protocol](PROTOCOL.md#service-readiness-and-graceful-quit).

- Reissuing Go to mouse restarts intent. Actual native drag, digits, non-macOS right-click cycle, L, Tab and close cancel without success; native menu opening/dismissal and Option+I/reveal-all do not. Space pauses motion and animation, including requests received while paused; bubble deadlines continue outside synchronous native tracking. Occlusion alone does not suspend pursuit or celebration. Hard deadline gates, accumulated fractional intent and cached compositor-matched alpha prevent early-event acceleration or invisible-pixel arrival. Stalling/non-convergence cancels with a warning rather than reporting success.
- Movement commits the hidden anchor so bubble expiry does not restore a pre-command location. `run --move-speed` is finite 40–2000 desktop logical points/second, default 360, independent of font size 12, sprite scale 0.5 and scene scale 1.5. Actual pointer motion, monitor crossings, occlusion and edge clamping still need manual acceptance; feature MVP tests used no synthetic OS input.
- Removal, snapshot omission, disconnect or expiry leaves the pin unavailable, not reassigned to background work. A new producer instance needs explicit selection/next. The same exact record becoming available can satisfy the retained pin again.
- Optional `Presentation.following {source, instance, activity, summary?, available}` retains identity and last selected summary through unavailability, even with no base/cue. Focus changes replace this cache, including with no summary; background updates cannot change bubble text. A present replacement record with omitted/null summary clears its old summary, unlike omission of the whole activity.
- Missing/ambiguous/other-owned matches leave selection unchanged. Next chooses the first eligible lexical record if the pin is unavailable, wraps, and errors if none exist.
- Terminal records remain selectable and retain identity but map to idle. Selection, registration and snapshots do not replay historical completion cues.
- Bubble status follows the current base, not transient cues: Completed/Failed/Cancelled can remain visible while the pet rests. Waiting distinguishes Needs your input from Waiting on dependency. Metadata-only updates repaint while paused or on the same animation, without restarting it or stealing focus.
- 1–9 (and non-macOS right-click) override expressions, not conversation identity; macOS right-click opens the native menu. L resumes the selected live base without switching conversations. CLI focus does not cancel that GUI override; Tab both requests next and resumes live control. Tab/L cannot enable IPC in `--manual` or `--demo` sessions.
- 1–9 (and non-macOS right-click) and Tab/L reveal only that view. Demo cycling does not auto-reveal. Notifications and later Open never request focus; startup `main` can opt in with `--focus`. Native menu/drag focus behavior remains a manual acceptance check.
- Q/Esc/native close closes only the addressed live familiar. Last-close keeps IPC/hotkey alive with zero windows for later Open; Ctrl-C quits the service. Manual/demo remain socket-free single-view sessions with close-to-exit behavior. Native/GPU errors close only the failed live view and retry registry removal as needed; shared font initialization remains fatal. Per-view timers/pause are isolated; the global exit deadline remains scheduled with zero views.
- Global I/M use independent guards and repeat latches until matching release, sharing one main-thread Carbon manager via `global-hotkey` 0.9 and one capture-free two-weak-target dispatcher, not EventTap/all-keystroke capture. A collision warns without disabling the other key. `--no-hotkey` disables both global keys, not focused local I/M or CLI controls. Bounded private zero-view native registration and normal-exit cleanup passed all four [smoke cases](README.md#final-optionm-validation), including M registration in a following fresh process. Collision handling was tested deterministically, not through real OS collision. Actual OS presses/background-focus delivery and hotkey-driven motion remain unverified; historical CLI-driven native motion evidence is separate. No synthetic OS keys, input taps, permission requests or existing-user-service control/restarts occurred. Native unregister is best-effort; see [cleanup limitation](ARCHITECTURE.md#live-display-and-output).
- IDs are stable only per service lifetime: generated `familiar-N` is monotonic without generated-history growth; closed IDs are not reused. Explicit-ID history is bounded to 256 plus reserved `main` (257 total). No persistent profiles/preferences or OS-focus inference exist; restart resets IDs.
- Explicit summaries may be sensitive on screen. `run --no-bubble` hard-disables every reveal and global I registration, but not global M; it remains sprite-only and skips font discovery/loading without redacting wire/state or status/headless output. Bubble-capable GUIs load fonts once even while initially hidden. Numen reads no transcripts and automatically copies no raw prompts.
- Character sizing does not change mode or selection. `--sprite-scale` is finite 0.25–2, default 0.5: native 192 × 208 atlas cells render at 96 × 104 logical units. The native window starts sprite-only, expands to 384 × 248 when revealed (sprite at `(144, 144)`), and shrinks back on hide without a full hidden input area. Expansion preserves the anchor where possible and clamps to current monitor bounds; collapse restores the saved hidden position unless an explicit left-drag commits a new location. Unsupported positioning is best-effort. `--sprite-scale 1` gives 192 × 208 hidden or 384 × 352 shown without changing bubble/text size. `--scale` remains the whole-scene multiplier, default 1.5, including the font.
- Typography does not change selection, source identity or character mapping. The GUI prefers installed SauceCodePro Nerd Font Regular, with bundled OFL Source Code Pro Regular (no Nerd icons) as default fallback; it does not parse Kitty at runtime. Explicit `--font-family`/`--font-file` overrides conflict and error on unusable selection when the bubble is enabled. `--font-size` is finite 8–32, default 12 logical units, scaled with the scene/display independently of sprite scale; it never resizes the sprite.
- Text is antialiased at physical display resolution, while only sprite art is nearest-scaled. Measured wrapping uses up to three lines as space/line height permit and U+2026 `…` truncation. Unicode/Nerd icons depend on the chosen face; missing glyphs use its U+FFFD, then `?`, then `.notdef`. No full script shaping, color emoji or fallback-font chain exists; stored UTF-8 metadata is unchanged. See [font details](assets/fonts/README.md).

## Example usage

On macOS, release a left-click → Pause (only that view), reopen → Resume; Settings → Text size → Large (16) submits one canonical edit. Close familiar leaves the service resident; Quit Numen (all familiars) shuts it down. An older service requires one user-explicit restart with the rebuilt Numen binary to gain this menu/Option+M/settings; never auto-restart it or restart Djinn for this update. The updated binary was built, but no app restart or home configuration change occurred; existing live user processes were untouched.

Confirmed after the latest poll/click-race fix with the macOS 26.5 SDK workaround: **278 passed (257 unit + 21 CLI), one existing visual-preview test ignored**. `cargo fmt --check`, `cargo test --offline --locked`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo build --offline --locked` and `git diff --check` passed. These are reported main-agent results, not checks rerun here. An independent temporary AppKit/winit window used the production popup adapter on the main thread for three successful automatic cancellations (**380/378/353 ms**, `Arc<AtomicBool>` and native 25 ms event-tracking `NSTimer`, exit 0/no errors). This checks popup construction/placement API use and repeated timer/tracking cleanup only. No synthetic OS keys/mouse, hotkeys or existing-user-app controls were used. Physical click/drag/item selection, Close versus Quit, Esc, focus and mixed-DPI acceptance remain manual; historical source-handoff/settings/Option+M evidence is separate.

With `./target/debug/numen run` or `listen` already serving and activities connected:

```sh
./target/debug/numen status
./target/debug/numen status --offset 4
./target/debug/numen focus --source djinn --activity chatid
# Resolve ambiguity between producer lifetimes:
./target/debug/numen focus --source djinn --activity chatid --instance producer-1
./target/debug/numen focus --next
./target/debug/numen bubbles # Reveal only: does not change mode/selection
./target/debug/numen --socket /path/to/private/events.sock bubbles
./target/debug/numen presets
./target/debug/numen familiars --offset 0
./target/debug/numen targets --familiar main --offset 0
# Graphical service only; build/worker-2/task must be connected and unclaimed:
./target/debug/numen open --source build --instance worker-2 --activity task --id build-pet
./target/debug/numen status --familiar build-pet
./target/debug/numen bubbles --familiar main build-pet
./target/debug/numen go-to-mouse --familiar main build-pet
./target/debug/numen close --familiar build-pet
```

These are explicit selection controls, not an automatic Djinn adapter. Use the returned `focus_available` independently of the retained focus identity.

To supply a summary from a diagnostic producer (select it explicitly if another pin already exists):

```sh
./target/debug/numen publish --source djinn --instance producer-1 --activity chatid --state working --summary 'Investigating retry backoff' --duration 60
```

`--summary` is optional, nonblank, limited to 160 UTF-8 bytes, and rejects control/bidi embedding/isolate characters. The CLI trims surrounding whitespace and repeats it on every state, including demo; no summary is generated by Numen.

## Related files

- [Roadmap](ROADMAP.md): delivered menu/bootstrap and remaining automatic adapters.
- [`src/context_menu.rs`](src/context_menu.rs): declarative native actions and macOS adapter, separate from the fzf picker.
- [Wisp](WISP.md): opt-in palette setup, menu controls, installer recovery and validation boundaries.
- [README](README.md): commands, controls and pending acceptance checks.
- [Architecture](ARCHITECTURE.md): service, presentation and rendering boundaries.
- [Protocol](PROTOCOL.md): control ACKs, paging, producer lifecycle and security bounds.
- [`src/activity.rs`](src/activity.rs): exact focus and store; [`src/ipc.rs`](src/ipc.rs): bounded control transport.
- [`src/main.rs`](src/main.rs), [`src/window.rs`](src/window.rs), [`src/display.rs`](src/display.rs): CLI, Tab/live controls and terminal expression mapping.
- [`src/motion.rs`](src/motion.rs), [`src/pointer.rs`](src/pointer.rs): pure navigation and read-only macOS pointer coordinates, independent of conversation selection.
- [`src/bubble.rs`](src/bubble.rs): selected-conversation text, Preview labels and CPU composition.
- [`src/visibility.rs`](src/visibility.rs), [`src/hotkey.rs`](src/hotkey.rs): transient visibility policy and owned global reveal shortcut.
- [`src/typography.rs`](src/typography.rs), [font assets README](assets/fonts/README.md): font selection and rendering limits, independent of familiar mode.
