# Laws

The properties omabox must never lose, each with what checks it. `AGENTS.md`'s Non-negotiables say
the same in prose, together with the rules for how agents work here (no `ydotool`, `sudo` only when
allowed); this file keeps only what the program itself must hold, so each law can have a check.

A change that breaks a law fails a check, never a review. A law with a gap is one a change could
still break unseen: closing gaps is #355. A new law comes with its check, or with its gap written
down here.

How a law is held, strongest first:

- **type**: the code can't express a violation (Rust: the only way to build the thing checks it).
- **property**: a unit test over generated inputs (every combination of options, not a few examples).
- **self-check**: omabox checks the live box itself and stops it when the law fails.
- **live**: the suite checks it on real boxes, after every test or in named tests.

## L1. The real desktop is left alone

No focus change, workspace switch, cursor warp, window or layer on the user's session from a box or
a command, unless the user asked for one (interactive mode, `omabox peek`). What must touch the host
Hyprland goes `silent` to the configured workspace.

- live, every test: `host_scan` reads the host's event log after each test and fails it on anything
  of the suite's (`leak_scan`, its cases in `t_unit_leak_scan`, `t_leak_control`); `host_same` at the
  end of the run.
- Gap: cursor warps are checked only where `no_pointer_on` is called (`t_pointer`).
- Rust (wave 5+), type: a host Hyprland handle that only has `silent` dispatches; one that may focus
  needs a value only interactive mode and peek get.

## L2. A box never gets the real seat

Never bound into a box: `/dev/dri/card*` as a device, `/dev/input`, `/run/seatd.sock`, `/run/dbus`
(the system bus), the real `$XDG_RUNTIME_DIR` (a box gets its own). Hyprland tries DRM first; these
are all that stop it from taking the seat.

- live, named tests: `check_box_safety` (in `t_main`, `t_isolated`, `t_hyprland`, `t_guard`,
  `t_systemd`, `t_saves` and 8 more).
- Gap: a box no test passes to `check_box_safety` is never checked (a new option, a new mode).
- Rust (wave 5+), type: the bwrap command takes only a bind type whose constructor refuses these
  paths; property: every combination of `up`'s options (net modes, nested, GPU, `--bind`/`--ro-bind`,
  saves) builds a command with none of them; self-check: `up` reads the started box's mountinfo and
  stops it if one is there.

## L3. A box holds no secrets

The box's HOME is fake and seeded without secrets: never `~/.config/omarchy/api-keys.env`, keyrings,
tokens, `~/.ssh`. HOME and its secret folders are refused as a bind source.

- live/unit: `t_unit_mount_rules` (what may go in, and where), `t_unit_refusals`, `t_unit_seed_copy`
  (a link in a seeded folder stays a link, never followed), `t_main` and `t_desk_shell` on live boxes.
- Gap: no check reads a live box's whole HOME for secret files; the seed is checked case by case.
- Rust (wave 5+), type: seeding goes through an allow list, never a copy of a whole folder; property:
  the source check over generated paths (`..`, links, trailing slashes, repeated slashes) refuses every
  path that resolves to HOME or under a secret folder.

## L4. Host code is read-only in a box

What omabox mounts from the host (its checkout, the system, the user's code it was given) is
read-only unless the user bound it writable.

- live: `t_main` (the checkout), `t_desk_shell` (the desk's plugins), `t_hyprland` (its config folder)
  try to write and expect it to fail.
- Rust (wave 5+), type: a host mount is read-only unless built from the user's explicit `--bind`.

## L5. A box that ends leaves nothing running

`down`, an idle limit, a failed `up` or a signal to `up` ends the box's namespace and its pasta, and
leaves no process of the box.

- live, named tests: `t_pasta_dies`, `t_stale_pid`, `t_unit_kill_box`, `t_orphan`, `none_running` in
  two tests; on the port branch also `t_up_signals` (finding 434: a signal cut `up` short and left
  the box).
- Gap: nothing checks at the end of a run that none of the run's boxes left a process.

## L6. An isolated box reaches only the ports it was given

With `--net isolated --allow PORTS`, a box reaches those host ports and no other.

- live: `t_isolated`, `t_run_options`.
- Rust (wave 5+), type: the isolated net mode carries its allow list; property: pasta's arguments
  open exactly the allowed ports for any list.

## L7. The input tools refuse to run outside a box

`omabox-keyboard`, `-pointer`, `-still`, `-burst`, `-events` exit 2 when run from a host shell: run
there, they would drive or watch the real desktop.

- unit: `t_unit_pointer`, `t_unit_wait`, `t_unit_inspect` run each one outside a box and expect 2.

## L8. An agent under the guard has no way to the real display

Under `omabox guard`, a shell's `WAYLAND_DISPLAY`, `DISPLAY` and `HYPRLAND_INSTANCE_SIGNATURE` reach no
real session, and opening a link or file is refused; only `omabox host -- CMD` reaches the desktop,
and only for what the user asked.

- live: `t_guard` (a box standing in for the host), `t_unit_guard_exec_host`.
