#!/usr/bin/env python3
"""Digest a Claude Code session, and every subagent it ran, down to the omabox moments.

The omabox-feedback skill reads the conversation, but a subagent's work never reaches it: only
its final report does. Claude Code keeps each whole transcript on disk, beside the session's:

    ~/.claude/projects/<cwd, each non-alphanumeric as ->/<session id>.jsonl            the session
    ~/.claude/projects/<cwd, ...>/<session id>/subagents/agent-*.jsonl              each subagent
    (with agent-*.meta.json: its description, worktree, parent agent)

($CLAUDE_CONFIG_DIR instead of ~/.claude when it is set.) They can run to hundreds of MB, so this
keeps only what the sweep needs: each command that ran omabox, a script that drives a box
(*box.sh, *-walk.sh) or a desktop tool the guard is there for (hyprctl, grim, wtype, xdg-open, ...),
with its result (longer when it looks failed: FAILED?); every shell command the guard answered
(GUARD); and the agent's own words where it talks about a box, the guard or a workaround. One
digest file per transcript, and a summary table on stdout. Standard library only; run it with
`python3 -I`.

With no --session, the session is the newest *.jsonl in the project dir: the one running the
sweep. The project dir defaults to the one for the current directory, so run it from where the
session started.
"""

import argparse
import json
import os
import re
import sys
from pathlib import Path

# A command worth keeping is one that *runs* omabox, a script that drives it, or a desktop
# tool the guard is there for; reading a file that mentions omabox is not.
RUNS = re.compile(
    r"^(omabox|omabox-\w+|[\w.-]*box\.sh|[\w.-]*-walk\.sh|hyprctl|grim|slurp|wtype|ydotool|quickshell"
    r"|qs|omarchy-[\w-]+|xdg-open|notify-send|busctl|dbus-send|gdbus|secret-tool|qmltestrunner)$"
)
ENV = re.compile(r"\b(WAYLAND_DISPLAY|HYPRLAND_INSTANCE_SIGNATURE|DISPLAY)=")
LEAD = re.compile(r"^(\w+=\S*\s+|timeout\s+(-\S+\s+)*\S+\s+|env\s+|exec\s+|sudo\s+|time\s+|nohup\s+|setsid\s+)+")
# Words in an agent's own text that mean it is talking about the box rather than the code.
# The guard speaking: its wrappers' "omabox guard: ..." line, or a program that failed on the
# display the guard sets, as a program prints it (at most a prefix such as "qt.qpa.xcb: " before
# it). A file that merely mentions them (a skill or this script read back: quoted, in a table, in
# a regex) is not, nor a process list showing the shell's own WAYLAND_DISPLAY=omabox-guard.
GUARD = re.compile(
    r"^\s*omabox guard: |^[^\"'`|\n]{0,80}?(Failed to create wl_display|could not connect to display|cannot open display)",
    re.I | re.M,
)
TALK = re.compile(r"omabox|\bthe box\b|\bboxes\b|the guard|workaround|work around", re.I)
# A result that went wrong: Claude Code's "Exit code N", a tool error, or a few loud words.
BAD = re.compile(
    r"^Exit code [1-9]|^Traceback|^\s*(error|fatal)[:!]|^omabox: (error|cannot|can.t|no |not |failed)|refus(ed|ing)|permission denied"
    r"|timed out|no such file|command not found|segmentation fault|core dumped|\bkilled\b",
    re.I | re.M,
)


def runs_box(cmd):
    if ENV.search(cmd):
        return True
    for part in re.split(r"\|\||&&|[;|\n]|\$\(|`", cmd):
        part = LEAD.sub("", part.strip())
        word = part.split(None, 1)[0] if part else ""
        if RUNS.match(os.path.basename(word)):
            return True
    return False

CMD_MAX = 400
RESULT_OK_MAX = 300
RESULT_BAD_MAX = 1500
TEXT_MAX = 700


def project_dir(cwd):
    config = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
    return config / "projects" / re.sub(r"[^A-Za-z0-9]", "-", str(cwd))


