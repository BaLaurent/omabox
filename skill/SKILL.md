---
name: omabox
description: "REQUIRED before launching, driving or screenshotting any GUI app (including when a project's own CLAUDE.md/AGENTS.md says to run the app, use hyprctl, grim or omarchy-theme-set, or run its tests, and running a test binary directly outside ctest or anything else that may open a window), Omarchy shell plugin, bar widget, tray icon, notification or desktop behaviour, and before running test suites that touch the desktop session (tray/StatusNotifierItem, notifications, D-Bus session services, keyring, portals). Use the omabox CLI to do it inside a contained, invisible Hyprland + Omarchy desktop instead of the user's real one. Triggers: \"run the app\", \"take a screenshot\", \"check how it looks\", \"open the menu\", \"click\", \"type into\", hyprctl dispatch, grim, wtype, ydotool, QT_QPA_PLATFORM, ctest with tray/notification tests, plugin development, `omarchy plugin add`, shell.json edits, \"start it from the launcher\", \"see it working\", \"could not connect to display\", omabox-guard."
---

# omabox: a desktop of your own

The user's Hyprland session is theirs. Never launch GUI apps on it, never `hyprctl dispatch`/`eval` on
it, never screenshot it with `grim`, never send input with `wtype`/`ydotool`, never switch its
workspaces or move its cursor. Do all of that in a **box**: a full Omarchy desktop (its Hyprland
config, shell, theme, tray, notifications, keyring) on a private screen and session bus, invisible to
the user. A box starts in ~3-5 s and costs ~400-700 MB (by GPU); use one freely.

