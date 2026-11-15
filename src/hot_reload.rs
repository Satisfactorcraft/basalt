//! Watches the game folder and reports changed files (debounced).

use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::vfs::GameFs;

/// Editors write files in several steps; wait for quiet before reloading.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct Watcher {
    _watcher: notify::RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
    pending: HashSet<String>,
    last_event: Instant,
}

impl Watcher {
    pub fn new(fs: &GameFs) -> Result<Self> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })?;
        watcher.watch(fs.root(), RecursiveMode::Recursive)?;
        Ok(Self {
            _watcher: watcher,
            rx,
            pending: HashSet::new(),
            last_event: Instant::now(),
        })
    }

    /// Returns game-relative paths that changed, once things have settled.
    pub fn poll(&mut self, fs: &GameFs) -> Vec<String> {
        while let Ok(event) = self.rx.try_recv() {
            let Ok(event) = event else { continue };
            if !matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                continue;
            }
            for path in &event.paths {
                if let Some(rel) = fs.relative(path) {
                    if is_interesting(&rel) {
                        self.pending.insert(rel);
                        self.last_event = Instant::now();
                    }
                }
            }
        }
        if self.pending.is_empty() || self.last_event.elapsed() < DEBOUNCE {
            return Vec::new();
        }
        let mut changed: Vec<String> = self.pending.drain().collect();
        changed.sort();
        changed
    }
}

/// Skip editor swap/backup files and hidden files.
fn is_interesting(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    !(name.starts_with('.')
        || name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swx")
        || name.ends_with(".tmp")
        || name.starts_with('#'))
}
