//! Lua scripting host.
//!
//! A script is a Lua file that returns a table ("class"). Every entity with a
//! script gets its own instance table whose metatable falls back to the class,
//! so `self` is per-entity state and the class holds the functions:
//!
//! ```lua
//! local Spinner = {}
//! function Spinner:on_start() end
//! function Spinner:on_update(dt) self.entity:rotate_y(90 * dt) end
//! function Spinner:on_destroy() end
//! function Spinner:on_reload() end   -- called after hot reload
//! return Spinner
//! ```
//!
//! Hot reload swaps the class under live instances, so `self` state survives.

mod api;
mod types;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use anyhow::{bail, Result};
use log::{error, info, warn};
use mlua::prelude::*;
use mlua::{LuaOptions, LuaSerdeExt, MultiValue, StdLib, Value};

use crate::input::Input;
use crate::scene::{EntityId, LevelDef, World};
use crate::vfs::GameFs;
use types::LuaEntity;

const INSTANCES_KEY: &str = "basalt.instances";

/// Things scripts ask the engine to do; handled by the app after the update.
#[derive(Default)]
pub struct Requests {
    pub load_level: Option<String>,
    pub quit: bool,
    pub cursor_lock: Option<bool>,
    pub set_title: Option<String>,
}

/// Engine state scripts can see. Shared between Rust and Lua callbacks.
pub struct Shared {
    pub fs: GameFs,
    pub world: RefCell<World>,
    pub input: RefCell<Input>,
    pub requests: RefCell<Requests>,
    pub window_size: Cell<(u32, u32)>,
    pub cursor_locked: Cell<bool>,
    /// Compiled script classes by path; `None` = failed to compile.
    /// Lives here (not in ScriptHost) so `scene.spawn` can build instances.
    classes: RefCell<HashMap<String, Option<LuaTable>>>,
    /// Instances created by `scene.spawn` mid-update, adopted by the host
    /// at the end of the update pass.
    pending: RefCell<Vec<(EntityId, Instance)>>,
}

impl Shared {
    pub fn new(fs: GameFs, window_size: (u32, u32)) -> Self {
        Self {
            fs,
            world: RefCell::new(World::default()),
            input: RefCell::new(Input::default()),
            requests: RefCell::new(Requests::default()),
            window_size: Cell::new(window_size),
            cursor_locked: Cell::new(false),
            classes: RefCell::new(HashMap::new()),
            pending: RefCell::new(Vec::new()),
        }
    }
}

struct Instance {
    script: String,
    table: LuaTable,
    meta: LuaTable,
    has_class: bool,
    started: bool,
    /// A hook errored; the instance is paused until the script is reloaded.
    failed: bool,
}

pub struct ScriptHost {
    lua: Lua,
    shared: Rc<Shared>,
    instances: HashMap<EntityId, Instance>,
    order: Vec<EntityId>,
    builtin_modules: HashSet<String>,
    /// (script, hook) errors already logged, so 50 entities sharing a broken
    /// script print it once. Cleared on reload.
    reported: HashSet<(String, String)>,
    elapsed: f64,
    frame: u64,
}

/// Returns the compiled class for a script, compiling it on first use.
/// No borrow is held while compiling: the script's top-level code may
/// itself spawn scripted entities.
fn load_class(lua: &Lua, shared: &Shared, path: &str) -> Option<LuaTable> {
    if let Some(c) = shared.classes.borrow().get(path) {
        return c.clone();
    }
    let class = match compile_class(lua, shared, path) {
        Ok(t) => Some(t),
        Err(e) => {
            error!("{e:#}");
            None
        }
    };
    shared
        .classes
        .borrow_mut()
        .insert(path.to_string(), class.clone());
    class
}

fn compile_class(lua: &Lua, shared: &Shared, path: &str) -> Result<LuaTable> {
    let source = shared.fs.read_string(path)?;
    let value: Value = lua.load(&source).set_name(format!("@{path}")).eval()?;
    match value {
        Value::Table(t) => Ok(t),
        other => bail!(
            "{path}: script must `return` a table, got {}",
            other.type_name()
        ),
    }
}

