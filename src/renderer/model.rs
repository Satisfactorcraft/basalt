//! glTF 2.0 model loading (.gltf + .bin + images, or .glb).

use std::path::Path;

use anyhow::{bail, Context, Result};
use ash::vk;
use glam::{Mat4, Vec3, Vec4};
use log::warn;

use super::context::GpuContext;
use super::resources::{Buffer, Image2d, Texture};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
}

pub struct Primitive {
    pub first_index: u32,
    pub index_count: u32,
    pub material: usize,
    /// Node transform inside the model.
    pub transform: Mat4,
}

pub struct CpuMaterial {
    pub base_color: Vec4,
    /// Fragments with alpha below this are discarded. Negative = opaque
    /// (alpha ignored), per glTF `alphaMode: OPAQUE`.
    pub alpha_cutoff: f32,
    /// Render both faces (no back-face culling).
    pub double_sided: bool,
    /// Index into `CpuModel::images`.
    pub image: Option<usize>,
}

pub struct CpuModel {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    pub primitives: Vec<Primitive>,
    pub materials: Vec<CpuMaterial>,
    pub images: Vec<Image2d>,
}

pub struct GpuMaterial {
    pub set: vk::DescriptorSet,
    pub base_color: Vec4,
    pub alpha_cutoff: f32,
    pub double_sided: bool,
}

pub struct GpuModel {
    pub vertex_buffer: Buffer,
    pub index_buffer: Buffer,
    pub primitives: Vec<Primitive>,
    pub materials: Vec<GpuMaterial>,
    pub textures: Vec<Texture>,
}

impl GpuModel {
    pub fn destroy(self, ctx: &mut GpuContext, pool: vk::DescriptorPool) {
        let sets: Vec<vk::DescriptorSet> = self.materials.iter().map(|m| m.set).collect();
        if !sets.is_empty() {
            unsafe {
                let _ = ctx.device.free_descriptor_sets(pool, &sets);
            }
        }
        ctx.destroy_buffer(self.vertex_buffer);
        ctx.destroy_buffer(self.index_buffer);
        for t in self.textures {
            ctx.destroy_texture(t);
        }
    }
}

pub fn load_gltf(path: &Path) -> Result<CpuModel> {
    let (doc, buffers, images) =
        gltf::import(path).with_context(|| format!("failed to load glTF {}", path.display()))?;

    let mut model = CpuModel {
        vertices: Vec::new(),
        indices: Vec::new(),
        primitives: Vec::new(),
        materials: Vec::new(),
        images: Vec::new(),
    };

    // Only base color textures are used in v1; upload just those.
    let mut image_map: Vec<Option<usize>> = vec![None; images.len()];
    for mat in doc.materials() {
        let pbr = mat.pbr_metallic_roughness();
        let image = pbr
            .base_color_texture()
            .map(|info| info.texture().source().index());
        let image = image.and_then(|i| {
            if image_map[i].is_none() {
                match to_rgba8(&images[i]) {
                    Ok(img) => {
                        image_map[i] = Some(model.images.len());
                        model.images.push(img);
                    }
                    Err(e) => warn!("{}: image {i}: {e}", path.display()),
                }
            }
            image_map[i]
        });
        let alpha_cutoff = match mat.alpha_mode() {
            gltf::material::AlphaMode::Opaque => -1.0,
            gltf::material::AlphaMode::Mask => mat.alpha_cutoff().unwrap_or(0.5),
            gltf::material::AlphaMode::Blend => {
                warn!(
                    "{}: material '{}' uses alphaMode BLEND, which isn't supported yet; rendering it alpha-tested at 0.5",
                    path.display(),
                    mat.name().unwrap_or("?")
                );
                0.5
            }
        };
        model.materials.push(CpuMaterial {
            base_color: Vec4::from(pbr.base_color_factor()),
            alpha_cutoff,
            double_sided: mat.double_sided(),
            image,
        });
    }
    let default_material = model.materials.len();
    model.materials.push(CpuMaterial {
        base_color: Vec4::ONE,
        alpha_cutoff: -1.0,
        double_sided: false,
        image: None,
    });

    let scene = doc.default_scene().or_else(|| doc.scenes().next());
    let Some(scene) = scene else {
        bail!("{}: glTF has no scene", path.display())
    };
    for node in scene.nodes() {
        visit_node(
            &node,
            Mat4::IDENTITY,
            &buffers,
            default_material,
            &mut model,
            path,
        );
    }
    if model.primitives.is_empty() {
        bail!("{}: glTF contains no triangle meshes", path.display());
    }
    Ok(model)
}