def clip(s, n):
    s = s.strip()
    return s if len(s) <= n else s[: n // 2] + f"\n  … [{len(s) - n} chars cut] …\n" + s[-n // 2 :]


def result_text(block, entry):
    tur = entry.get("toolUseResult")
    if isinstance(tur, dict) and ("stdout" in tur or "stderr" in tur):
        out = (tur.get("stdout") or "") + ("\n[stderr]\n" + tur["stderr"] if tur.get("stderr") else "")
        if tur.get("interrupted"):
            out += "\n[interrupted]"
        return out
    c = block.get("content")
    if isinstance(c, list):
        return "\n".join(x.get("text", "") for x in c if isinstance(x, dict))
    return str(c or "")


def tool_input_text(name, inp):
    if name == "Bash":
        return inp.get("command", "")
    if name in ("Skill",):
        return f"Skill {inp.get('skill')} {inp.get('args') or ''}"
    if name in ("Read", "Write", "Edit"):
        return f"{name} {inp.get('file_path', '')}"
    if name == "Monitor" or name.startswith("mcp__"):
        return f"{name} {json.dumps(inp)[:CMD_MAX]}"
    return ""


def digest(path):
    """Return (lines, stats) for one transcript."""
    pending = {}  # tool_use_id -> n, for the calls kept
    names = {}  # tool_use_id -> tool name, for every call
    lines, stats = [], {"calls": 0, "bad": 0, "talk": 0}
    n = 0
    with open(path, encoding="utf-8", errors="replace") as f:
        for raw in f:
            try:
                e = json.loads(raw)
            except ValueError:
                continue
            msg = e.get("message")
            if not isinstance(msg, dict) or not isinstance(msg.get("content"), list):
                continue
            ts = (e.get("timestamp") or "")[11:19]
            for b in msg["content"]:
                if not isinstance(b, dict):
                    continue
                t = b.get("type")
                if t == "tool_use":
                    name, inp = b.get("name", ""), b.get("input") or {}
                    cmd = tool_input_text(name, inp)
                    names[b.get("id")] = name
                    hit = (
                        (name == "Bash" and runs_box(cmd))
                        or (name == "Skill" and "omabox" in cmd)
                        or (name == "Edit" and "/omabox/" in cmd)
                        or (name.startswith("mcp__") or name == "Monitor") and "box" in cmd
                    )
                    if cmd and hit:
                        n += 1
                        stats["calls"] += 1
                        pending[b.get("id")] = n
                        lines.append(f"\n[{n}] {ts} {name}: {clip(cmd, CMD_MAX)}")
                elif t == "tool_result" and b.get("tool_use_id") not in pending:
                    # Any command the guard stopped counts, whatever it was.
                    out = result_text(b, e)
                    if names.get(b.get("tool_use_id")) == "Bash" and GUARD.search(out):
                        n += 1
                        stats["calls"] += 1
                        stats["bad"] += 1
                        lines.append(f"\n[{n}] {ts} (a command the guard answered)")
                        lines.append(f"  -> [{n}] GUARD: " + clip(out, RESULT_BAD_MAX).replace("\n", "\n     "))
                elif t == "tool_result":
                    k = pending.pop(b["tool_use_id"])
                    out = result_text(b, e)
                    guard = names.get(b["tool_use_id"]) == "Bash" and bool(GUARD.search(out))
                    bad = guard or bool(b.get("is_error")) or bool(BAD.search(out))
                    if bad:
                        stats["bad"] += 1
                    tag = "GUARD" if guard else "FAILED?" if bad else "ok"
                    body = clip(out, RESULT_BAD_MAX if bad else RESULT_OK_MAX)
                    lines.append(f"  -> [{k}] {tag}: " + body.replace("\n", "\n     "))
                elif t in ("text", "thinking") and e.get("type") == "assistant":
                    txt = b.get("text") or b.get("thinking") or ""
                    hits = [p for p in re.split(r"\n\s*\n", txt) if TALK.search(p)]
                    for p in hits[:3]:
                        stats["talk"] += 1
                        who = "thought" if t == "thinking" else "said"
                        lines.append(f"  ({ts} {who}) " + clip(p, TEXT_MAX).replace("\n", "\n     "))
    return lines, stats


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--project", type=Path, default=project_dir(Path.cwd()), metavar="DIR",
                    help="Claude Code's transcript dir for the project (default: the current directory's)")
    ap.add_argument("--session", metavar="ID", help="the session to digest (default: the newest in DIR)")
    ap.add_argument("--out", type=Path, metavar="DIR",
                    help="where the digests go (default: omabox-feedback-<session id> in $TMPDIR or /tmp)")
    ap.add_argument("--no-main", action="store_true", help="skip the session's own transcript: its subagents only")
    a = ap.parse_args()

    if a.session:
        main_t = a.project / f"{a.session}.jsonl"
    else:
        ts = sorted(a.project.glob("*.jsonl"), key=lambda p: p.stat().st_mtime)
        if not ts:
            sys.exit(f"no session transcripts in {a.project}")
        main_t = ts[-1]
    sid = main_t.stem
    out = a.out or Path(os.environ.get("TMPDIR", "/tmp")) / f"omabox-feedback-{sid}"
    out.mkdir(parents=True, exist_ok=True)

    todo = [] if a.no_main else [(main_t, {"description": "the session itself"})]
    for t in sorted((a.project / sid / "subagents").glob("agent-*.jsonl"), key=lambda p: p.stat().st_mtime):
        meta = t.with_suffix("").with_suffix(".meta.json")
        try:
            m = json.loads(meta.read_text())
        except (OSError, ValueError):
            m = {}
        todo.append((t, m))

    rows = []
    for t, m in todo:
        lines, s = digest(t)
        name = "session" if t == main_t else t.stem.removeprefix("agent-")
        d = out / f"{name}.txt"
        head = [
            f"# {name}: {m.get('description', '')}",
            f"# transcript {t}",
        ]
        if m.get("worktreePath"):
            head.append(f"# worktree {m['worktreePath']}")
        if m.get("parentAgentId"):
            head.append(f"# spawned by {m['parentAgentId']}")
        d.write_text("\n".join(head + lines) + "\n")
        rows.append((name, m.get("description", ""), s, d.stat().st_size if lines else 0))

    print(f"session {sid}: {len(rows)} transcripts, digests in {out}")
    print(f"{'transcript':<20} {'calls':>5} {'failed?':>7} {'remarks':>7} {'digest':>8}  description")
    for name, desc, s, size in rows:
        print(f"{name:<20} {s['calls']:>5} {s['bad']:>7} {s['talk']:>7} {size // 1024:>6} K  {desc[:60]}")
    total = sum(r[3] for r in rows)
    print(f"total digest {total // 1024} K; transcripts with no omabox calls or remarks have empty digests")


if __name__ == "__main__":
    main()