/// Builds an entity's script instance and registers it so
/// `entity:script()` works immediately. `params` is used as-is (any Lua
/// value); nil becomes an empty table.
fn new_instance(
    lua: &Lua,
    shared: &Shared,
    id: EntityId,
    script: String,
    name: String,
    params: Value,
) -> LuaResult<Instance> {
    let class = load_class(lua, shared, &script);
    let table = lua.create_table()?;
    table.set("entity", LuaEntity(id))?;
    table.set("name", name)?;
    let params = match params {
        Value::Nil => Value::Table(lua.create_table()?),
        p => p,
    };
    table.set("params", params)?;

    let meta = lua.create_table()?;
    let has_class = class.is_some();
    meta.set("__index", class.unwrap_or(lua.create_table()?))?;
    table.set_metatable(Some(meta.clone()))?;

    let instances: LuaTable = lua.named_registry_value(INSTANCES_KEY)?;
    instances.set(id.key(), table.clone())?;
    Ok(Instance {
        script,
        table,
        meta,
        has_class,
        started: false,
        failed: !has_class,
    })
}

/// `package.searchers` entry: resolves a module name inside `scripts/`.
/// Returns the compiled chunk plus its path, or a "not found" message.
fn search_module(lua: &Lua, name: String) -> LuaResult<MultiValue> {
    let shared = types::shared(lua);
    let base = name.replace('.', "/");
    let mut tried = Vec::new();
    for candidate in [
        format!("scripts/{base}.lua"),
        format!("scripts/{base}/init.lua"),
    ] {
        // normalize() rejects absolute paths and escaping the game folder;
        // also refuse anything that resolves outside scripts/.
        let Ok(path) = GameFs::normalize(&candidate) else {
            continue;
        };
        if !path.starts_with("scripts/") {
            continue;
        }
        if !shared.fs.exists(&path) {
            tried.push(format!("\n\tno file '{path}'"));
            continue;
        }
        let source = shared
            .fs
            .read_string(&path)
            .map_err(|e| LuaError::runtime(format!("{e:#}")))?;
        let chunk = lua
            .load(&source)
            .set_name(format!("@{path}"))
            .into_function()?;
        return (chunk, path).into_lua_multi(lua);
    }
    if tried.is_empty() {
        tried.push(format!("\n\tinvalid module name '{name}'"));
    }
    tried.concat().into_lua_multi(lua)
}

impl ScriptHost {
    pub fn new(shared: Rc<Shared>) -> Result<Self> {
        // No io/debug; os gets trimmed to the harmless bits below.
        let libs = StdLib::COROUTINE
            | StdLib::TABLE
            | StdLib::STRING
            | StdLib::UTF8
            | StdLib::MATH
            | StdLib::PACKAGE
            | StdLib::OS;
        let lua = Lua::new_with(libs, LuaOptions::default())?;
        lua.set_app_data(shared.clone());

        let g = lua.globals();
        g.set("dofile", Value::Nil)?;
        g.set("loadfile", Value::Nil)?;
        let os: LuaTable = g.get("os")?;
        for name in [
            "execute",
            "exit",
            "getenv",
            "remove",
            "rename",
            "tmpname",
            "setlocale",
        ] {
            os.set(name, Value::Nil)?;
        }

        // `require("foo.bar")` loads scripts/foo/bar.lua (or foo/bar/init.lua)
        // through GameFs. The stock searchers read package.path/cpath, which
        // scripts can rewrite to load files from anywhere, so they're replaced
        // outright and package.path is irrelevant.
        let package: LuaTable = g.get("package")?;
        let searchers = lua.create_table()?;
        searchers.raw_set(
            1,
            package.get::<LuaTable>("searchers")?.raw_get::<Value>(1)?,
        )?; // preload
        searchers.raw_set(2, lua.create_function(search_module)?)?;
        package.set("searchers", searchers)?;
        package.set("path", "")?;
        package.set("cpath", "")?;
        package.set("loadlib", Value::Nil)?;
        package.set("searchpath", Value::Nil)?;

        api::install(&lua)?;
        lua.set_named_registry_value(INSTANCES_KEY, lua.create_table()?)?;

        let loaded: LuaTable = package.get("loaded")?;
        let builtin_modules = loaded
            .pairs::<String, Value>()
            .filter_map(|p| p.ok().map(|(k, _)| k))
            .collect();

        Ok(Self {
            lua,
            shared,
            instances: HashMap::new(),
            order: Vec::new(),
            builtin_modules,
            reported: HashSet::new(),
            elapsed: 0.0,
            frame: 0,
        })
    }

