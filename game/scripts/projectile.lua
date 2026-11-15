-- A thrown crate: gravity, a few bounces, then it destroys itself.
-- params: velocity (vec3)
local Projectile = {}

local GRAVITY = -18
local LIFETIME = 6

function Projectile:on_start()
  self.velocity = (self.params.velocity or vec3(0)):copy()
  self.age = 0
  self.spin = vec3(math.random() - 0.5, math.random() - 0.5, math.random() - 0.5)
end

function Projectile:on_update(dt)
  local e = self.entity
  self.age = self.age + dt
  if self.age > LIFETIME then
    e:destroy()
    return
  end

  self.velocity.y = self.velocity.y + GRAVITY * dt
  e:translate(self.velocity * dt)
  e:rotate(self.spin, 360 * dt)

  local p = e.position
  local floor = e.scale.y * 0.5
  if p.y < floor then
    p.y = floor
    e.position = p
    self.velocity = vec3(self.velocity.x * 0.7, -self.velocity.y * 0.45, self.velocity.z * 0.7)
  end
end

return Projectile
