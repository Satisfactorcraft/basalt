# basalt — notes for Claude

## Git workflow (from the repo owner)
- Do all work on the `claude` branch. Never push to `main` directly.
- When work is done, open a PR from `claude` into `main` (or into `testing`
  if that branch exists and the change should be tested first).

## Project
- Rust game engine: the executable is the engine, a game is a folder
  (`game/` is the demo). See README.md for the folder format and Lua API.
- Build/test: `cargo build`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt`.
- Shaders: GLSL in `shaders/`, compiled by build.rs via `glslc`; after editing,
  run `BASALT_UPDATE_PREBUILT=1 cargo build` to refresh `shaders/prebuilt/`.
- Demo assets are generated: `python3 tools/gen_demo_assets.py`.
- Headless smoke test on Linux: run under Xvfb with
  `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json` (lavapipe);
  needs `libxkbcommon-x11`.

## Rules from the owner
- Bug hunts: when asked to find bugs, file one GitHub issue per bug (label `bug`)
  and do NOT fix them in the same pass. Say in each issue whether it was
  reproduced or only found in code review.
- Known bugs are tracked as GitHub issues (#2–#17 from the first hunt). Check
  open issues before fixing something or filing a duplicate.

## Lessons learned (v1 build)
- Git: a branch named `claude` exists, so `claude/<anything>` branches CANNOT
  be created (git ref conflict). Always use plain `claude`.
- mlua: enable the `error-send` feature, otherwise `mlua::Error` isn't
  Send+Sync and `?` into `anyhow::Result` fails to compile.
- mlua 0.11 has no lifetimes on Table/Function; store `LuaTable` directly.
  Engine state reaches Lua callbacks via `lua.set_app_data(Rc<Shared>)`.
  Never hold a `RefCell` borrow of the world while calling into Lua.
- Vulkan: Y is flipped in the projection (`proj.y_axis.y *= -1`) and the
  front face stays `COUNTER_CLOCKWISE`; this was verified with screenshots.
- winit on Linux X11 needs `libxkbcommon-x11` at runtime, or it panics at
  startup. Install it in the container: `apt-get install -y libxkbcommon-x11-0`.
- Testing in the container: `Xvfb :99` + `DISPLAY=:99` + lavapipe ICD. Take
  screenshots with `import -window root -crop 1280x720+0+0 out.png`. xdotool
  keys only arrive after `xdotool windowfocus $WID` (no window manager, so
  `windowactivate` fails).
- Debug builds run with the validation layer on; a clean run should log zero
  `vulkan` warnings or errors.
- A cold `cargo build` takes over 2 minutes (Lua is built from source);
  run it in the background.
- Windows: cross-compile with `cargo build --target x86_64-pc-windows-gnu`
  (needs the `gcc-mingw-w64-x86-64` package). It compiles but has never been
  run on real Windows. See issue #3.
- For Lua-side repros, add a throwaway `#[test]` using the `game()` helper in
  `src/scripting/mod.rs` tests, in a scratch copy of the source, built with
  `CARGO_TARGET_DIR=<repo>/target` so it reuses the compiled deps.

## Lessons learned (bug-fix round, #2–#17)
- Windows CAN be tested here: install `wine64`, then
  `WINEDEBUG=-all CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine cargo test --target x86_64-pc-windows-gnu`.
  It runs the full test suite as a real .exe. (No Vulkan under wine; tests only.)
- Minimize/focus behavior needs a window manager: run `openbox &` on the
  Xvfb display, then `xdotool windowminimize`. Find the window with
  `xdotool search --pid $(pgrep -x basalt)`, not `--name` (stale ids).
- Harness traps that produced false results: `pgrep -f` matches the
  `timeout` wrapper (use `pgrep -x basalt`); `cargo test` does not rebuild
  `target/debug/basalt` (run `cargo build` before manual runs); a
  non-executable fake `glslc` on PATH is skipped and the real one is found
  (build a PATH dir of symlinks without it instead); `timeout ... | grep`
  can kill the pipe before grep prints (write to a log file first).
- Debug input issues by temporarily eprintln-ing `WindowEvent`s; winit's
  `is_synthetic` and X11 auto-repeat were only visible that way.
- Script engine structure: class cache + pending instances live in
  `Shared` so Lua callbacks (`scene.spawn`) can create instances; never
  hold a `RefCell` borrow across Lua calls (script top-level code can spawn).
