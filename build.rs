//! Compiles the engine's GLSL shaders to SPIR-V.
//!
//! If `glslc` (ships with the Vulkan SDK / shaderc) is on PATH, shaders are
//! compiled fresh into OUT_DIR. Otherwise, or if glslc fails while the
//! source is unchanged, the prebuilt SPIR-V committed in `shaders/prebuilt/`
//! is used, so building does not require the SDK.
//!
//! Each prebuilt `.spv` has a `.spv.hash` holding a hash of the GLSL it was
//! built from, so the build can tell whether the prebuilt copy is current.
//! Refresh both with `BASALT_UPDATE_PREBUILT=1 cargo build`.

use std::path::{Path, PathBuf};
use std::process::Command;

const SHADERS: &[&str] = &["mesh.vert", "mesh.frag"];

/// FNV-1a over the source with CRLF normalized, so a Windows checkout with
/// autocrlf hashes the same as Linux.
fn source_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes().filter(|&b| b != b'\r') {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let shader_dir = Path::new("shaders");
    let prebuilt_dir = shader_dir.join("prebuilt");
    let update_prebuilt = std::env::var_os("BASALT_UPDATE_PREBUILT").is_some();
    println!("cargo:rerun-if-env-changed=BASALT_UPDATE_PREBUILT");

    let have_glslc = Command::new("glslc").arg("--version").output().is_ok();
    if !have_glslc {
        println!("cargo:warning=glslc not found, using prebuilt SPIR-V from shaders/prebuilt/");
    }

    for name in SHADERS {
        let src = shader_dir.join(name);
        let spv_name = format!("{name}.spv");
        let dst = out_dir.join(&spv_name);
        let prebuilt = prebuilt_dir.join(&spv_name);
        let hash_file = prebuilt_dir.join(format!("{spv_name}.hash"));
        println!("cargo:rerun-if-changed={}", src.display());
        println!("cargo:rerun-if-changed={}", prebuilt.display());
        println!("cargo:rerun-if-changed={}", hash_file.display());

        let source = std::fs::read_to_string(&src)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", src.display()));
        let hash = source_hash(&source);
        let prebuilt_current = prebuilt.exists()
            && std::fs::read_to_string(&hash_file).is_ok_and(|h| h.trim() == hash);

        let use_prebuilt = |reason: &str| {
            if !prebuilt_current {
                println!(
                    "cargo:warning={name}: {reason}, and shaders/prebuilt/{spv_name} is stale \
                     (built from different GLSL); your shader edits are NOT in this build"
                );
            }
            std::fs::copy(&prebuilt, &dst).unwrap_or_else(|e| {
                panic!(
                    "{reason}, and no prebuilt shader {}: {e}",
                    prebuilt.display()
                )
            });
        };

        if !have_glslc {
            use_prebuilt("glslc not found");
            continue;
        }

        let output = Command::new("glslc")
            .arg("-O")
            .arg("--target-env=vulkan1.3")
            .arg(&src)
            .arg("-o")
            .arg(&dst)
            .output()
            .expect("failed to run glslc");
        if output.status.success() {
            if update_prebuilt {
                std::fs::create_dir_all(&prebuilt_dir).unwrap();
                std::fs::copy(&dst, &prebuilt).unwrap();
                std::fs::write(&hash_file, format!("{hash}\n")).unwrap();
            }
            continue;
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if prebuilt_current {
            // Source unchanged: most likely an old glslc on PATH (e.g. no
            // vulkan1.3 target). The committed SPIR-V is exactly what we want.
            let first = stderr.lines().next().unwrap_or("").trim();
            println!(
                "cargo:warning={name}: glslc failed ({first}); source unchanged, using prebuilt SPIR-V"
            );
            std::fs::copy(&prebuilt, &dst).unwrap();
        } else {
            // The GLSL was edited, so the prebuilt copy is stale: the error
            // is real and must not be hidden.
            panic!("glslc failed to compile {}:\n{stderr}", src.display());
        }
    }
}
