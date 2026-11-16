//! Watches the game folder and reports changed files (debounced).

use std::collections::HashSet;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use anyhow::Result;
use log::warn;
use notify::{EventKind, RecursiveMode, Watcher as _};

use crate::vfs::GameFs;

/// Editors write files in several steps; wait for quiet before reloading.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct Watcher {
    /// None only in tests, which feed events through the channel directly.
    _watcher: Option<notify::RecommendedWatcher>,
    rx: Receiver<notify::Result<notify::Event>>,
    pending: HashSet<String>,
    /// Events may have been lost (queue overflow, watcher error).
    rescan: bool,
    last_event: Instant,
    warned: HashSet<String>,
}

/// What changed since the last poll.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    pub paths: Vec<String>,
    /// The watcher may have missed events: treat everything as changed.
    pub rescan: bool,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.rescan
    }
}

impl Watcher {
    pub fn new(fs: &GameFs) -> Result<Self> {
        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(move |ev| {
            let _ = tx.send(ev);
        })?;
        watcher.watch(fs.root(), RecursiveMode::Recursive)?;
        Ok(Self::with_receiver(Some(watcher), rx))
    }

    fn with_receiver(
        watcher: Option<notify::RecommendedWatcher>,
        rx: Receiver<notify::Result<notify::Event>>,
    ) -> Self {
        Self {
            _watcher: watcher,
            rx,
            pending: HashSet::new(),
            rescan: false,
            last_event: Instant::now(),
            warned: HashSet::new(),
        }
    }

    /// Returns what changed, once things have settled.
    pub fn poll(&mut self, fs: &GameFs) -> Changes {
        while let Ok(event) = self.rx.try_recv() {
            let event = match event {
                Ok(event) => event,
                Err(e) => {
                    // E.g. inotify queue overflow or watch limit: changes may
                    // have been missed, so say so and reload everything.
                    let msg = e.to_string();
                    if self.warned.insert(msg.clone()) {
                        warn!("file watcher error, hot reload may miss changes: {msg}");
                    }
                    self.rescan = true;
                    self.last_event = Instant::now();
                    continue;
                }
            };
            if event.need_rescan() {
                if self.warned.insert("rescan".into()) {
                    warn!("file watcher dropped events (too many changes at once); reloading everything");
                }
                self.rescan = true;
                self.last_event = Instant::now();
            }
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
        if (self.pending.is_empty() && !self.rescan) || self.last_event.elapsed() < DEBOUNCE {
            return Changes::default();
        }
        let mut paths: Vec<String> = self.pending.drain().collect();
        paths.sort();
        Changes {
            paths,
            rescan: std::mem::take(&mut self.rescan),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{Flag, ModifyKind};

    #[test]
    fn errors_and_rescans_are_reported() {
        let root = std::env::temp_dir().join(format!("basalt-watch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let fs = GameFs::new(&root).unwrap();
        let (tx, rx) = channel();
        let mut w = Watcher::with_receiver(None, rx);

        tx.send(Err(notify::Error::generic("inotify queue overflow")))
            .unwrap();
        tx.send(Ok(notify::Event::new(EventKind::Modify(ModifyKind::Any))
            .add_path(fs.root().join("scripts/a.lua"))))
            .unwrap();
        // Like the game loop: the first poll drains, the debounce runs, then
        // a later poll reports.
        assert!(w.poll(&fs).is_empty());
        std::thread::sleep(DEBOUNCE + Duration::from_millis(20));
        let c = w.poll(&fs);
        assert!(c.rescan);
        assert_eq!(c.paths, ["scripts/a.lua"]);
        assert!(w.poll(&fs).is_empty(), "rescan is reported once");

        tx.send(Ok(
            notify::Event::new(EventKind::Other).set_flag(Flag::Rescan)
        ))
        .unwrap();
        assert!(w.poll(&fs).is_empty());
        std::thread::sleep(DEBOUNCE + Duration::from_millis(20));
        assert!(w.poll(&fs).rescan);
        let _ = std::fs::remove_dir_all(&root);
    }
}
