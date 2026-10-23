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

No focus change, workspace switch, cursor warp, window, layer or clipboard change on the user's
session from a box or a command, except by a command whose job it is: an interactive box's window and
its monitors, `omabox peek`, `omabox clip --from-box`, `omabox host -- CMD`. What must touch the host
Hyprland goes `silent` to the configured workspace.

- live, every test: `host_scan` reads the host's event log after each test and fails it on anything
  of the suite's (`leak_scan`, its cases in `t_unit_leak_scan`, `t_leak_control`); `host_same` at the
  end of the run.
- Gap: nothing checks the host's cursor: the event log has no cursor events, and every `cursorpos`
  in the suite is a box's.
- Rust (wave 5+), type: a host Hyprland handle that only has `silent` dispatches; one that may focus
  needs a value only interactive mode and peek get.

## L2. A box never gets the real seat

Never bound into a box: `/dev/dri/card*` as a device, `/dev/input`, `/run/seatd.sock`, `/run/dbus`
(the system bus), the real `$XDG_RUNTIME_DIR` (a box gets its own). Hyprland tries DRM first; these
are all that stop it from taking the seat.

- before the box: every host folder `up` mounts by name (`--ro-bind`, `--overlay`, `--seed`,
  plugins, the repo, `--hyprland`/`--quickshell`'s folders, fonts) goes through `refuse_src`, which
  refuses `/dev`, `/run`, `/proc`, `/sys` and the runtime dir (`t_unit_mount_rules`). omabox's fixed
  binds (the system's folders, the render node, NVIDIA's files) don't pass through it.
- live, named tests: `check_box_safety` (in `t_main`, `t_isolated`, `t_hyprland`, `t_guard`,
  `t_systemd`, `t_saves` and 8 more).
- Gap: a box no test passes to `check_box_safety` is never checked (a new option, a new mode).
- Rust (wave 5+), type: the bwrap command takes only a bind type whose constructor refuses these
  paths; property: every combination of `up`'s options (net modes, nested, GPU, `--ro-bind`, `--overlay`,
  saves) builds a command with none of them; self-check: `up` reads the started box's mountinfo and
  stops it if one is there.

## L3. A box holds no secrets

The box's HOME is fake and seeded without secrets: never `~/.config/omarchy/api-keys.env`, keyrings,
tokens, `~/.ssh`. HOME itself, `~/.config/omarchy` (but its `plugins/` and `themes/`), the secret
folders (keys, credentials, browsers, agents' configs) and live sockets are refused as a mount source.
A command in a box starts from an empty environment: a host variable goes in only when the caller
names it (`--env`, `--pass`, `--env-file`). The host clipboard goes in only by `omabox clip`.

- live/unit: `t_unit_mount_rules` (what may go in, and where), `t_unit_refusals`, `t_unit_seed_copy`
  (a link in a seeded folder stays a link, never followed), `t_main` and `t_desk_shell` on live boxes.
- Gap: no check reads a live box's whole HOME for secret files; the seed is checked case by case.
  No check that a box command's environment holds only what was named.
- Rust (wave 5+), type: seeding goes through an allow list, never a copy of a whole folder; property:
  the source check over generated paths (`..`, links, trailing slashes, repeated slashes) refuses every
  path that resolves to HOME or under a secret folder.

## L4. A box never writes to the host's files

What omabox mounts from the host (its checkout, the system, the user's code it was given) is
read-only. There is no writable bind: `--ro-bind` is read-only, and an `--overlay`'s writes go to a
tmpfs layer in the box (bwrap's `--tmp-overlay`), never to the folder. The only writable binds
are the box's own: its HOME (`~/.cache/omabox/NAME/home`), its runtime dir and, with `--systemd`,
its own cgroup.

- live: `t_main` (the checkout), `t_desk_shell` (the desk's plugins), `t_hyprland` (its config folder)
  try to write and expect it to fail.
- Gap: no check writes through an `--overlay` and then looks at the host folder.
- Rust (wave 5+), type: a host mount can only be built read-only or as an overlay.

## L5. A box that ends leaves nothing running

`down`, an idle limit, a failed `up` or a signal to `up` ends the box's namespace and its pasta, and
leaves no process of the box.

**Broken in 1.0** (finding 434): a signal that cuts `up` short after its box started leaves the box
running. Fixed on the port branch.

- live, named tests: `t_pasta_dies`, `t_stale_pid`, `t_unit_kill_box`, `t_orphan`, `none_running` in
  two tests; on the port branch also `t_up_signals` (finding 434: a signal cut `up` short and left
  the box).
- Gap: nothing checks at the end of a run that none of the run's boxes left a process.

## L6. An isolated box reaches only the ports it was given

With `--net isolated --allow PORTS`, a box reaches those host ports and no other, and no network
beyond the host.

Not a law for a connected box (the default): pasta forwards every host loopback port into it on
purpose (`-T auto`, so an app in the box reaches a dev server on the host), and a host service that
listens on every address is also reached through the host's other addresses (a docker bridge, a VPN
address), which no forwarding rule covers. So a connected box can reach the user's own local
services; what keeps agents off them is `AGENTS.md`'s rule, not the program. Planned (#355): ports
the user protects, which no box reaches by any of the host's addresses.

- live: `t_isolated`, `t_run_options`.
- Rust (wave 5+), type: the isolated net mode carries its allow list; property: pasta's arguments
  open exactly the allowed ports for any list.

## L7. The input tools refuse to run outside a box

`omabox-keyboard`, `-pointer`, `-still`, `-burst`, `-events`, `-tree` exit 2 when run from a host
shell: run there, they would drive or watch the real desktop.

- unit: `t_unit_pointer`, `t_unit_wait`, `t_unit_inspect` run each one outside a box and expect 2.

## L8. An agent under the guard has no way to the real display

Under `omabox guard`, a shell's `WAYLAND_DISPLAY`, `DISPLAY` and `HYPRLAND_INSTANCE_SIGNATURE` reach no
real session, and opening a link or file is refused; only `omabox host -- CMD` reaches the desktop.
(Using `host` only when the user asked is an agent's rule, in `AGENTS.md`.)

- live: `t_guard` (a box standing in for the host), `t_unit_guard_exec_host`.
