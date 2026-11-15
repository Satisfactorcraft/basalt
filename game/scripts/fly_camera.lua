-- Free-fly camera.
--   WASD move, E/Space up, Q/Ctrl down, Shift faster, scroll = speed
--   hold right mouse to look, or Tab to toggle mouse lock
local util = require("lib.util")

local Fly = {}

function Fly:on_start()
  local r = self.entity.rotation
  self.pitch, self.yaw = r.x, r.y
  self.speed = self.params.speed or 6
  self.sensitivity = self.params.sensitivity or 0.12
end

function Fly:on_update(dt)
  local e = self.entity

  if input.key_pressed("tab") then
    input.lock_cursor(not input.cursor_locked())
  end

  if input.cursor_locked() or input.mouse_down("right") then
    local dx, dy = input.mouse_delta()
    self.yaw = self.yaw - dx * self.sensitivity
    self.pitch = util.clamp(self.pitch - dy * self.sensitivity, -89, 89)
    e.rotation = vec3(self.pitch, self.yaw, 0)
  end

  local move = vec3(0)
  if input.key_down("w") then move = move + e:forward() end
  if input.key_down("s") then move = move - e:forward() end
  if input.key_down("d") then move = move + e:right() end
  if input.key_down("a") then move = move - e:right() end
  if input.key_down("e") or input.key_down("space") then move = move + vec3(0, 1, 0) end
  if input.key_down("q") or input.key_down("ctrl") then move = move - vec3(0, 1, 0) end

  if move:length() > 0 then
    local speed = self.speed * (input.key_down("shift") and 3 or 1)
    e:translate(move:normalized() * speed * dt)
  end

  local scroll = input.scroll()
  if scroll ~= 0 then
    self.speed = util.clamp(self.speed * (1 + scroll * 0.1), 0.5, 100)
  end
end

return Fly
