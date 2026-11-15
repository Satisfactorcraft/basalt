//! Global Lua API: `vec3`, `scene`, `input`, `time`, `game`, `log`, `json`.

use glam::Vec3;
use mlua::prelude::*;
use mlua::{LuaSerdeExt, SerializeOptions, Value, Variadic};

use super::types::{shared, LuaEntity, LuaVec3};
use crate::scene::{euler_deg_to_quat, Camera, Entity};
use crate::vfs::GameFs;

fn rt<E: std::fmt::Display>(e: E) -> LuaError {
    LuaError::runtime(e.to_string())
}

pub fn json_options() -> SerializeOptions {
    SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false)
}

fn values_to_string(lua: &Lua, args: Variadic<Value>) -> LuaResult<String> {
    let tostring: LuaFunction = lua.globals().get("tostring")?;
    let mut parts = Vec::with_capacity(args.len());
    for v in args {
        parts.push(tostring.call::<String>(v)?);
    }
    Ok(parts.join("\t"))
}

pub fn install(lua: &Lua) -> LuaResult<()> {
    let g = lua.globals();

    // -- vec3 ---------------------------------------------------------------
    g.set(
        "vec3",
        lua.create_function(|_, (x, y, z): (Option<f32>, Option<f32>, Option<f32>)| {
            let x = x.unwrap_or(0.0);
            // vec3(5) == vec3(5, 5, 5)
            let (y, z) = match (y, z) {
                (None, None) => (x, x),
                (y, z) => (y.unwrap_or(0.0), z.unwrap_or(0.0)),
            };
            Ok(LuaVec3(Vec3::new(x, y, z)))
        })?,
    )?;

    // -- log / print --------------------------------------------------------
    let log = lua.create_table()?;
    log.set(
        "info",
        lua.create_function(|lua, a: Variadic<Value>| {
            log::info!(target: "lua", "{}", values_to_string(lua, a)?);
            Ok(())
        })?,
    )?;
    log.set(
        "warn",
        lua.create_function(|lua, a: Variadic<Value>| {
            log::warn!(target: "lua", "{}", values_to_string(lua, a)?);
            Ok(())
        })?,
    )?;
    log.set(
        "error",
        lua.create_function(|lua, a: Variadic<Value>| {
            log::error!(target: "lua", "{}", values_to_string(lua, a)?);
            Ok(())
        })?,
    )?;
    log.set(
        "debug",
        lua.create_function(|lua, a: Variadic<Value>| {
            log::debug!(target: "lua", "{}", values_to_string(lua, a)?);
            Ok(())
        })?,
    )?;
    g.set("print", log.get::<LuaFunction>("info")?)?;
    g.set("log", log)?;

    // -- time (fields are updated every frame by the host) ------------------
    let time = lua.create_table()?;
    time.set("dt", 0.0)?;
    time.set("elapsed", 0.0)?;
    time.set("frame", 0)?;
    g.set("time", time)?;

    // -- scene --------------------------------------------------------------
    let scene = lua.create_table()?;
    scene.set(
        "find",
        lua.create_function(|lua, name: String| {
            Ok(shared(lua).world.borrow().find(&name).map(LuaEntity))
        })?,
    )?;
    scene.set(
        "all",
        lua.create_function(|lua, ()| {
            let s = shared(lua);
            let w = s.world.borrow();
            Ok(w.iter()
                .filter(|(_, e)| !e.pending_destroy)
                .map(|(id, _)| LuaEntity(id))
                .collect::<Vec<_>>())
        })?,
    )?;
    scene.set(
        "camera",
        lua.create_function(|lua, ()| {
            let s = shared(lua);
            let w = s.world.borrow();
            let cam = w
                .iter()
                .find(|(_, e)| e.camera.is_some() && !e.pending_destroy)
                .map(|(id, _)| LuaEntity(id));
            Ok(cam)
        })?,
    )?;
    scene.set("spawn", lua.create_function(spawn)?)?;
    scene.set(
        "load_level",
        lua.create_function(|lua, path: String| {
            let path = GameFs::normalize(&path).map_err(rt)?;
            shared(lua).requests.borrow_mut().load_level = Some(path);
            Ok(())
        })?,
    )?;
    scene.set(
        "reload_level",
        lua.create_function(|lua, ()| {
            let s = shared(lua);
            let level = s.world.borrow().level_path.clone();
            s.requests.borrow_mut().load_level = level;
            Ok(())
        })?,
    )?;
    scene.set(
        "level",
        lua.create_function(|lua, ()| Ok(shared(lua).world.borrow().level_path.clone()))?,
    )?;
    scene.set(
        "set_clear_color",
        lua.create_function(|lua, (r, g, b, a): (f32, f32, f32, Option<f32>)| {
            shared(lua).world.borrow_mut().environment.clear_color = [r, g, b, a.unwrap_or(1.0)];
            Ok(())
        })?,
    )?;
    scene.set(
        "set_sun",
        lua.create_function(|lua, (dir, color): (LuaVec3, Option<LuaVec3>)| {
            let s = shared(lua);
            let mut w = s.world.borrow_mut();
            w.environment.sun_direction = dir.0.try_normalize().unwrap_or(Vec3::NEG_Y);
            if let Some(c) = color {
                w.environment.sun_color = c.0;
            }
            Ok(())
        })?,
    )?;
    scene.set(
        "set_ambient",
        lua.create_function(|lua, color: LuaVec3| {
            shared(lua).world.borrow_mut().environment.ambient = color.0;
            Ok(())
        })?,
    )?;
    g.set("scene", scene)?;

    // -- input --------------------------------------------------------------
    let input = lua.create_table()?;
    input.set(
        "key_down",
        lua.create_function(|lua, k: String| Ok(shared(lua).input.borrow().key_down(&k)))?,
    )?;
    input.set(
        "key_pressed",
        lua.create_function(|lua, k: String| Ok(shared(lua).input.borrow().key_pressed(&k)))?,
    )?;
    input.set(
        "key_released",
        lua.create_function(|lua, k: String| Ok(shared(lua).input.borrow().key_released(&k)))?,
    )?;
    input.set(
        "mouse_down",
        lua.create_function(|lua, b: String| Ok(shared(lua).input.borrow().mouse_down(&b)))?,
    )?;
    input.set(
        "mouse_pressed",
        lua.create_function(|lua, b: String| Ok(shared(lua).input.borrow().mouse_pressed(&b)))?,
    )?;
    input.set(
        "mouse_released",
        lua.create_function(|lua, b: String| Ok(shared(lua).input.borrow().mouse_released(&b)))?,
    )?;
    input.set(
        "mouse_position",
        lua.create_function(|lua, ()| Ok(shared(lua).input.borrow().mouse_position()))?,
    )?;
    input.set(
        "mouse_delta",
        lua.create_function(|lua, ()| Ok(shared(lua).input.borrow().mouse_delta()))?,
    )?;
    input.set(
        "scroll",
        lua.create_function(|lua, ()| Ok(shared(lua).input.borrow().scroll()))?,
    )?;
    input.set(
        "lock_cursor",
        lua.create_function(|lua, locked: bool| {
            let s = shared(lua);
            s.cursor_locked.set(locked);
            s.requests.borrow_mut().cursor_lock = Some(locked);
            Ok(())
        })?,
    )?;
    input.set(
        "cursor_locked",
        lua.create_function(|lua, ()| Ok(shared(lua).cursor_locked.get()))?,
    )?;
    g.set("input", input)?;

    // -- game ---------------------------------------------------------------
    let game = lua.create_table()?;
    game.set(
        "quit",
        lua.create_function(|lua, ()| {
            shared(lua).requests.borrow_mut().quit = true;
            Ok(())
        })?,
    )?;
    game.set(
        "read_file",
        lua.create_function(|lua, path: String| {
            shared(lua)
                .fs
                .read_string(&path)
                .map_err(|e| LuaError::runtime(format!("{e:#}")))
        })?,
    )?;
    game.set(
        "file_exists",
        lua.create_function(|lua, path: String| Ok(shared(lua).fs.exists(&path)))?,
    )?;
    game.set(
        "list_dir",
        lua.create_function(|lua, path: Option<String>| {
            shared(lua)
                .fs
                .list_dir(path.as_deref().unwrap_or(""))
                .map_err(|e| LuaError::runtime(format!("{e:#}")))
        })?,
    )?;
    game.set(
        "window_size",
        lua.create_function(|lua, ()| Ok(shared(lua).window_size.get()))?,
    )?;
    game.set(
        "set_title",
        lua.create_function(|lua, title: String| {
            shared(lua).requests.borrow_mut().set_title = Some(title);
            Ok(())
        })?,
    )?;
    g.set("game", game)?;

    // -- json ---------------------------------------------------------------
    let json = lua.create_table()?;
    json.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let v: serde_json::Value = serde_json::from_str(&text).map_err(rt)?;
            lua.to_value_with(&v, json_options())
        })?,
    )?;
    json.set(
        "encode",
        lua.create_function(|lua, (v, pretty): (Value, Option<bool>)| {
            let v: serde_json::Value = lua.from_value(v)?;
            if pretty.unwrap_or(false) {
                serde_json::to_string_pretty(&v).map_err(rt)
            } else {
                serde_json::to_string(&v).map_err(rt)
            }
        })?,
    )?;
    g.set("json", json)?;

    Ok(())
}

