//! Userdata types exposed to Lua: `vec3` and `Entity`.

use std::rc::Rc;

use glam::{Mat4, Quat, Vec3};
use mlua::prelude::*;
use mlua::{MetaMethod, UserData, UserDataFields, UserDataMethods, Value};

use super::Shared;
use crate::scene::{euler_deg_to_quat, quat_to_euler_deg, Entity, EntityId};
use crate::vfs::GameFs;

pub fn shared(lua: &Lua) -> Rc<Shared> {
    lua.app_data_ref::<Rc<Shared>>()
        .expect("engine state missing")
        .clone()
}

// ---------------------------------------------------------------------------
// vec3
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct LuaVec3(pub Vec3);

fn number(v: &Value) -> Option<f32> {
    match v {
        Value::Integer(i) => Some(*i as f32),
        Value::Number(n) => Some(*n as f32),
        _ => None,
    }
}

impl FromLua for LuaVec3 {
    fn from_lua(value: Value, _: &Lua) -> LuaResult<Self> {
        match value {
            Value::UserData(ud) => Ok(*ud.borrow::<LuaVec3>()?),
            // Accept {x=, y=, z=} and {1, 2, 3} tables too.
            Value::Table(t) => {
                let get = |named: &str, index: i64| -> LuaResult<f32> {
                    let v: Option<f32> = t.get(named)?;
                    Ok(match v {
                        Some(v) => v,
                        None => t.get::<Option<f32>>(index)?.unwrap_or(0.0),
                    })
                };
                Ok(LuaVec3(Vec3::new(get("x", 1)?, get("y", 2)?, get("z", 3)?)))
            }
            other => Err(LuaError::runtime(format!(
                "expected vec3, got {}",
                other.type_name()
            ))),
        }
    }
}

enum Operand {
    Vec(Vec3),
    Num(f32),
}

fn operand(v: Value) -> LuaResult<Operand> {
    if let Some(n) = number(&v) {
        return Ok(Operand::Num(n));
    }
    match v {
        Value::UserData(ud) => Ok(Operand::Vec(ud.borrow::<LuaVec3>()?.0)),
        other => Err(LuaError::runtime(format!(
            "cannot do vec3 math with {}",
            other.type_name()
        ))),
    }
}

fn arith(a: Value, b: Value, op: fn(Vec3, Vec3) -> Vec3) -> LuaResult<LuaVec3> {
    let v = match (operand(a)?, operand(b)?) {
        (Operand::Vec(a), Operand::Vec(b)) => op(a, b),
        (Operand::Vec(a), Operand::Num(b)) => op(a, Vec3::splat(b)),
        (Operand::Num(a), Operand::Vec(b)) => op(Vec3::splat(a), b),
        (Operand::Num(_), Operand::Num(_)) => unreachable!(),
    };
    Ok(LuaVec3(v))
}

impl UserData for LuaVec3 {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("x", |_, v| Ok(v.0.x));
        f.add_field_method_get("y", |_, v| Ok(v.0.y));
        f.add_field_method_get("z", |_, v| Ok(v.0.z));
        f.add_field_method_set("x", |_, v, x: f32| {
            v.0.x = x;
            Ok(())
        });
        f.add_field_method_set("y", |_, v, y: f32| {
            v.0.y = y;
            Ok(())
        });
        f.add_field_method_set("z", |_, v, z: f32| {
            v.0.z = z;
            Ok(())
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("length", |_, v, ()| Ok(v.0.length()));
        m.add_method("length_squared", |_, v, ()| Ok(v.0.length_squared()));
        m.add_method("normalized", |_, v, ()| {
            Ok(LuaVec3(v.0.normalize_or_zero()))
        });
        m.add_method("dot", |_, v, o: LuaVec3| Ok(v.0.dot(o.0)));
        m.add_method("cross", |_, v, o: LuaVec3| Ok(LuaVec3(v.0.cross(o.0))));
        m.add_method("distance", |_, v, o: LuaVec3| Ok(v.0.distance(o.0)));
        m.add_method("lerp", |_, v, (o, t): (LuaVec3, f32)| {
            Ok(LuaVec3(v.0.lerp(o.0, t)))
        });
        m.add_method("copy", |_, v, ()| Ok(*v));
        m.add_method("unpack", |_, v, ()| Ok((v.0.x, v.0.y, v.0.z)));

        m.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            arith(a, b, |a, b| a + b)
        });
        m.add_meta_function(MetaMethod::Sub, |_, (a, b): (Value, Value)| {
            arith(a, b, |a, b| a - b)
        });
        m.add_meta_function(MetaMethod::Mul, |_, (a, b): (Value, Value)| {
            arith(a, b, |a, b| a * b)
        });
        m.add_meta_function(MetaMethod::Div, |_, (a, b): (Value, Value)| {
            arith(a, b, |a, b| a / b)
        });
        m.add_meta_method(MetaMethod::Unm, |_, v, ()| Ok(LuaVec3(-v.0)));
        m.add_meta_method(MetaMethod::Eq, |_, v, o: LuaVec3| Ok(v.0 == o.0));
        m.add_meta_method(MetaMethod::ToString, |_, v, ()| {
            Ok(format!("vec3({}, {}, {})", v.0.x, v.0.y, v.0.z))
        });
    }
}

