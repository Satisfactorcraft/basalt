//! Entities, transforms and level files.

use anyhow::{bail, Context, Result};
use glam::{EulerRot, Mat4, Quat, Vec3};
use serde::Deserialize;

use crate::vfs::GameFs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityId {
    index: u32,
    generation: u32,
}

impl EntityId {
    /// Stable integer key, handy for Lua tables.
    pub fn key(self) -> i64 {
        ((self.generation as i64) << 32) | self.index as i64
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Transform {
    pub position: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        }
    }
}

impl Transform {
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.position)
    }
    /// -Z is forward, +X is right, +Y is up (right-handed, glTF convention).
    pub fn forward(&self) -> Vec3 {
        self.rotation * Vec3::NEG_Z
    }
    pub fn right(&self) -> Vec3 {
        self.rotation * Vec3::X
    }
    pub fn up(&self) -> Vec3 {
        self.rotation * Vec3::Y
    }
}

/// Euler angles in degrees `[pitch(x), yaw(y), roll(z)]`, applied yaw then pitch then roll.
pub fn euler_deg_to_quat(e: Vec3) -> Quat {
    Quat::from_euler(
        EulerRot::YXZ,
        e.y.to_radians(),
        e.x.to_radians(),
        e.z.to_radians(),
    )
}

pub fn quat_to_euler_deg(q: Quat) -> Vec3 {
    let (y, x, z) = q.to_euler(EulerRot::YXZ);
    Vec3::new(x.to_degrees(), y.to_degrees(), z.to_degrees())
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Camera {
    pub fov: f32,
    pub near: f32,
    pub far: f32,
}

impl Camera {
    /// Bad values silently wreck the projection (near = 0 collapses depth),
    /// so reject them up front.
    pub fn validate(&self) -> Result<()> {
        if !(self.fov > 0.0 && self.fov < 180.0) {
            bail!(
                "camera fov must be between 0 and 180 degrees, got {}",
                self.fov
            );
        }
        if !(self.near > 0.0) {
            bail!("camera near must be greater than 0, got {}", self.near);
        }
        if !(self.far > self.near) {
            bail!(
                "camera far ({}) must be greater than near ({})",
                self.far,
                self.near
            );
        }
        Ok(())
    }
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            fov: 70.0,
            near: 0.1,
            far: 1000.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entity {
    pub name: String,
    pub transform: Transform,
    pub model: Option<String>,
    pub visible: bool,
    pub camera: Option<Camera>,
    pub script: Option<String>,
    pub params: serde_json::Value,
    pub pending_destroy: bool,
}

impl Default for Entity {
    fn default() -> Self {
        Self {
            name: String::new(),
            transform: Transform::default(),
            model: None,
            visible: true,
            camera: None,
            script: None,
            params: serde_json::Value::Null,
            pending_destroy: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Environment {
    pub clear_color: [f32; 4],
    /// Direction the sunlight travels in.
    pub sun_direction: Vec3,
    pub sun_color: Vec3,
    pub ambient: Vec3,
}

impl Default for Environment {
    fn default() -> Self {
        EnvironmentDef::default().into()
    }
}

struct Slot {
    generation: u32,
    entity: Option<Entity>,
}

#[derive(Default)]
pub struct World {
    slots: Vec<Slot>,
    free: Vec<u32>,
    spawn_counter: u64,
    pub environment: Environment,
    pub level_path: Option<String>,
}

impl World {
    pub fn spawn(&mut self, mut entity: Entity) -> EntityId {
        self.spawn_counter += 1;
        if entity.name.is_empty() {
            entity.name = format!("entity_{}", self.spawn_counter);
        }
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.entity = Some(entity);
            EntityId {
                index,
                generation: slot.generation,
            }
        } else {
            self.slots.push(Slot {
                generation: 0,
                entity: Some(entity),
            });
            EntityId {
                index: self.slots.len() as u32 - 1,
                generation: 0,
            }
        }
    }

    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        let slot = self.slots.get(id.index as usize)?;
        (slot.generation == id.generation)
            .then_some(slot.entity.as_ref())
            .flatten()
    }

    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        let slot = self.slots.get_mut(id.index as usize)?;
        (slot.generation == id.generation)
            .then_some(slot.entity.as_mut())
            .flatten()
    }

    pub fn despawn(&mut self, id: EntityId) -> Option<Entity> {
        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        let entity = slot.entity.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(id.index);
        Some(entity)
    }

    pub fn iter(&self) -> impl Iterator<Item = (EntityId, &Entity)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| {
            s.entity.as_ref().map(|e| {
                (
                    EntityId {
                        index: i as u32,
                        generation: s.generation,
                    },
                    e,
                )
            })
        })
    }

    pub fn ids(&self) -> Vec<EntityId> {
        self.iter().map(|(id, _)| id).collect()
    }

    pub fn find(&self, name: &str) -> Option<EntityId> {
        self.iter()
            .find(|(_, e)| e.name == name && !e.pending_destroy)
            .map(|(id, _)| id)
    }

    pub fn clear(&mut self) {
        for id in self.ids() {
            self.despawn(id);
        }
    }

    /// The first live entity with a camera component.
    pub fn active_camera(&self) -> Option<(Transform, Camera)> {
        self.iter().find_map(|(_, e)| {
            e.camera
                .filter(|_| !e.pending_destroy)
                .map(|c| (e.transform, c))
        })
    }
}

