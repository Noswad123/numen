# Familiar modes

## Overview

**Shared-focused is implemented and recommended for the current single-view familiar.** One visible familiar represents **one focused conversation**, identified by an exact `(source, instance, activity)` tuple. “Focused” means selected in Numen, not the OS foreground window, most recently updated conversation, or highest-priority background task. Aggregate automatic attention/outcome arbitration is not the product goal.

## API / Interfaces

| Mode | Scope and selection | Status / recommendation |
| --- | --- | --- |
| `shared-focused` | One shared view pinned to one exact activity across connected producers. | Implemented; use for predictable ambient awareness without background takeover. |
| `per-process` | A view scoped to a producer process/instance; conversation selection remains a separate concern if it hosts several activities. | Future design choice when process-level separation is useful; not permission to aggregate every conversation. |
| `per-conversation` | A separate view pinned to each chosen conversation. | Future option for simultaneous independent visibility; prefer opt-in views to desktop clutter. |
| `follow-selected` | A view follows an explicit selected-conversation signal from an application. | Future convenience mode; recommend explicit app signals, never inferred OS focus. No signal contract is implemented yet. |

These choices separate **view scope**, **conversation selection**, and **character mapping**. Multiple future views can share one service/store; one process per pet is not required. No mode-selection CLI or multiple-view implementation exists today.

Current interfaces: `status [--offset N]` returns JSON with `mode: "shared-focused"`, exact focus, availability and four activities per page, including optional summaries. Four-record pages keep worst-case ACKs below 8 KiB. `focus --source SOURCE --activity ACTIVITY [--instance INSTANCE]` requires exactly one live match. `focus --next` cycles lexical live records, including terminals, with wraparound. Tab uses a bounded nonblocking GUI request and resumes live expression control. See [Protocol](PROTOCOL.md) for wire controls and limits.

## Data Flow

All producers → bounded store → exact selected activity → base plus fresh focused cue and retained following context → expression controller + thought bubble → one view.

The default thought bubble shows only the selected `source / activity / instance`, its explicit producer summary (or activity ID fallback), and current base status. It never summarizes aggregate work or calls a model. No focus shows Choose a conversation / Not following. Manual/demo overrides visibly say Preview rather than implying agent activity. A future Djinn adapter is expected to reuse existing title/short-intent metadata; it is not implemented.

The first available activity is auto-chosen once, using lexical order among records in the initial snapshot. Thereafter the exact pin persists. Background updates remain stored/queryable but cannot change the view or inject attention/failure/success cues. Only a new terminal transition of the focused conversation can produce a one-shot; afterward it rests unless that same conversation has already resumed work.

Character identity is independent: today's temporary global default is ID `djinn`, name Djinn, using `SpriteAtlas` and embedded `assets/djinn/`. Every tool currently uses that fallback. A different global default plus explicit application-source `djinn` → character `djinn` mapping is planned, not implemented; equal spellings do not establish a mapping.

## Edge Cases

- Removal, snapshot omission, disconnect or expiry leaves the pin unavailable, not reassigned to background work. A new producer instance needs explicit selection/next. The same exact record becoming available can satisfy the retained pin again.
- Optional `Presentation.following {source, instance, activity, summary?, available}` retains identity and last selected summary through unavailability, even with no base/cue. Focus changes replace this cache, including with no summary; background updates cannot change bubble text. A present replacement record with omitted/null summary clears its old summary, unlike omission of the whole activity.
- Missing/ambiguous explicit matches error without changing selection. Next chooses the first lexical record if the pin is unavailable, wraps after the last, and errors if no records exist.
- Terminal records remain selectable and retain identity but map to idle. Selection, registration and snapshots do not replay historical completion cues.
- Bubble status follows the current base, not transient cues: Completed/Failed/Cancelled can remain visible while the pet rests. Waiting distinguishes Needs your input from Waiting on dependency. Metadata-only updates repaint while paused or on the same animation, without restarting it or stealing focus.
- 1–9/right-click override expressions, not conversation identity. L resumes the selected live base without switching conversations. CLI focus does not cancel that GUI override; Tab both requests next and resumes live control. Tab/L cannot enable IPC in `--manual` or `--demo` sessions.
- Selection and state are ephemeral; there is no durable preference/history store or OS-focus inference.
- Explicit summaries may be sensitive on screen. `run --no-bubble` restores the 192 × 208 sprite-only canvas instead of the default 384 × 352 bubble canvas and skips font discovery/loading entirely; it does not redact wire/state or status/headless output. Numen reads no transcripts and automatically copies no raw prompts.
- Typography does not change selection, source identity or character mapping. The GUI prefers installed SauceCodePro Nerd Font Regular, with bundled OFL Source Code Pro Regular (no Nerd icons) as default fallback; it does not parse Kitty at runtime. Explicit `--font-family`/`--font-file` overrides conflict and error on unusable selection when the bubble is enabled. `--font-size` is finite 8–32, default 16 logical units, scaled with the scene/display; it never resizes the sprite.
- Text is antialiased at physical display resolution, while only sprite art is nearest-scaled. Measured wrapping uses up to three lines as space/line height permit and U+2026 `…` truncation. Unicode/Nerd icons depend on the chosen face; missing glyphs use its U+FFFD, then `?`, then `.notdef`. No full script shaping, color emoji or fallback-font chain exists; stored UTF-8 metadata is unchanged. See [font details](assets/fonts/README.md).

## Example usage

With `./target/debug/numen run` or `listen` already serving and activities connected:

```sh
./target/debug/numen status
./target/debug/numen status --offset 4
./target/debug/numen focus --source djinn --activity chatid
# Resolve ambiguity between producer lifetimes:
./target/debug/numen focus --source djinn --activity chatid --instance producer-1
./target/debug/numen focus --next
```

These are explicit selection controls, not an automatic Djinn adapter. Use the returned `focus_available` independently of the retained focus identity.

To supply a summary from a diagnostic producer (select it explicitly if another pin already exists):

```sh
./target/debug/numen publish --source djinn --instance producer-1 --activity chatid --state working --summary 'Investigating retry backoff' --duration 60
```

`--summary` is optional, nonblank, limited to 160 UTF-8 bytes, and rejects control/bidi embedding/isolate characters. The CLI trims surrounding whitespace and repeats it on every state, including demo; no summary is generated by Numen.

## Related files

- [README](README.md): commands, controls and pending acceptance checks.
- [Architecture](ARCHITECTURE.md): service, presentation and rendering boundaries.
- [Protocol](PROTOCOL.md): control ACKs, paging, producer lifecycle and security bounds.
- [`src/activity.rs`](src/activity.rs): exact focus and store; [`src/ipc.rs`](src/ipc.rs): bounded control transport.
- [`src/main.rs`](src/main.rs), [`src/window.rs`](src/window.rs), [`src/display.rs`](src/display.rs): CLI, Tab/live controls and terminal expression mapping.
- [`src/bubble.rs`](src/bubble.rs): selected-conversation text, Preview labels and CPU composition.
- [`src/typography.rs`](src/typography.rs), [font assets README](assets/fonts/README.md): font selection and rendering limits, independent of familiar mode.
