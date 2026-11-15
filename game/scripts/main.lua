-- Game-wide logic, attached to the "game" entity in levels/main.json.
local Game = {}

function Game:on_start()
  self:load_settings()
end

function Game:load_settings()
  local settings = json.decode(game.read_file("data/demo.json"))
  log.info(settings.message)
  self.fire_speed = settings.fire_speed or 15
end

-- Hot reload for data files: edit data/demo.json while the game runs.
function Game:on_file_changed(path)
  if path == "data/demo.json" then
    self:load_settings()
  end
end

function Game:on_update(dt)
  if input.key_pressed("escape") then
    if input.cursor_locked() then
      input.lock_cursor(false)
    else
      game.quit()
    end
  end

  if input.key_pressed("r") then
    scene.reload_level()
  end

  if input.key_pressed("f") or input.mouse_pressed("left") then
    self:throw_crate()
  end
end

function Game:throw_crate()
  local cam = scene.camera()
  if not cam then return end
  local f = cam:forward()
  local v = f * self.fire_speed + vec3(0, 2, 0)
  scene.spawn {
    model = "models/crate.gltf",
    position = cam.position + f,
    scale = 0.4,
    script = "scripts/projectile.lua",
    params = { velocity = v },
  }
end

return Game
