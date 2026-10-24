# Security

omabox promises one thing: a box never reaches your real desktop. Its Hyprland does not get your
screen, keyboard or mouse; its apps do not see your HOME, your secrets or your session; and the
commands that drive a box do not land on your desktop instead. This page lists each rule behind that
promise, where `bin/omabox` (or a tool) enforces it, and the test in `test/run.sh` that proves it.

## Reporting a problem

Report a way around any rule below privately, through GitHub's
[private vulnerability reporting](https://github.com/diogochaves/omabox/security/advisories/new),
not in a public issue. Say what you ran, what the box reached and your versions (`omabox --version`,
`Hyprland --version`, `omarchy version`). Only the latest release gets fixes.

## What omabox is not

A box is **not a security boundary against a hostile program**. It shares your kernel and your GPU
(a runaway app can load or hang either), and by default your network: it reaches the internet, your
LAN and the servers on your host's loopback (and, through the host's other addresses, any server of yours
listening on all of them). It reads what you mount into it. The rules below keep a
box from reaching your desktop, by mistake or through an app that misbehaves. They are not a defence
against code written to escape. For code you do not trust, use `--net isolated` at least,
[ai-jail](https://github.com/akitaonrails/ai-jail), or a VM.

Commands you run on the host are not boxed either: a project's `sudo ./setup`, or `omabox host --
CMD`, which runs on your real desktop on purpose.

## The rules

A change that weakens a rule updates its row; how each will be held by construction, and what no
test checks yet, is #355.

| Rule | Enforced in | Proven by |
|---|---|---|
| **S1. Hyprland never gets your seat.** Hyprland tries DRM first, so a box gets a GPU render node (`renderD*`, plus NVIDIA's `/dev/nvidia*` nodes for it) and never `/dev/dri/card*`, `/dev/input`, `/run/seatd.sock`, the system bus or your `$XDG_RUNTIME_DIR`. Its runtime dir is one of its own, at the same path. | `cmd_up`'s bwrap arguments (a fresh `/dev`, no host `/run`); `render_node`/`render_nodes` accept only `/dev/dri/renderD[0-9]+` | `t_main`: "no /dev/input", "no DRM card node", "no seatd socket", "no system bus", "the runtime dir is the box's own, not yours" |
| **S2. The box HOME is fake and holds no secrets.** It starts from `/etc/skel` and gets your Omarchy look (theme, terminal configs, branding, menu extensions) and git's `user.name`/`user.email`. Never `api-keys.env`, keyrings, tokens or the rest of your git config. Links in what it copies stay links, so a link to a secret brings nothing in. Your real HOME is not mounted. A command in a box starts from an empty environment: a host variable goes in only when you name it (`--env`, `--pass`, `--env-file`). | `seed_home`, `seed_copy` | `t_main`: "real HOME invisible", "no api-keys.env in the box HOME"; `t_unit_seed_copy` |
| **S3. Secrets and live sockets are never mounted or copied in.** `--ro-bind`, `--overlay`, `--plugin`, `--theme-dir` and `--seed` refuse anything that is or contains HOME, `~/.config/omarchy` or `/tmp`, anything inside `~/.config/omarchy` but `plugins/` and `themes/` (`api-keys.env`, your hooks), and anything inside or containing `~/.ssh`, `~/.gnupg`, keyrings, `~/.password-store`, `~/.aws`, `~/.kube`, `~/.docker`, `~/.netrc`, git's credential files, gh's, gcloud's, azure's and 1Password's `op` config, omabox's own saves and box HOMEs, the runtime dir, `/run`, `/dev`, `/proc`, `/sys` or `/tmp`'s socket dirs. A path is checked as named and where its links lead, so a link to a secret, or a secret's link to a file elsewhere, is refused too. `--seed` never writes through a link already in the box HOME. Nothing is mounted over the box's own system dirs, runtime dir, `/tmp` or HOME. A refused mount fails before a box dir is made. The repo `up` runs from goes in whole, its `.env` and `.git/config` included; for code you don't trust, use ai-jail, whose hidden paths stay hidden in the box (S9). | `refuse_src`, `refuse_dest` (`seed_copy` also skips what `refuse_src` refuses) | `t_unit_mount_rules`, `t_unit_refusals` |
| **S4. Host code is read-only.** `/usr`, `/etc` and omabox's own files are read-only binds. A repo is mounted read-only at its own path, or, for `omabox run` with no box up, as an overlay whose writes are thrown away. The only writable host folders are the box's own HOME and runtime dir. | `cmd_up`'s `--ro-bind` and `--tmp-overlay` arguments | `t_main`: "repo read-only"; `t_throwaway`: "host checkout unchanged" |
| **S5. Each box has its own namespaces and network.** pid, IPC, UTS and network namespaces of its own, so its Xwayland cannot take your X11 socket. A connected box's servers are forwarded to your host's 127.0.0.1 only, never the LAN. An isolated box has a loopback and the `--allow` ports, nothing else. | `cmd_up`: bwrap's `--unshare-*`, pasta's arguments | `t_isolated`, `t_connected`, `t_no_new_privs` |
| **S6. Teardown kills the box and nothing else.** `down` SIGKILLs the box's PID 1, and the kernel ends everything in its pid namespace. A recorded pid counts only while it is still in the box's own pid namespace, and a pasta only while its name and command line are this box's, so a reused pid of yours is never killed. A box ends with its pasta and with its Hyprland. | `box_pid`, `box_pasta`, `pasta_pid`, `kill_box` | `t_main` (down leaves no dir, HOME or reaper), `t_stale_pid`, `t_isolated_no_pidfile`, `t_pasta_dies`, `t_hyprland_dies`, `t_other_userns` |
| **S7. The tools that drive a box never follow it out.** A box can write its runtime dir and HOME, so omabox runs grim, hyprctl, gdb and its input tools inside the box's namespaces with a clean environment, writes into a box HOME from inside the box (`gdb --watch`'s log, `run -d`'s), and only takes plain names from what the box wrote (no paths), reading its files as regular files only (a FIFO there would hang the host, a link read a host file). omabox's own steps in a box run the system's tools by their full paths with a clean environment (the box's PATH and session environment are the box's to write), and what the host reads of a box's files or a step's answer it bounds on its own side. A command in a box gets only the fds it is given, and its stdin, stdout and stderr reach it as pipes held on the host, or a pseudo-terminal of its own, never a file, a terminal or a device of yours; a box with no pasta in front of it (a nested one) gets none of its caller's open files. | `on_box`, `box_env`, `box_sys`, `box_copy`, `fds_only`, `stdio_relay`, `box_pty`, `cmd_gdb`; `cmd_up`'s launch script (the fds) | `t_hostile`, `t_gdb`, `t_replace`, `t_jail_record`, `t_run_stdio`, `t_leak_control` (the nested box) |
| **S8. Input never reaches your desktop.** omabox's keyboard, pointer, still and events tools refuse to run outside a box. While the suite runs, a watcher listens to your Hyprland's event socket (read-only): any window, focus change or virtual keyboard of the suite's that shows up there fails the test that was running. At the end it checks your focused workspace and window are what they were. | the tools' own check: `/opt/omabox/share` a mount point and no system bus, as in every box and never on your desktop; the suite's watcher | `t_unit_pointer`, `t_unit_wait`, `t_unit_inspect` (the refusals); `t_unit_leak_scan`, and `t_leak_control`, which leaks on purpose into a box standing in for your desktop and must see each leak |
| **S9. An agent in ai-jail drives only its own boxes, and gets no more than its jail.** The broker reads the jail's policy from its bwrap command line, which nothing in the jail can change, and refuses what it cannot read. It refuses boxes another jail or you started, folders and a network the jail was not given, a path outside the jail to write a shot to, and `omabox host`. A jail's shots and recordings are written (and a video for a contact sheet read) only through files its own omabox opened and passed: regular files open as they are used, none of the kernel's, never reopened by name. A folder with paths the jail hides goes in with those hidden again, checked on the box's own mounts before anything in it runs. | `broker_init`, `jail_policy`, `select_box`, `jail_cover`, `jail_verify`, `fd_file`, `relay_record` | `t_jail`, `t_jail_record`, `t_unit_jail_policy`, `t_unit_jail_covers`, `t_unit_broker_units`, `t_unit_record` |

The agent guard (`omabox guard`) gives agents' shells a display that does not exist, so a GUI app
an agent starts by mistake fails instead of opening on your desktop (`t_guard`,
`t_unit_guard_settings`, `t_unit_guard_exec_host`). It catches mistakes. It is not a boundary: an
agent can still run anything you could.
