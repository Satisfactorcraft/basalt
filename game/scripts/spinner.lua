-- Spins an entity around its Y axis and optionally bobs it up and down.
-- params: speed (degrees/second), bob (height), bob_speed
local util = require("lib.util")

local Spinner = {}

function Spinner:on_start()
  self.base_y = self.entity.position.y
  self.t = math.random() * 10
  self:read_params()
end

-- Runs after you edit and save this file, with `self` state kept.
function Spinner:on_reload()
  self:read_params()
end

function Spinner:read_params()
  self.speed = self.params.speed or 90
  self.bob = self.params.bob or 0
  self.bob_speed = self.params.bob_speed or 2
end

function Spinner:on_update(dt)
  self.t = self.t + dt
  self.entity:rotate_y(self.speed * dt)
  if self.bob ~= 0 then
    local p = self.entity.position
    p.y = self.base_y + util.wave(self.t, self.bob_speed, self.bob)
    self.entity.position = p
  end
end

return Spinner