// ---------------------------------------------------------------------------
// Level files
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LevelDef {
    pub environment: EnvironmentDef,
    pub entities: Vec<EntityDef>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnvironmentDef {
    pub clear_color: [f32; 4],
    pub sun_direction: [f32; 3],
    pub sun_color: [f32; 3],
    pub sun_intensity: f32,
    pub ambient: [f32; 3],
}

impl Default for EnvironmentDef {
    fn default() -> Self {
        Self {
            clear_color: [0.05, 0.06, 0.08, 1.0],
            sun_direction: [-0.4, -1.0, -0.3],
            sun_color: [1.0, 0.97, 0.9],
            sun_intensity: 1.0,
            ambient: [0.15, 0.17, 0.22],
        }
    }
}

impl From<EnvironmentDef> for Environment {
    fn from(d: EnvironmentDef) -> Self {
        Self {
            clear_color: d.clear_color,
            sun_direction: Vec3::from(d.sun_direction)
                .try_normalize()
                .unwrap_or(Vec3::NEG_Y),
            sun_color: Vec3::from(d.sun_color) * d.sun_intensity,
            ambient: Vec3::from(d.ambient),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(untagged)]
pub enum ScaleDef {
    Uniform(f32),
    Xyz([f32; 3]),
}

impl From<ScaleDef> for Vec3 {
    fn from(s: ScaleDef) -> Self {
        match s {
            ScaleDef::Uniform(v) => Vec3::splat(v),
            ScaleDef::Xyz(v) => Vec3::from(v),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EntityDef {
    pub name: Option<String>,
    pub model: Option<String>,
    pub position: [f32; 3],
    /// Euler angles in degrees.
    pub rotation: [f32; 3],
    pub scale: ScaleDef,
    pub visible: bool,
    pub camera: Option<Camera>,
    pub script: Option<String>,
    pub params: serde_json::Value,
}

impl Default for EntityDef {
    fn default() -> Self {
        Self {
            name: None,
            model: None,
            position: [0.0; 3],
            rotation: [0.0; 3],
            scale: ScaleDef::Uniform(1.0),
            visible: true,
            camera: None,
            script: None,
            params: serde_json::Value::Null,
        }
    }
}

impl EntityDef {
    pub fn into_entity(self) -> Result<Entity> {
        if let Some(cam) = &self.camera {
            cam.validate().with_context(|| {
                format!("entity '{}'", self.name.as_deref().unwrap_or("<unnamed>"))
            })?;
        }
        Ok(Entity {
            name: self.name.unwrap_or_default(),
            transform: Transform {
                position: self.position.into(),
                rotation: euler_deg_to_quat(self.rotation.into()),
                scale: self.scale.into(),
            },
            model: self.model.as_deref().map(GameFs::normalize).transpose()?,
            visible: self.visible,
            camera: self.camera,
            script: self.script.as_deref().map(GameFs::normalize).transpose()?,
            params: self.params,
            pending_destroy: false,
        })
    }
}

impl LevelDef {
    pub fn load(fs: &GameFs, path: &str) -> Result<Self> {
        let text = fs.read_string(path)?;
        serde_json::from_str(&text).with_context(|| format!("invalid level '{path}'"))
    }
}

#[cfg(test)]
mod tests {
    use super::Camera;

    #[test]
    fn camera_validation() {
        assert!(Camera::default().validate().is_ok());
        let bad = [
            Camera {
                near: 0.0,
                ..Default::default()
            },
            Camera {
                near: -1.0,
                ..Default::default()
            },
            Camera {
                near: 10.0,
                far: 5.0,
                ..Default::default()
            },
            Camera {
                fov: 0.0,
                ..Default::default()
            },
            Camera {
                fov: 180.0,
                ..Default::default()
            },
            Camera {
                near: f32::NAN,
                ..Default::default()
            },
        ];
        for cam in bad {
            assert!(cam.validate().is_err(), "{cam:?} should be rejected");
        }
    }
}