    // -- levels ---------------------------------------------------------------

    /// Replaces the current scene with a level file. On error the current
    /// scene is left untouched.
    pub fn load_level(&mut self, path: &str) -> Result<()> {
        let path = GameFs::normalize(path)?;
        let def = LevelDef::load(&self.shared.fs, &path)?;
        let entities = def
            .entities
            .into_iter()
            .map(|d| d.into_entity())
            .collect::<Result<Vec<_>>>()?;
        let count = entities.len();

        self.unload_all();
        self.reported.clear();
        let ids: Vec<EntityId> = {
            let mut world = self.shared.world.borrow_mut();
            world.environment = def.environment.into();
            world.level_path = Some(path.clone());
            entities.into_iter().map(|e| world.spawn(e)).collect()
        };
        for id in ids {
            self.attach(id);
        }
        if self.shared.world.borrow().active_camera().is_none() {
            warn!("level '{path}' has no camera entity, using a default view");
        }
        info!("loaded level '{path}' ({count} entities)");
        Ok(())
    }

    fn unload_all(&mut self) {
        self.attach_pending();
        for id in self.order.clone() {
            self.destroy_hook(id);
        }
        self.instances.clear();
        self.order.clear();
        if let Ok(t) = self.lua.create_table() {
            let _ = self.lua.set_named_registry_value(INSTANCES_KEY, t);
        }
        self.shared.world.borrow_mut().clear();
        self.shared.pending.borrow_mut().clear();
    }

    /// on_destroy only pairs with an on_start that actually ran.
    fn destroy_hook(&mut self, id: EntityId) {
        if self.instances.get(&id).is_some_and(|i| i.started) {
            self.call(id, "on_destroy", ());
        }
    }

    // -- per-frame ------------------------------------------------------------

    pub fn update(&mut self, dt: f32) {
        self.attach_pending();
        self.elapsed += dt as f64;
        self.frame += 1;
        if let Ok(time) = self.lua.globals().get::<LuaTable>("time") {
            let _ = time.set("dt", dt);
            let _ = time.set("elapsed", self.elapsed);
            let _ = time.set("frame", self.frame);
        }

        self.order.retain(|id| self.instances.contains_key(id));
        for id in self.order.clone() {
            if !self.runnable(id) {
                continue;
            }
            let Some(inst) = self.instances.get_mut(&id) else {
                continue;
            };
            if !inst.started {
                inst.started = true;
                self.call(id, "on_start", ());
                // on_start may have errored or destroyed its own entity.
                if !self.runnable(id) {
                    continue;
                }
            }
            self.call(id, "on_update", dt);
        }

        self.attach_pending();
        self.flush_destroyed();
    }

    /// The entity is alive and its script instance isn't paused by an error.
    fn runnable(&self, id: EntityId) -> bool {
        let alive = self
            .shared
            .world
            .borrow()
            .get(id)
            .is_some_and(|e| !e.pending_destroy);
        alive && self.instances.get(&id).is_some_and(|i| !i.failed)
    }

    /// Adopts instances that `scene.spawn` created during the update.
    fn attach_pending(&mut self) {
        let pending = std::mem::take(&mut *self.shared.pending.borrow_mut());
        for (id, inst) in pending {
            self.instances.insert(id, inst);
            self.order.push(id);
        }
    }

    fn flush_destroyed(&mut self) {
        // on_destroy may destroy more entities; loop until settled.
        for _ in 0..16 {
            let doomed: Vec<EntityId> = {
                let w = self.shared.world.borrow();
                w.iter()
                    .filter(|(_, e)| e.pending_destroy)
                    .map(|(id, _)| id)
                    .collect()
            };
            if doomed.is_empty() {
                return;
            }
            for id in doomed {
                self.destroy_hook(id);
                self.detach(id);
                self.shared.world.borrow_mut().despawn(id);
            }
        }
    }

    // -- instances ------------------------------------------------------------

