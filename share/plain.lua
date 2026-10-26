-- omabox's own small Hyprland config for `up --desktop plain` (#247): a box with nothing of Omarchy's,
-- for any Hyprland 0.56+ (Lua config). Hyprland's own defaults otherwise; hyprland.lua then sets the
-- screen, input devices, debug and the startup as for every box. No bar: `up --shell CMD` brings one.
hl.config({
  general = { gaps_in = 4, gaps_out = 8, border_size = 2 },
  misc = {
    disable_hyprland_logo = true,
    disable_splash_rendering = true,
    background_color = "rgb(1a1b26)",
    focus_on_activate = true,
  },
  input = { kb_layout = "us", follow_mouse = 1 },
})
-- The two binds a bare desktop needs to be used by hand (an interactive box): a terminal, and closing
-- a window. Anything else is the shell's or the agent's (`omabox keys`, `omabox hyprctl dispatch`).
hl.bind("SUPER + RETURN", hl.dsp.exec_cmd(os.getenv("TERMINAL") or "xdg-terminal-exec"))
hl.bind("SUPER + W", hl.dsp.window.close())
