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