fn visit_node(
    node: &gltf::Node,
    parent: Mat4,
    buffers: &[gltf::buffer::Data],
    default_material: usize,
    model: &mut CpuModel,
    path: &Path,
) {
    let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                warn!("{}: skipping non-triangle primitive", path.display());
                continue;
            }
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|n| n.collect());
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|t| t.into_f32().collect());
            let indices: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            if indices.iter().any(|&i| i as usize >= positions.len()) {
                warn!(
                    "{}: primitive has out-of-range indices, skipping",
                    path.display()
                );
                continue;
            }
            let normals = normals.unwrap_or_else(|| compute_normals(&positions, &indices));

            let base = model.vertices.len() as u32;
            for (i, p) in positions.iter().enumerate() {
                model.vertices.push(Vertex {
                    position: *p,
                    normal: normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]),
                    uv: uvs
                        .as_ref()
                        .and_then(|u| u.get(i).copied())
                        .unwrap_or([0.0, 0.0]),
                });
            }
            let first_index = model.indices.len() as u32;
            model.indices.extend(indices.iter().map(|i| i + base));
            model.primitives.push(Primitive {
                first_index,
                index_count: indices.len() as u32,
                material: prim.material().index().unwrap_or(default_material),
                transform,
            });
        }
    }
    for child in node.children() {
        visit_node(&child, transform, buffers, default_material, model, path);
    }
}

fn compute_normals(positions: &[[f32; 3]], indices: &[u32]) -> Vec<[f32; 3]> {
    let mut normals = vec![Vec3::ZERO; positions.len()];
    for tri in indices.chunks_exact(3) {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        let (pa, pb, pc) = (
            Vec3::from(positions[a]),
            Vec3::from(positions[b]),
            Vec3::from(positions[c]),
        );
        let n = (pb - pa).cross(pc - pa);
        normals[a] += n;
        normals[b] += n;
        normals[c] += n;
    }
    normals
        .into_iter()
        .map(|n| n.normalize_or(Vec3::Y).into())
        .collect()
}

fn to_rgba8(img: &gltf::image::Data) -> Result<Image2d> {
    use gltf::image::Format;
    let px = &img.pixels;
    let n = (img.width * img.height) as usize;
    let rgba: Vec<u8> = match img.format {
        Format::R8G8B8A8 => px.clone(),
        Format::R8G8B8 => px
            .chunks_exact(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        // Two channels = luminance + alpha (gray PNGs with transparency).
        Format::R8G8 => px
            .chunks_exact(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        Format::R8 => px.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        // 16-bit: keep the high byte (little endian).
        Format::R16G16B16A16 => px
            .chunks_exact(8)
            .flat_map(|c| [c[1], c[3], c[5], c[7]])
            .collect(),
        Format::R16G16B16 => px
            .chunks_exact(6)
            .flat_map(|c| [c[1], c[3], c[5], 255])
            .collect(),
        Format::R16G16 => px
            .chunks_exact(4)
            .flat_map(|c| [c[1], c[1], c[1], c[3]])
            .collect(),
        Format::R16 => px
            .chunks_exact(2)
            .flat_map(|c| [c[1], c[1], c[1], 255])
            .collect(),
        other => bail!("unsupported image format {other:?}"),
    };
    if rgba.len() != n * 4 {
        bail!("image data size mismatch");
    }
    Ok(Image2d {
        width: img.width,
        height: img.height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use crate::vfs::GameFs;

    /// Loads the demo models through GameFs the way the renderer does. The
    /// crate's texture is referenced as `../textures/crate.png`, which is what
    /// broke with verbatim `\\?\` roots on Windows (#3).
    #[test]
    fn demo_models_load_with_relative_textures() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("game");
        let fs = GameFs::new(&root).unwrap();
        assert!(!fs.root().to_string_lossy().starts_with(r"\\?\"));
        for name in ["crate", "ground", "gem"] {
            let model = super::load_gltf(&fs.resolve(&format!("models/{name}.gltf")).unwrap())
                .unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(!model.primitives.is_empty());
        }
        let crate_model = super::load_gltf(&fs.resolve("models/crate.gltf").unwrap()).unwrap();
        assert_eq!(crate_model.images.len(), 1, "crate texture should load");
        assert!(crate_model.materials[0].image.is_some());
    }

    #[test]
    fn gray_textures_expand_to_rgba() {
        let img = |format, pixels: Vec<u8>| gltf::image::Data {
            pixels,
            format,
            width: 1,
            height: 1,
        };
        use gltf::image::Format;
        let cases = [
            (img(Format::R8G8, vec![200, 100]), [200, 200, 200, 100]),
            (img(Format::R8, vec![50]), [50, 50, 50, 255]),
            // 16-bit, little endian: high byte wins.
            (
                img(Format::R16G16, vec![0, 200, 0, 100]),
                [200, 200, 200, 100],
            ),
            (img(Format::R16, vec![0, 50]), [50, 50, 50, 255]),
        ];
        for (data, want) in cases {
            assert_eq!(
                super::to_rgba8(&data).unwrap().rgba,
                want,
                "{:?}",
                data.format
            );
        }
    }
}
