-- Shared helpers. Loaded with require("lib.util") from any script.
local util = {}

function util.wave(t, speed, amount)
  return math.sin(t * speed) * amount
end

function util.clamp(v, lo, hi)
  return math.max(lo, math.min(hi, v))
end

return util
