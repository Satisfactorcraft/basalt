//! Basalt: the executable is the engine, the game is a folder.
//!
//! ```text
//! basalt [path/to/game]
//! ```
//! Without an argument, looks for a `game/` folder next to the executable,
//! then in the current directory.

mod app;
mod config;
mod hot_reload;
mod input;
mod renderer;
mod scene;
mod scripting;
mod vfs;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Result};
use log::{error, info};

use config::GameConfig;
use vfs::GameFs;

const USAGE: &str = "\
usage: basalt [GAME_FOLDER]

Runs the game in GAME_FOLDER. Without an argument, uses the `game` folder
next to the executable, or in the current directory.

A game folder contains game.json plus levels/, models/, textures/, scripts/.

environment:
  RUST_LOG=debug          more logging
  BASALT_VALIDATION=1|0   force Vulkan validation layers on/off";

fn find_game_folder() -> Result<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    if let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            Some("-V" | "--version") => {
                println!("basalt {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => return Ok(PathBuf::from(arg)),
        }
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
    {
        candidates.push(dir.join("game"));
    }
    candidates.push(PathBuf::from("game"));
    for c in &candidates {
        if c.join(config::CONFIG_FILE).is_file() {
            return Ok(c.clone());
        }
    }
    bail!("no game folder found (looked for game/game.json next to the executable and in the current directory)\n\n{USAGE}");
}

fn run() -> Result<()> {
    let folder = find_game_folder()?;
    let fs = GameFs::new(&folder)?;
    let config = GameConfig::load(&fs)?;
    info!(
        "basalt {} running '{}' from {}",
        env!("CARGO_PKG_VERSION"),
        config.title,
        fs.root().display()
    );
    app::run(fs, config)
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp(None)
        .init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}