    fn attach(&mut self, id: EntityId) {
        if let Err(e) = self.try_attach(id) {
            error!("failed to attach script: {e:#}");
        }
    }

    fn try_attach(&mut self, id: EntityId) -> Result<()> {
        let (script, params, name) = {
            let w = self.shared.world.borrow();
            let Some(e) = w.get(id) else { return Ok(()) };
            let Some(script) = e.script.clone() else {
                return Ok(());
            };
            (script, e.params.clone(), e.name.clone())
        };
        let params = match params {
            serde_json::Value::Null => Value::Nil,
            p => self.lua.to_value_with(&p, api::json_options())?,
        };
        let inst = new_instance(&self.lua, &self.shared, id, script, name, params)?;
        self.instances.insert(id, inst);
        self.order.push(id);
        Ok(())
    }

    fn detach(&mut self, id: EntityId) {
        self.instances.remove(&id);
        if let Ok(instances) = self.lua.named_registry_value::<LuaTable>(INSTANCES_KEY) {
            let _ = instances.set(id.key(), Value::Nil);
        }
    }

    /// Calls `instance:hook(args...)` if the script defines it.
    fn call(&mut self, id: EntityId, hook: &str, args: impl IntoLuaMulti) {
        let Some(inst) = self.instances.get_mut(&id) else {
            return;
        };
        let result = (|| -> LuaResult<()> {
            let Some(func) = inst.table.get::<Option<LuaFunction>>(hook)? else {
                return Ok(());
            };
            let mut argv: MultiValue = args.into_lua_multi(&self.lua)?;
            argv.push_front(Value::Table(inst.table.clone()));
            func.call::<()>(argv)
        })();
        if let Err(e) = result {
            inst.failed = true;
            if self
                .reported
                .insert((inst.script.clone(), hook.to_string()))
            {
                error!("{}:{hook}: {e}\n  (paused every entity using this script until you fix it and save)", inst.script);
            }
        }
    }

    // -- classes --------------------------------------------------------------