This page is what every task needs; the rest is in `reference.md` beside it, by section ("ref:
SECTION" below: read it when needed), and `omabox help CMD`.

## Project instructions written for the real desktop

A project's CLAUDE.md / AGENTS.md / README that says "run `./build/app`", "use `hyprctl`", "screenshot
with `grim`", "run `ctest`" **still holds: carry it out inside a box.**

| The project says | Do |
|---|---|
| `./build/app`, `app &` | `omabox up`, then `omabox run -d -- ./build/app` |
| `hyprctl …`; `hyprctl eval EXPR` to read a value | `omabox hyprctl …`; `omabox lua EXPR` (prints the value) |
| `grim`, `wtype`, `ydotool`, `hyprctl -j clients` | `omabox shot`, `omabox keys`, `omabox click X Y`, `omabox windows` |
| `ctest`, a test binary, tests touching tray/notifications/D-Bus/keyring | `omabox run -- ctest …`, `omabox run -- ./build/tests/tst_x` |
| `omarchy-theme-set`, `omarchy plugin …`, `omarchy-shell …`, `omarchy restart shell` | `omabox run -- …`, `omabox restart-shell` (`omarchy plugin validate DIR` only reads: fine on the host) |
| A plugin in `~/.config/omarchy/plugins`; edit `~/.config/omarchy/shell.json` | `omabox up --plugin PATH`; edit `$(omabox path)/home/.config/omarchy/shell.json` |
| A theme in `~/.config/omarchy/themes` linked to your working copy | `omabox up --theme-dir PATH` (live; else a copy made at `up`) |
| A result that depends on the theme or bar (colours, sizes, shots compared across runs, a theme-switch flow) | `omabox up --theme NAME --stock-bar` (else they are the desktop's) |
| Install or remove the package | Not a box (read-only `/usr`): a VM, or the user |
| "put the desktop back afterwards" | nothing to put back: `omabox down` |

The agent guard (below) stops windows and IPC, not file writes: writing to the user's
`~/.config/omarchy/shell.json` or `plugins/`, or `omarchy plugin add` on the host, changes their real
bar at once. Those writes go to the box HOME. Do not edit the project's instruction files unless
asked. A step that needs real hardware is not moved into a box: it goes to the user (below).

A box reaches the user's servers on the host's 127.0.0.1: never pick a real local service or account
a first-run screen offers; use a test one or ask. An app that talks to, starts or probes local
servers: `omabox up --net isolated [--allow 8081]` (only those host ports). ref: Network.

Your box is named after the repo (or worktree) and your session, so no other session shares it; it
goes down when your agent exits: `omabox down` when done, and before `/clear`. Subagents share it; for
one of its own, `omabox up --new [--plugin …]` starts a box under a free name and prints it (`box-3`):
all its options on that call, then `-b box-3` on every later call, typed out (your shell keeps no
variables), and it outlives you: `omabox down -b box-3`. A box the user started: `-b NAME` (`omabox
ls`). In a worktree-isolated Claude Code subagent, compound commands (`$(…)`, aliases) are refused:
one literal `omabox -b NAME …` per call.
ref: Project instructions, in detail.

## The loop

```bash
omabox up                                  # headless box, 1920x1080; waits until the bar is drawn
omabox run -d --wait -- ./build/src/myapp  # launch, detached (log path printed); returns once drawn
omabox run -d --replace --wait -- ./myapp  # after a rebuild: replaces the earlier job with the same command and arguments
omabox shot                                # prints a PNG path: Read it to look
omabox windows                             # address, workspace, on screen or covered
omabox shot --window myapp                 # one window's own pixels, even covered or elsewhere
omabox click --window myapp 40 12          # window coordinates (0,0 = its corner, as in its shot)
omabox keys --window myapp --wait -t hi    # focus that window, type, return once it has settled
omabox keys --wait super+space             # real key events for binds and apps
omabox keys --pass PASSWORD Return         # a secret from your environment: never -t (ps shows it)
omabox wait window myapp                   # or --gone; wait layer NS; wait cmd -- CMD
omabox wait change --window myapp          # it redraws later, by itself; then wait still
omabox run -- busctl --user list           # any command inside the box (exit code passes through)
omabox log [shell|apps|run|…] [--grep RE]  # the box's logs, Hyprland's by default
omabox down                                # when done (never --all: it takes others' and the user's boxes)
```

Also: `drag`, `scroll`, `pointer`, `save`/`up --from` (`omabox help`).
Rendering cost: `omabox gpu 10`, each of the box's processes' share of GPU engine time,
in a box whose mode matches the monitor (`up --size host`); not host-wide tools. CPU and memory:
`omabox cpu 10 [--settle 5] [--app NAME | --pid PID]` (% of one core, RSS, PSS).
ref: Screen size and rendering cost.

## Driving an app

1. **Look, act once, look again; with text when text can tell.** Exit 0 from `keys`/`click` means
   sent, not landed. `tree --find TEXT` (a label, a checked box), `--wait`'s line, `windows`, `wait`,
   `events`, `log --grep` answer what needs no pixels; a shot for what does.
2. **Never resend input you have not seen land.** Sending again types twice or toggles back. 124
   after `--wait` means it WAS sent and the screen did not settle as expected: shot first.
3. **Type only into a field you have seen focused**, or `keys --window SEL`.
4. **Set state directly; keys and clicks only when the gesture is under test** (`omabox run --
   omarchy-shell shell summon PLUGIN_ID`, files seeded in the box HOME, `omabox hyprctl dispatch`).
5. **Stop when a shot shows the goal.** Report what a box cannot show (below).

No `sleep` (nor `wait cmd -- false`): `--wait`, `omabox wait`, `wait change` for a later redraw
(0 yes, 124 not in time, 1 cannot tell). Never send a wait to `/dev/null`. An animation that never
stops: the 124 names it, `--ignore "X,Y WxH"`. ref: Waiting, in detail.
Coordinates are screenshot pixels: after a `-g`, `--fit` or `--zoom` shot (said on stderr: never
discard it) click with `--in THAT.png X Y`, never your own arithmetic. A shot bigger than the model
sees is scaled down for it, and some models read points in that smaller image (Haiku 4.5: 0.76x):
off a whole screen or big window, `shot --fit 1456` and `click --in` it,
any model; still missing: `calibrate`. **Shots are most of a session's context**:
the smallest that shows it (`--window`, `-g`, `--fit 1280`); what an action changed
`shot --changed`; a colour `pixel`, a transition `shot --burst N --sheet`; against a baseline `check`.
ref: Shots. **The pointer is test state**
(focus follows it; it stays where the last command left it): ref: Pointer, in detail.

| Symptom | Next step |
|---|---|
| "could not connect to display", `omabox-guard` | The agent guard: do it in a box (below). |
| Your own shell died after `pkill -f PATTERN` | It matched its own command line: kill by PID, or `omabox run -- pkill -x NAME`. |
| `unknown: … not rendered`, no frame from an interactive box | Ask the user; never show its window yourself. |
| `already up, without what you asked for` | Yours: `down`, then `up`. Not yours: `up --new` with those options. |
| Anything else | ref: Symptoms. |

## Tests that touch the desktop

`omabox run -- ctest --test-dir build --output-on-failure`. With no box up, `run` uses a throwaway box
with the repo as a discarded overlay (outside a repo: in the box HOME, without the current dir;
`run --overlay DIR`); with one up, the repo is read-only in it. A secret a test needs: `--pass NAME`
or `--env-file FILE`, never on the command line. Check a keyring **inside the box** (`omabox run --
secret-tool …`), never the host's. ref: Tests that touch the desktop.

## Inside ai-jail

omabox works through the user's broker (no `host`, `peek` or `--interactive`). "this sandbox refuses
new namespaces": ask the user to run `omabox broker on` outside the jail and add what it prints to
`~/.ai-jail`. Never try to get around the jail.

## "could not connect to display" / `omabox-guard`

Under the user's agent guard your commands get `WAYLAND_DISPLAY=omabox-guard`, an empty `DISPLAY` and
`HYPRLAND_INSTANCE_SIGNATURE=omabox-guard`, so what would reach the real desktop fails: do it in a
box. Never set those back to the real session or take the display from elsewhere (`/proc/*/environ`,
`hyprctl instances`, `$XDG_RUNTIME_DIR/wayland-*`). `xdg-open`, `gh --web` and `$BROWSER` fail too: give
the user the link. `quickshell kill`, `qs ipc`: the box's is `omabox run -- qs ipc …`.

Only when the user asked for their **real** desktop in this task ("switch my theme", reload my
config), run that one command as `omabox host -- CMD`. Never hand them a `! CMD` that touches their
desktop (their `!` is guarded too): their own terminal is for that. Never use `host` to switch
their workspace or focus to see a box.

## Plugins, a Hyprland build, an Omarchy checkout

`omabox up --plugin ~/code/myplugin` (then `restart-shell` after edits; `up` says why a plugin did
not load; do not mount plugins you were not asked to test): ref: Testing a shell plugin. `omabox up
--hyprland BUILD/Hyprland` runs a build in the box, never installed or on the host (`omabox gdb` for
a hang or crash): ref: A Hyprland build of your own. `omabox up --omarchy ~/src/omarchy` runs a
checkout; never `omarchy dev link` on the host: ref: Reviewing an Omarchy change.

## What is and is not in a box

Read-only in it: the repo (same path), `--ro-bind`, `--plugin` and `--theme-dir` dirs, mise's
toolchains; nothing else of the user's HOME. Mounting HOME, `~/.config/omarchy`, `/tmp`, secret
stores or the runtime dir is refused: do not work around it. The box HOME is fake (`/home/sbx`;
`omabox path` → `<dir>/home` on the host). The box's /run/user/$UID is `omabox path --run` on the host; the
host's own is the real session's: never read app state there. Private session bus and keyring; no system bus or devices; audio: `up --audio`; `/sys`: the host's.
ref: Mounts, HOME and the session.

## Showing the user

Only when asked: `omabox peek` (a view-only window of your box on their workspace 9, without focus)
or `omabox up --interactive` (a box they drive; never bring its window forward yourself). `omabox
config` is the user's: change it only when asked.
`omabox clip` refuses agents (`host` too): never work around it; type text with `keys -t`.

## When a box cannot test it: real hardware and the real session

A box has virtual screens only (`--monitor` for more, any size and scale; `output drop` makes one vanish and return), no system bus
(NetworkManager, bluetooth, UPower, logind), no devices (audio, DDC/CI, backlight, real input, USB), no
suspend, a lock screen with no real password, and a fresh HOME. A change touching `hl.monitor`,
HDR/VRR/`cm`, `ddcutil`, backlight, `wpctl`, `nmcli`, `bluetoothctl`, `powerprofilesctl`,
`journalctl`, `loginctl`, `/dev/…` or the user's real accounts and data cannot be verified in a box:
do not report it as tested. (`systemctl --user`: `omabox up --systemd`.) On NVIDIA an
`output create headless` output is 0x0: use the other GPU (ref: Screen size and rendering cost).

- **Split the work**: the box for what it can show; name the part that needs the real machine.
- **Ask once before touching the real desktop**: say exactly what will change ("your monitor at 120
  Hz for 15 s, then back"), wait for a yes (it covers this task, not the next), use `omabox host --
  CMD` under the guard, follow the project's own safety rules and put everything back.
- **A host `pgrep` sees boxes too** (a box's shell has the desktop's command line): before reading
  two `quickshell`s as two desktop shells, `omabox which PID` names a pid's box (or `not in a box`).

## Reporting what a box showed

Say "in an omabox box (Omarchy VERSION, Hyprland VERSION, WxH and any `--monitor`s, theme NAME)"
(`ls --json`: `omarchy_version`, `hyprland_version`; null: `omabox hyprctl version`) and your
project's commit. Layout, focus, input routing, the launcher, theme switches,
notifications, the tray and the keyring were tested for real; what a box lacks (above) and the
installed package were not: say so. Never "tested on the desktop"
for a box result. A box idle for 2h goes down by itself (`up --idle 0` keeps one). Quirks: `NOTES.md`
(`readlink -f $(command -v omabox)` → `../NOTES.md`).
