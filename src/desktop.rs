//! Select the native surface role before starting the graphical service.
use std::path::Path;

use anyhow::{Result, bail};
use clap::ValueEnum;
use tracing::info;

use crate::{RunOptions, assets::SpriteAtlas};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Backend {
    Auto,
    Winit,
    LayerShell,
}

#[cfg(any(target_os = "linux", test))]
fn prefer_layer(
    options: &RunOptions,
    wayland: bool,
    inherited_socket: bool,
    x11_requested: bool,
) -> bool {
    options.backend != Backend::Winit
        && wayland
        // connect_to_env consumes WAYLAND_SOCKET. Auto probing must not consume
        // the only connection before discovering that fallback is required.
        && !inherited_socket
        && !x11_requested
        && !options.focus
        && !options.normal_window_level
}

pub(crate) fn run(atlas: SpriteAtlas, options: RunOptions, socket: Option<&Path>) -> Result<()> {
    if options.backend == Backend::LayerShell && (options.focus || options.normal_window_level) {
        bail!(
            "layer-shell has no normal-window/focus mode; omit --focus/--normal-window-level or choose --backend winit"
        );
    }
    #[cfg(target_os = "linux")]
    {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("WAYLAND_SOCKET").is_some();
        let inherited_socket = std::env::var_os("WAYLAND_SOCKET").is_some();
        let x11_requested = std::env::var("WINIT_UNIX_BACKEND").as_deref() == Ok("x11");
        if options.backend == Backend::LayerShell
            || prefer_layer(&options, wayland, inherited_socket, x11_requested)
        {
            match crate::layer_shell::prepare() {
                Ok(prepared) => return prepared.run(atlas, options, socket),
                Err(error) if options.backend == Backend::LayerShell => return Err(error),
                Err(error) => {
                    info!(%error, "Layer-shell unavailable; using ordinary desktop windows")
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    if options.backend == Backend::LayerShell {
        bail!("layer-shell is available only on supported Linux Wayland desktops");
    }
    info!(backend = "Winit", "Desktop backend selected");
    crate::window::run(atlas, options, socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn options(args: &[&str]) -> RunOptions {
        let args = [vec!["numen", "run"], args.to_vec()].concat();
        let Some(crate::Command::Run(options)) = crate::Cli::try_parse_from(args).unwrap().command
        else {
            panic!("run options");
        };
        options
    }

    #[test]
    fn auto_prefers_layer_only_when_native_wayland_semantics_match() {
        let o = options(&[]);
        assert!(prefer_layer(&o, true, false, false));
        assert!(!prefer_layer(&o, false, false, false));
        assert!(!prefer_layer(&o, true, false, true));
        assert!(!prefer_layer(&o, true, true, false));
        for args in [
            &["--focus"][..],
            &["--normal-window-level"],
            &["--backend", "winit"],
        ] {
            assert!(!prefer_layer(&options(args), true, false, false));
        }
    }

    #[test]
    fn backend_values_are_explicit_and_validated() {
        assert_eq!(
            options(&["--backend", "layer-shell"]).backend,
            Backend::LayerShell
        );
        assert!(crate::Cli::try_parse_from(["numen", "run", "--backend", "unknown"]).is_err());
    }
}
