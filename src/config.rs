//! `game.json`: the one file every game folder must have.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::vfs::GameFs;

pub const CONFIG_FILE: &str = "game.json";

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GameConfig {
    pub title: String,
    pub start_level: String,
    pub window: WindowConfig,
    /// Watch the game folder and reload scripts, levels and assets on change.
    pub hot_reload: bool,
    /// Enables developer hotkeys (F5 = reload level). Off by default so a
    /// shipped game can't be reset by a stray key press.
    pub dev_mode: bool,
    /// Longest a single script call (a hook, or a script file's top level)
    /// may run before it's stopped as a likely infinite loop. 0 = no limit.
    pub script_timeout_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WindowConfig {
    pub width: u32,
    pub height: u32,
    pub vsync: bool,
    pub resizable: bool,
    pub fullscreen: bool,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            title: "Basalt Game".into(),
            start_level: "levels/main.json".into(),
            window: WindowConfig::default(),
            hot_reload: true,
            dev_mode: false,
            script_timeout_ms: 1000,
        }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            vsync: true,
            resizable: true,
            fullscreen: false,
        }
    }
}

impl GameConfig {
    pub fn load(fs: &GameFs) -> Result<Self> {
        let text = fs.read_string(CONFIG_FILE).with_context(|| {
            format!(
                "{} has no {CONFIG_FILE}, is this a game folder?",
                fs.root().display()
            )
        })?;
        serde_json::from_str(&text).with_context(|| format!("invalid {CONFIG_FILE}"))
    }
}
