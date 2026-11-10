# Deprioritized features

## Overview

The two features below are deferred and low priority—not active implementation tasks or dependencies of fractional scaling. Fractional scaling is implemented independently of both. Reopening either scope requires separate user approval.

## Deferred scope and prerequisites

### Cross-output familiar transfer

- **Scope:** explicit monitor selection, transferring a familiar to another output, and seamless cross-monitor dragging.
- **Existing limits:** native Linux dragging and random corners stay on the current output. Output-removal recovery is not user-directed transfer.
- **Prerequisites:** separately approved output-selection/transfer semantics and multi-output/compositor validation. None is required for per-surface fractional scaling.

### Linux Go to mouse

- **Scope:** global pointer pursuit on Linux.
- **Existing limits:** Go to mouse remains disabled and forced dispatch is rejected; current-output corner navigation is independent of it.
- **Prerequisites:** a separately approved compositor adapter or suitable global-pointer capability and validation of its coordinate/support boundaries. Fractional scaling introduces neither that adapter nor that capability.

## Related files

- [README: native Wayland behavior and evidence](README.md#native-wayland-layer-shell-new-default)
- [Architecture: Linux desktop surface policy](ARCHITECTURE.md#linux-desktop-surface-policy)
- [`src/layer_shell.rs`](src/layer_shell.rs), [`src/layer_shell/travel.rs`](src/layer_shell/travel.rs), [`src/layer_shell/popup.rs`](src/layer_shell/popup.rs): current output-local behavior and action gates.
