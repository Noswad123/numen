mod animation;
mod assets;
mod window;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use std::time::Duration;

use animation::Animation;
use assets::{CELL_HEIGHT, CELL_WIDTH, HexAtlas};

#[derive(Parser)]
#[command(version, about = "Numen — an independent desktop familiar")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show Hex in a draggable desktop window (rendering spike; no event socket yet).
    Run(RunOptions),
    /// Validate the bundled atlas and print clip information without opening a window.
    CheckAssets,
}

#[derive(Args)]
pub struct RunOptions {
    /// Initial expression. Success, failure, and waving play once, then return to idle.
    #[arg(long, value_enum, default_value = "idle")]
    animation: Animation,
    /// Window size relative to the 192 × 208 logical-pixel sprite cell.
    #[arg(long, default_value = "1.5", value_parser = parse_scale)]
    scale: f64,
    /// Cycle through every expression automatically, three seconds per selection.
    #[arg(long, conflicts_with = "paused")]
    demo: bool,
    /// Start with animation paused (also useful for reduced motion).
    #[arg(long)]
    paused: bool,
    /// Show an opaque black background to diagnose transparency problems.
    #[arg(long)]
    opaque: bool,
    /// Use normal window stacking instead of requesting always-on-top.
    #[arg(long)]
    normal_window_level: bool,
    /// Explicitly request keyboard focus at startup; otherwise leave focus alone.
    #[arg(long)]
    focus: bool,
    /// Close automatically after this many seconds, for a bounded graphical smoke test.
    #[arg(long, value_parser = parse_seconds)]
    exit_after: Option<Duration>,
}

fn parse_scale(value: &str) -> Result<f64, String> {
    let scale: f64 = value.parse().map_err(|_| "expected a number".to_owned())?;
    if scale.is_finite() && (0.5..=4.0).contains(&scale) {
        Ok(scale)
    } else {
        Err("scale must be a finite number between 0.5 and 4".to_owned())
    }
}

fn parse_seconds(value: &str) -> Result<Duration, String> {
    let seconds: f64 = value.parse().map_err(|_| "expected seconds".to_owned())?;
    if seconds.is_finite() && (0.1..=86400.0).contains(&seconds) {
        Ok(Duration::from_secs_f64(seconds))
    } else {
        Err("seconds must be a finite number between 0.1 and 86400".to_owned())
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "numen=info,wgpu=warn".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let atlas = HexAtlas::load().context("cannot load Hex runtime assets")?;
    match cli.command {
        Command::Run(options) => window::run(atlas, options),
        Command::CheckAssets => {
            println!("Hex: validated embedded RGBA atlas; {CELL_WIDTH} × {CELL_HEIGHT} cells");
            for animation in Animation::ALL {
                let clip = atlas.clip(animation);
                let total: Duration = clip.frame_durations.iter().copied().sum();
                println!(
                    "  {:<14} {:>2} frames, {:>4} ms, {}",
                    animation.label(),
                    clip.frame_durations.len(),
                    total.as_millis(),
                    if clip.looping {
                        "loop"
                    } else {
                        "once → idle"
                    }
                );
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_defaults_and_diagnostic_options() {
        Cli::command().debug_assert();
        let cli = Cli::try_parse_from(["numen", "run"]).unwrap();
        let Command::Run(options) = cli.command else {
            panic!("expected run")
        };
        assert_eq!(options.animation, Animation::Idle);
        assert_eq!(options.scale, 1.5);
        assert!(!options.focus);
        assert!(Cli::try_parse_from(["numen", "check-assets"]).is_ok());
        assert!(
            Cli::try_parse_from(["numen", "run", "--demo", "--opaque", "--exit-after", "0.5"])
                .is_ok()
        );
    }

    #[test]
    fn rejects_invalid_cli_values() {
        for value in ["NaN", "inf", "0", "0.49", "4.1", "huge"] {
            assert!(Cli::try_parse_from(["numen", "run", "--scale", value]).is_err());
        }
        for value in ["NaN", "inf", "0", "-1", "86401"] {
            assert!(Cli::try_parse_from(["numen", "run", "--exit-after", value]).is_err());
        }
        assert!(Cli::try_parse_from(["numen", "run", "--demo", "--paused"]).is_err());
        assert!(Cli::try_parse_from(["numen", "run", "--animation", "unknown"]).is_err());
    }

    use clap::CommandFactory;
}