// ---------------------------------------------------------------------------
// Entity handle
// ---------------------------------------------------------------------------

/// A handle to an entity. Stays valid as a value after the entity is gone;
/// `entity:valid()` tells you if it still exists.
#[derive(Clone, Copy, Debug)]
pub struct LuaEntity(pub EntityId);

impl FromLua for LuaEntity {
    fn from_lua(value: Value, _: &Lua) -> LuaResult<Self> {
        match value {
            Value::UserData(ud) => Ok(*ud.borrow::<LuaEntity>()?),
            other => Err(LuaError::runtime(format!(
                "expected entity, got {}",
                other.type_name()
            ))),
        }
    }
}

fn with<R>(lua: &Lua, id: EntityId, f: impl FnOnce(&mut Entity) -> LuaResult<R>) -> LuaResult<R> {
    let shared = shared(lua);
    let mut world = shared.world.borrow_mut();
    match world.get_mut(id) {
        Some(e) => f(e),
        None => Err(LuaError::runtime("entity was destroyed")),
    }
}

fn look_rotation(dir: Vec3, up: Vec3) -> Option<Quat> {
    let dir = dir.try_normalize()?;
    let up = if dir.cross(up).length_squared() < 1e-6 {
        Vec3::Z
    } else {
        up
    };
    let (_, rot, _) = Mat4::look_to_rh(Vec3::ZERO, dir, up)
        .inverse()
        .to_scale_rotation_translation();
    Some(rot)
}

