//! Window, event loop and the per-frame order of things:
//! input -> hot reload -> scripts -> engine requests -> render.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use log::{error, info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};

use crate::config::{GameConfig, CONFIG_FILE};
use crate::hot_reload::Watcher;
use crate::renderer::Renderer;
use crate::scripting::{ScriptHost, Shared};
use crate::vfs::GameFs;

pub fn run(fs: GameFs, config: GameConfig) -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);

    let shared = Rc::new(Shared::new(
        fs.clone(),
        (config.window.width, config.window.height),
    ));
    let scripts = ScriptHost::new(shared.clone())?;
    let watcher = if config.hot_reload {
        match Watcher::new(&fs) {
            Ok(w) => {
                info!("hot reload on: watching {}", fs.root().display());
                Some(w)
            }
            Err(e) => {
                warn!("hot reload unavailable: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut app = App {
        renderer: None,
        window: None,
        fs,
        config,
        shared,
        scripts,
        watcher,
        last_frame: Instant::now(),
        fps_timer: Instant::now(),
        fps_frames: 0,
        fatal: None,
    };
    event_loop.run_app(&mut app)?;
    match app.fatal.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

struct App {
    // Field order matters: the renderer must drop before the window.
    renderer: Option<Renderer>,
    window: Option<Arc<Window>>,
    fs: GameFs,
    config: GameConfig,
    shared: Rc<Shared>,
    scripts: ScriptHost,
    watcher: Option<Watcher>,
    last_frame: Instant,
    fps_timer: Instant,
    fps_frames: u32,
    fatal: Option<anyhow::Error>,
}

impl App {
    fn init(&mut self, el: &ActiveEventLoop) -> Result<()> {
        let w = &self.config.window;
        let mut attrs = Window::default_attributes()
            .with_title(&self.config.title)
            .with_inner_size(LogicalSize::new(w.width, w.height))
            .with_resizable(w.resizable);
        if w.fullscreen {
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        let window = Arc::new(el.create_window(attrs)?);
        let size = window.inner_size();
        self.shared.window_size.set((size.width, size.height));

        let renderer = Renderer::new(&window, self.fs.clone(), &self.config.title, w.vsync)?;
        self.renderer = Some(renderer);
        self.window = Some(window);

        let level = self.config.start_level.clone();
        if let Err(e) = self.scripts.load_level(&level) {
            error!("failed to load start level: {e:#}");
        }
        self.last_frame = Instant::now();
        Ok(())
    }

    fn frame(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        // Clamp so a breakpoint or a long load doesn't teleport everything.
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.1);
        self.last_frame = now;

        self.handle_file_changes();

        // Built-in dev key: F5 reloads the current level.
        if self.shared.input.borrow().key_pressed("f5") {
            self.reload_level();
        }

        self.scripts.update(dt);
        self.handle_requests(el);

        if let Some(r) = self.renderer.as_mut() {
            if let Err(e) = r.render(&self.shared.world.borrow()) {
                error!("render failed: {e:#}");
                self.fatal = Some(e);
                el.exit();
            }
        }
        self.shared.input.borrow_mut().end_frame();

        self.fps_frames += 1;
        if self.fps_timer.elapsed().as_secs_f32() >= 5.0 {
            let fps = self.fps_frames as f32 / self.fps_timer.elapsed().as_secs_f32();
            log::debug!("{fps:.0} fps");
            self.fps_timer = Instant::now();
            self.fps_frames = 0;
        }
    }

    fn reload_level(&mut self) {
        let level = self.shared.world.borrow().level_path.clone();
        if let Some(level) = level {
            if let Err(e) = self.scripts.load_level(&level) {
                error!("level reload failed: {e:#}");
            }
        }
    }

    fn handle_file_changes(&mut self) {
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        let changed = watcher.poll(&self.fs);
        if changed.is_empty() {
            return;
        }
        let current_level = self.shared.world.borrow().level_path.clone();
        let (mut scripts, mut level, mut assets) = (false, false, false);
        for path in &changed {
            info!("changed: {path}");
            if path.ends_with(".lua") {
                scripts = true;
            } else if Some(path) == current_level.as_ref() {
                level = true;
            } else if path == CONFIG_FILE {
                warn!("{CONFIG_FILE} changed, restart the game to apply it");
            } else if is_asset(path) {
                assets = true;
            }
        }
        if assets {
            if let Some(r) = self.renderer.as_mut() {
                r.invalidate_models();
                info!("hot reload: assets reloaded");
            }
        }
        if scripts {
            self.scripts.reload_scripts();
        }
        if level {
            self.reload_level();
        }
    }

    fn handle_requests(&mut self, el: &ActiveEventLoop) {
        let req = std::mem::take(&mut *self.shared.requests.borrow_mut());

        if let Some(level) = req.load_level {
            if let Err(e) = self.scripts.load_level(&level) {
                error!("failed to load level: {e:#}");
            }
        }
        if let Some(window) = &self.window {
            if let Some(title) = req.set_title {
                window.set_title(&title);
            }
            if let Some(lock) = req.cursor_lock {
                let result = if lock {
                    window
                        .set_cursor_grab(CursorGrabMode::Locked)
                        .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
                } else {
                    window.set_cursor_grab(CursorGrabMode::None)
                };
                if let Err(e) = result {
                    warn!("cursor lock failed: {e}");
                }
                window.set_cursor_visible(!lock);
            }
        }
        if req.quit {
            info!("game requested quit");
            el.exit();
        }
    }
}

/// Files that models are built from. Anything else (editor temp files,
/// notes, data files) doesn't need the GPU cache flushed.
fn is_asset(path: &str) -> bool {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    matches!(
        ext.as_deref(),
        Some("gltf" | "glb" | "bin" | "png" | "jpg" | "jpeg")
    )
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.init(el) {
            self.fatal = Some(e);
            el.exit();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => {
                self.shared.window_size.set((size.width, size.height));
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let pressed = event.state == ElementState::Pressed;
                    self.shared.input.borrow_mut().on_key(
                        code,
                        pressed,
                        event.repeat,
                        is_synthetic,
                    );
                    // Alt+Enter toggles fullscreen.
                    if pressed
                        && !is_synthetic
                        && !event.repeat
                        && code == KeyCode::Enter
                        && self.shared.input.borrow().key_down("alt")
                    {
                        if let Some(w) = &self.window {
                            let fs = w
                                .fullscreen()
                                .is_none()
                                .then_some(Fullscreen::Borderless(None));
                            w.set_fullscreen(fs);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.shared
                    .input
                    .borrow_mut()
                    .on_mouse_button(button, state == ElementState::Pressed);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.shared
                    .input
                    .borrow_mut()
                    .on_cursor_moved(position.x, position.y);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let amount = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 40.0,
                };
                self.shared.input.borrow_mut().on_scroll(amount);
            }
            WindowEvent::Focused(false) => self.shared.input.borrow_mut().release_all(),
            WindowEvent::RedrawRequested => self.frame(el),
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            self.shared
                .input
                .borrow_mut()
                .on_mouse_motion(delta.0, delta.1);
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // GPU resources first, then the window they render into.
        self.renderer = None;
        self.window = None;
    }
}
