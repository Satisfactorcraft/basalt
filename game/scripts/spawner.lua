-- Spawns a ring of spinning crates and gems at runtime.
-- params: count, radius
local Spawner = {}

function Spawner:on_start()
  local count = self.params.count or 8
  local radius = self.params.radius or 5
  for i = 1, count do
    local angle = (i / count) * math.pi * 2
    local gem = i % 2 == 0
    scene.spawn {
      name = "ring_" .. i,
      model = gem and "models/gem.gltf" or "models/crate.gltf",
      position = vec3(math.cos(angle) * radius, gem and 1.2 or 0.4, math.sin(angle) * radius),
      scale = gem and 0.9 or 0.8,
      script = "scripts/spinner.lua",
      params = { speed = 20 + i * 12, bob = gem and 0.3 or 0 },
    }
  end
  log.info("spawner: created " .. count .. " entities")
end

return Spawner