/// `scene.spawn{ name=, model=, position=, rotation=, scale=, visible=, camera=, script=, params= }`
#[allow(clippy::field_reassign_with_default)]
fn spawn(lua: &Lua, def: LuaTable) -> LuaResult<LuaEntity> {
    let mut e = Entity::default();
    e.name = def.get::<Option<String>>("name")?.unwrap_or_default();
    if let Some(p) = def.get::<Option<LuaVec3>>("position")? {
        e.transform.position = p.0;
    }
    if let Some(r) = def.get::<Option<LuaVec3>>("rotation")? {
        e.transform.rotation = euler_deg_to_quat(r.0);
    }
    match def.get::<Value>("scale")? {
        Value::Nil => {}
        Value::Integer(i) => e.transform.scale = Vec3::splat(i as f32),
        Value::Number(n) => e.transform.scale = Vec3::splat(n as f32),
        v => e.transform.scale = LuaVec3::from_lua(v, lua)?.0,
    }
    e.visible = def.get::<Option<bool>>("visible")?.unwrap_or(true);
    e.model = def
        .get::<Option<String>>("model")?
        .as_deref()
        .map(GameFs::normalize)
        .transpose()
        .map_err(rt)?;
    e.script = def
        .get::<Option<String>>("script")?
        .as_deref()
        .map(GameFs::normalize)
        .transpose()
        .map_err(rt)?;
    if let Some(cam) = def.get::<Option<LuaTable>>("camera")? {
        let d = Camera::default();
        e.camera = Some(Camera {
            fov: cam.get::<Option<f32>>("fov")?.unwrap_or(d.fov),
            near: cam.get::<Option<f32>>("near")?.unwrap_or(d.near),
            far: cam.get::<Option<f32>>("far")?.unwrap_or(d.far),
        });
    }
    match def.get::<Value>("params")? {
        Value::Nil => {}
        v => e.params = lua.from_value(v)?,
    }

    let has_script = e.script.is_some();
    let s = shared(lua);
    let id = s.world.borrow_mut().spawn(e);
    if has_script {
        s.requests.borrow_mut().new_scripts.push(id);
    }
    Ok(LuaEntity(id))
}