    /// Recompiles every script in use and swaps it under live instances.
    pub fn reload_scripts(&mut self) {
        self.reported.clear();
        // Forget `require`d modules so they get re-read too.
        if let Ok(loaded) = self
            .lua
            .globals()
            .get::<LuaTable>("package")
            .and_then(|p| p.get::<LuaTable>("loaded"))
        {
            let keys: Vec<String> = loaded
                .pairs::<String, Value>()
                .filter_map(|p| p.ok().map(|(k, _)| k))
                .filter(|k| !self.builtin_modules.contains(k))
                .collect();
            for k in keys {
                let _ = loaded.set(k, Value::Nil);
            }
        }

        let paths: Vec<String> = self.shared.classes.borrow().keys().cloned().collect();
        let mut reloaded = 0;
        for path in paths {
            match compile_class(&self.lua, &self.shared, &path) {
                Ok(t) => {
                    self.shared.classes.borrow_mut().insert(path, Some(t));
                    reloaded += 1;
                }
                // Keep the old version running if the new one is broken.
                Err(e) => error!("hot reload: {e:#}"),
            }
        }

        let mut to_notify = Vec::new();
        let classes = self.shared.classes.borrow();
        for (id, inst) in self.instances.iter_mut() {
            let Some(Some(class)) = classes.get(&inst.script) else {
                continue;
            };
            if inst.meta.set("__index", class.clone()).is_err() {
                continue;
            }
            if !inst.has_class {
                inst.has_class = true;
                inst.started = false;
            }
            inst.failed = false;
            if inst.started {
                to_notify.push(*id);
            }
        }
        drop(classes);
        for id in to_notify {
            self.call(id, "on_reload", ());
        }
        info!("hot reload: {reloaded} script(s) reloaded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempGame(PathBuf);
    impl Drop for TempGame {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn game(name: &str, files: &[(&str, &str)]) -> (TempGame, ScriptHost) {
        let root = std::env::temp_dir().join(format!("basalt-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in files {
            let p = root.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let shared = Rc::new(Shared::new(GameFs::new(&root).unwrap(), (800, 600)));
        let host = ScriptHost::new(shared).unwrap();
        (TempGame(root), host)
    }

    fn global<T: FromLua>(host: &ScriptHost, name: &str) -> T {
        host.lua.globals().get(name).unwrap()
    }

    #[test]
    fn lifecycle_spawn_destroy_and_api() {
        let (_g, mut host) = game(
            "lifecycle",
            &[
                (
                    "levels/l.json",
                    r#"{"entities":[{"name":"boss","script":"scripts/boss.lua","params":{"hp":3}}]}"#,
                ),
                (
                    "scripts/lib/m.lua",
                    "return { double = function(x) return x * 2 end }",
                ),
                (
                    "scripts/boss.lua",
                    r#"
                local m = require("lib.m")
                local Boss = {}
                function Boss:on_start()
                  STARTED = (STARTED or 0) + 1
                  assert(self.params.hp == 3)
                  assert(m.double(21) == 42)
                  local v = vec3(1, 2, 3) * 2 + vec3(1)
                  assert(v.x == 3 and v.y == 5 and v.z == 7, tostring(v))
                  self.entity.position = v
                  assert(self.entity.position == vec3(3, 5, 7))
                  self.minion = scene.spawn{ name = "minion", script = "scripts/minion.lua", position = {1, 0, 0} }
                  assert(scene.find("minion") == self.minion)
                  assert(json.decode('{"a":[1,2]}').a[2] == 2)
                end
                function Boss:on_update(dt)
                  UPDATES = (UPDATES or 0) + 1
                  if UPDATES == 3 then self.minion:destroy() end
                end
                return Boss
            "#,
                ),
                (
                    "scripts/minion.lua",
                    r#"
                local Minion = {}
                function Minion:on_start() MINION_STARTED = true end
                function Minion:on_destroy() MINION_DESTROYED = true end
                return Minion
            "#,
                ),
            ],
        );
        host.load_level("levels/l.json").unwrap();
        for _ in 0..5 {
            host.update(1.0 / 60.0);
        }
        assert_eq!(global::<i64>(&host, "STARTED"), 1);
        assert_eq!(global::<i64>(&host, "UPDATES"), 5);
        assert!(global::<bool>(&host, "MINION_STARTED"));
        assert!(global::<bool>(&host, "MINION_DESTROYED"));
        let world = host.shared.world.borrow();
        assert!(world.find("minion").is_none());
        let boss = world.find("boss").unwrap();
        assert_eq!(
            world.get(boss).unwrap().transform.position,
            glam::Vec3::new(3.0, 5.0, 7.0)
        );
    }

    #[test]
    fn hot_reload_keeps_state_and_recovers_from_errors() {
        let (g, mut host) = game("reload", &[
            ("levels/l.json", r#"{"entities":[{"script":"scripts/c.lua"}]}"#),
            ("scripts/c.lua", "local C = {} function C:on_update() self.n = (self.n or 0) + 1; N = self.n end return C"),
        ]);
        host.load_level("levels/l.json").unwrap();
        host.update(0.016);
        host.update(0.016);
        assert_eq!(global::<i64>(&host, "N"), 2);

        // Runtime error pauses the instance...
        std::fs::write(
            g.0.join("scripts/c.lua"),
            "local C = {} function C:on_update() error('boom') end return C",
        )
        .unwrap();
        host.reload_scripts();
        host.update(0.016);
        assert!(host.instances.values().all(|i| i.failed));

        // ...a syntax error keeps the previous version...
        std::fs::write(g.0.join("scripts/c.lua"), "local C = {").unwrap();
        host.reload_scripts();

        // ...and a fix resumes it with `self` state intact.
        std::fs::write(
            g.0.join("scripts/c.lua"),
            "local C = {} function C:on_reload() RELOADED = true end function C:on_update() self.n = self.n + 10; N = self.n end return C",
        )
        .unwrap();
        host.reload_scripts();
        host.update(0.016);
        assert_eq!(global::<i64>(&host, "N"), 12);
        assert!(global::<bool>(&host, "RELOADED"));
    }

    #[test]
    fn sandbox() {
        let (_g, host) = game(
            "sandbox",
            &[
                ("game.json", "{}"),
                ("game.lua", "return 'not a script module'"),
                ("scripts/ok.lua", "return 7"),
                ("scripts/pkg/init.lua", "return 8"),
            ],
        );
        let lua = &host.lua;
        assert!(lua.load("return io").eval::<Value>().unwrap().is_nil());
        assert!(lua
            .load("return os.execute")
            .eval::<Value>()
            .unwrap()
            .is_nil());
        assert!(lua.load("return dofile").eval::<Value>().unwrap().is_nil());
        assert!(lua
            .load("return game.read_file('../../etc/passwd')")
            .eval::<Value>()
            .is_err());
        // #2: rewriting package.path must not let require() leave scripts/.
        let outside =
            std::env::temp_dir().join(format!("basalt_escape_{}.lua", std::process::id()));
        std::fs::write(&outside, "return 'ESCAPED'").unwrap();
        let dir = outside
            .parent()
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let stem = outside.file_stem().unwrap().to_string_lossy().to_string();
        let escape = lua
            .load(format!(
                "package.path = '{dir}/?.lua'; return require('{stem}')"
            ))
            .eval::<String>();
        std::fs::remove_file(&outside).unwrap();
        assert!(
            escape.is_err(),
            "require escaped the game folder: {escape:?}"
        );
        assert!(lua
            .load("return package.searchpath")
            .eval::<Value>()
            .unwrap()
            .is_nil());
        assert!(lua
            .load("return require('x/../../game')")
            .eval::<Value>()
            .is_err());
        assert_eq!(lua.load("return require('ok')").eval::<i64>().unwrap(), 7);
        assert_eq!(lua.load("return require('pkg')").eval::<i64>().unwrap(), 8);
        assert_eq!(
            lua.load("return game.read_file('game.json')")
                .eval::<String>()
                .unwrap(),
            "{}"
        );
    }

    #[test]
    fn no_update_after_failed_or_destroying_start() {
        let (_g, mut host) = game(
            "start-fail",
            &[
                (
                    "levels/l.json",
                    r#"{"entities":[{"script":"scripts/a.lua"},{"script":"scripts/b.lua"}]}"#,
                ),
                (
                    "scripts/a.lua",
                    "local A = {} function A:on_start() error('x') end function A:on_update() A_UPDATED = true end return A",
                ),
                (
                    "scripts/b.lua",
                    "local B = {} function B:on_start() self.entity:destroy() end function B:on_update() B_UPDATED = true end return B",
                ),
            ],
        );
        host.load_level("levels/l.json").unwrap();
        host.update(0.016);
        host.update(0.016);
        assert!(global::<Option<bool>>(&host, "A_UPDATED").is_none());
        assert!(global::<Option<bool>>(&host, "B_UPDATED").is_none());
    }

    #[test]
    fn eq_across_userdata_types() {
        let (_g, host) = game("eq", &[("game.json", "{}")]);
        let r: (bool, bool, bool, bool) = host
            .lua
            .load("local e = scene.spawn{} local v = vec3(1) return e == v, v == e, e == e, v == vec3(1)")
            .eval()
            .unwrap();
        assert_eq!(r, (false, false, true, true));
    }

    #[test]
    fn spawn_params_are_lua_values_and_script_is_immediate() {
        let (_g, mut host) = game(
            "spawn",
            &[(
                "scripts/e.lua",
                "local E = {} function E:on_start() SEEN_DIR = self.params.dir; SEEN_TARGET = self.params.target end return E",
            )],
        );
        let ok: bool = host
            .lua
            .load(
                r#"
                local boss = scene.spawn{ name = "boss" }
                local e = scene.spawn{ script = "scripts/e.lua", params = { dir = vec3(1, 2, 3), target = boss } }
                local inst = e:script()
                assert(inst ~= nil, "script() should work right after spawn")
                inst.configured = true
                return inst.params.dir == vec3(1, 2, 3) and inst.params.target == boss
                "#,
            )
            .eval()
            .unwrap();
        assert!(ok);
        host.update(0.016);
        let dir: types::LuaVec3 = global(&host, "SEEN_DIR");
        assert_eq!(dir.0, glam::Vec3::new(1.0, 2.0, 3.0));
        assert!(host
            .lua
            .load("return scene.find('entity_2'):script().configured")
            .eval::<bool>()
            .unwrap());
    }
}
