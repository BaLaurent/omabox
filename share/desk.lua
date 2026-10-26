-- `up --desktop desk` (#321): the user's own Hyprland config, the box HOME's copy of ~/.config/hypr,
-- loaded as their desktop loads it. Before it, an autostart policy: what the config starts (at parse
-- time or at hyprland.start) runs, except what a box must not or cannot run: idle and lock daemons (a
-- box has no input, so it would lock itself: finding 10), the user's systemd session and uwsm (a box
-- has no user manager unless --systemd), applets for the system bus it does not have, and the box's
-- shell when `--shell` runs it (the shell slot starts it). Each decision is a line in ~/autostart.log.
-- Binds (hl.dsp.exec_cmd) are left alone: a key pressed in the box does what it does on the desk.
local home = os.getenv("HOME") or "/home/sbx"
local hypr = home .. "/.config/hypr"
package.path = hypr .. "/?.lua;" .. hypr .. "/?/init.lua;" .. package.path

local drops = {
  { "^%s*systemctl%s+%-%-user", "no user manager in a box (up --systemd)" },
  { "^%s*uwsm", "no user manager in a box (up --systemd)" },
  { "dbus%-update%-activation%-environment.*%-%-systemd", "no user manager in a box (up --systemd)" },
  { "hypridle", "idle: a box has no input and would lock itself" },
  { "swayidle", "idle: a box has no input and would lock itself" },
  { "hyprlock", "lock: a box takes no real password" },
  { "polkit", "no system bus in a box" },
  { "nm%-applet", "no system bus in a box" },
  { "blueman%-applet", "no system bus in a box" },
  { "udiskie", "no system bus in a box" },
}
local shell_cmd = os.getenv("OMABOX_SHELL_CMD") or ""

local function why_drop(cmd)
  if type(cmd) ~= "string" then return nil end
  if shell_cmd ~= "" and cmd:find(shell_cmd, 1, true) then return "the box's shell (up --shell starts it)" end
  for _, d in ipairs(drops) do
    if cmd:match(d[1]) then return d[2] end
  end
  return nil
end

local function note(what, cmd, why)
  local f = io.open(home .. "/autostart.log", "a")
  if f then
    f:write(what, " ", tostring(cmd), why and ("  (" .. why .. ")") or "", "\n")
    f:close()
  end
end

-- (A global guard: hyprctl reload runs this file again, over functions already wrapped.)
if not omabox_desk_wrapped then
  omabox_desk_wrapped = true
  local exec_cmd, execute, popen = hl.exec_cmd, os.execute, io.popen
  hl.exec_cmd = function(cmd, ...)
    local why = why_drop(cmd)
    note(why and "drop" or "run", cmd, why)
    if why then return end
    return exec_cmd(cmd, ...)
  end
  os.execute = function(cmd, ...)
    local why = why_drop(cmd)
    note(why and "drop" or "run", cmd, why)
    if why then return true, "exit", 0 end
    return execute(cmd, ...)
  end
  io.popen = function(cmd, ...)
    local why = why_drop(cmd)
    note(why and "drop" or "run", cmd, why)
    if why then return io.open("/dev/null") end
    return popen(cmd, ...)
  end
end

-- The user's config; an error in it is a config error (the box still starts, with what loaded).
local cfg = hypr .. "/hyprland.lua"
local f = io.open(cfg)
if not f then
  note("none", cfg, "no hyprland.lua: omabox's plain config instead")
  dofile("/opt/omabox/share/plain.lua")
else
  f:close()
  local ok, err = pcall(dofile, cfg)
  if not ok then note("error", cfg, tostring(err):gsub("%s+", " ")) end
end