impl UserData for LuaEntity {
    fn add_fields<F: UserDataFields<Self>>(f: &mut F) {
        f.add_field_method_get("id", |_, e| Ok(e.0.key()));
        f.add_field_method_get("name", |lua, e| with(lua, e.0, |e| Ok(e.name.clone())));
        f.add_field_method_set("name", |lua, e, name: String| {
            with(lua, e.0, |e| {
                e.name = name;
                Ok(())
            })
        });

        f.add_field_method_get("position", |lua, e| {
            with(lua, e.0, |e| Ok(LuaVec3(e.transform.position)))
        });
        f.add_field_method_set("position", |lua, e, v: LuaVec3| {
            with(lua, e.0, |e| {
                e.transform.position = v.0;
                Ok(())
            })
        });

        // Euler angles in degrees (pitch, yaw, roll).
        f.add_field_method_get("rotation", |lua, e| {
            with(lua, e.0, |e| {
                Ok(LuaVec3(quat_to_euler_deg(e.transform.rotation)))
            })
        });
        f.add_field_method_set("rotation", |lua, e, v: LuaVec3| {
            with(lua, e.0, |e| {
                e.transform.rotation = euler_deg_to_quat(v.0);
                Ok(())
            })
        });

        f.add_field_method_get("scale", |lua, e| {
            with(lua, e.0, |e| Ok(LuaVec3(e.transform.scale)))
        });
        f.add_field_method_set("scale", |lua, e, v: Value| {
            let s = match number(&v) {
                Some(n) => Vec3::splat(n),
                None => LuaVec3::from_lua(v, lua)?.0,
            };
            with(lua, e.0, |e| {
                e.transform.scale = s;
                Ok(())
            })
        });

        f.add_field_method_get("model", |lua, e| with(lua, e.0, |e| Ok(e.model.clone())));
        f.add_field_method_set("model", |lua, e, model: Option<String>| {
            let model = model
                .as_deref()
                .map(GameFs::normalize)
                .transpose()
                .map_err(LuaError::runtime)?;
            with(lua, e.0, |e| {
                e.model = model;
                Ok(())
            })
        });

        f.add_field_method_get("visible", |lua, e| with(lua, e.0, |e| Ok(e.visible)));
        f.add_field_method_set("visible", |lua, e, v: bool| {
            with(lua, e.0, |e| {
                e.visible = v;
                Ok(())
            })
        });

        f.add_field_method_get("script_path", |lua, e| {
            with(lua, e.0, |e| Ok(e.script.clone()))
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("valid", |lua, e, ()| {
            let shared = shared(lua);
            let world = shared.world.borrow();
            Ok(world.get(e.0).is_some_and(|e| !e.pending_destroy))
        });
        m.add_method("destroy", |lua, e, ()| {
            let shared = shared(lua);
            if let Some(ent) = shared.world.borrow_mut().get_mut(e.0) {
                ent.pending_destroy = true;
            }
            Ok(())
        });

        m.add_method("translate", |lua, e, v: LuaVec3| {
            with(lua, e.0, |e| {
                e.transform.position += v.0;
                Ok(())
            })
        });
        m.add_method("translate_local", |lua, e, v: LuaVec3| {
            with(lua, e.0, |e| {
                e.transform.position += e.transform.rotation * v.0;
                Ok(())
            })
        });
        // World-space axis.
        m.add_method("rotate", |lua, e, (axis, deg): (LuaVec3, f32)| {
            let axis = axis
                .0
                .try_normalize()
                .ok_or_else(|| LuaError::runtime("zero axis"))?;
            with(lua, e.0, |e| {
                e.transform.rotation = (Quat::from_axis_angle(axis, deg.to_radians())
                    * e.transform.rotation)
                    .normalize();
                Ok(())
            })
        });
        // Local-space axis.
        m.add_method("rotate_local", |lua, e, (axis, deg): (LuaVec3, f32)| {
            let axis = axis
                .0
                .try_normalize()
                .ok_or_else(|| LuaError::runtime("zero axis"))?;
            with(lua, e.0, |e| {
                e.transform.rotation = (e.transform.rotation
                    * Quat::from_axis_angle(axis, deg.to_radians()))
                .normalize();
                Ok(())
            })
        });
        for (name, axis) in [
            ("rotate_x", Vec3::X),
            ("rotate_y", Vec3::Y),
            ("rotate_z", Vec3::Z),
        ] {
            m.add_method(name, move |lua, e, deg: f32| {
                with(lua, e.0, |e| {
                    e.transform.rotation = (e.transform.rotation
                        * Quat::from_axis_angle(axis, deg.to_radians()))
                    .normalize();
                    Ok(())
                })
            });
        }
        m.add_method(
            "look_at",
            |lua, e, (target, up): (LuaVec3, Option<LuaVec3>)| {
                with(lua, e.0, |e| {
                    let up = up.map(|u| u.0).unwrap_or(Vec3::Y);
                    if let Some(rot) = look_rotation(target.0 - e.transform.position, up) {
                        e.transform.rotation = rot;
                    }
                    Ok(())
                })
            },
        );
        m.add_method("forward", |lua, e, ()| {
            with(lua, e.0, |e| Ok(LuaVec3(e.transform.forward())))
        });
        m.add_method("right", |lua, e, ()| {
            with(lua, e.0, |e| Ok(LuaVec3(e.transform.right())))
        });
        m.add_method("up", |lua, e, ()| {
            with(lua, e.0, |e| Ok(LuaVec3(e.transform.up())))
        });

        // The script instance table attached to this entity (or nil).
        m.add_method("script", |lua, e, ()| {
            let instances: LuaTable = lua.named_registry_value(super::INSTANCES_KEY)?;
            instances.get::<Value>(e.0.key())
        });

        m.add_meta_method(MetaMethod::Eq, |_, e, o: LuaEntity| Ok(e.0 == o.0));
        m.add_meta_method(MetaMethod::ToString, |lua, e, ()| {
            let shared = shared(lua);
            let world = shared.world.borrow();
            Ok(match world.get(e.0) {
                Some(ent) => format!("Entity({})", ent.name),
                None => "Entity(<destroyed>)".to_string(),
            })
        });
    }
}
