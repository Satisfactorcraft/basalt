//! Compiles the engine's GLSL shaders to SPIR-V.
//!
//! If `glslc` (ships with the Vulkan SDK / shaderc) is on PATH, shaders are
//! compiled fresh into OUT_DIR. Otherwise the prebuilt SPIR-V committed in
//! `shaders/prebuilt/` is used, so building does not require the SDK.

use std::path::{Path, PathBuf};
use std::process::Command;

const SHADERS: &[&str] = &["mesh.vert", "mesh.frag"];

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let shader_dir = Path::new("shaders");
    let prebuilt_dir = shader_dir.join("prebuilt");

    let have_glslc = Command::new("glslc").arg("--version").output().is_ok();
    if !have_glslc {
        println!("cargo:warning=glslc not found, using prebuilt SPIR-V from shaders/prebuilt/");
    }

    for name in SHADERS {
        let src = shader_dir.join(name);
        let spv_name = format!("{name}.spv");
        let dst = out_dir.join(&spv_name);
        println!("cargo:rerun-if-changed={}", src.display());
        println!(
            "cargo:rerun-if-changed={}",
            prebuilt_dir.join(&spv_name).display()
        );

        if have_glslc {
            let status = Command::new("glslc")
                .arg("-O")
                .arg("--target-env=vulkan1.3")
                .arg(&src)
                .arg("-o")
                .arg(&dst)
                .status()
                .expect("failed to run glslc");
            if !status.success() {
                panic!("glslc failed to compile {}", src.display());
            }
            // Keep the committed fallback in sync when developing with the SDK.
            if std::env::var_os("BASALT_UPDATE_PREBUILT").is_some() {
                std::fs::create_dir_all(&prebuilt_dir).unwrap();
                std::fs::copy(&dst, prebuilt_dir.join(&spv_name)).unwrap();
            }
        } else {
            let prebuilt = prebuilt_dir.join(&spv_name);
            std::fs::copy(&prebuilt, &dst).unwrap_or_else(|e| {
                panic!(
                    "no glslc and missing prebuilt shader {}: {e}",
                    prebuilt.display()
                )
            });
        }
    }
}
