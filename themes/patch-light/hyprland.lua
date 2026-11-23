-- Patch Light: hairline borders, square corners, no shadows. (Omarchy stages a
-- theme's .lua only for a local theme, not one installed from git.)
local active_border_color = "rgb(111412)"
local inactive_border_color = "rgba(5f646088)"

hl.config({
  general = {
    border_size = 1,
    col = {
      active_border = active_border_color,
      inactive_border = inactive_border_color,
    },
  },

  group = {
    col = {
      border_active = active_border_color,
      border_inactive = inactive_border_color,
    },
  },

  decoration = {
    rounding = 0,
    shadow = {
      enabled = false,
    },
  },
})
