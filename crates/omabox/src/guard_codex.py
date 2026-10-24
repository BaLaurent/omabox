import copy, json, os, re, sys, tomllib
mode, path, block = sys.argv[1:]
try:
    cur = open(path).read()
except FileNotFoundError:
    cur = ""
lines = block.split("\n")
begin, end, mine = "# >>> omabox guard", lines[-1], lines[1:-1]
guard = tomllib.loads("\n".join(mine))["shell_environment_policy"]["set"]
# Our keys go in a table of their own, or (#221) into the config's own shell_environment_policy.set:
# under its [shell_environment_policy.set] header, as set.KEY under [shell_environment_policy], or
# as dotted keys at the top. Each form names them with its own prefix.
PREFIXES = ("", "set.", "shell_environment_policy.set.")
current = {mine[0]} | {p + l for p in PREFIXES for l in mine[1:]}
def ours(l):   # our header, or one of our keys with any value (an older block's), in any form
    k = l.split(" = ")[0]
    return l == mine[0] or any(k == p + g for p in PREFIXES for g in guard)
def fail(msg):
    print("ERR " + msg); sys.exit(1)
def load(s, what):
    try:
        return tomllib.loads(s)
    except tomllib.TOMLDecodeError as e:
        fail(f"{path} {what} ({e}): not touched")
def pset(d):   # its shell_environment_policy.set, or None
    p = d.get("shell_environment_policy")
    return p.get("set") if isinstance(p, dict) else None
def has_guard(d):   # every key of the guard there with the guard's value, marked or not
    s = pset(d)
    return isinstance(s, dict) and all(s.get(k) == v for k, v in guard.items())
def by_hand(keys):
    return "\n".join(f"{k} = {json.dumps(guard[k])}" for k in keys)
def find(s):
    # (start, end, inner lines) of our block, or None. Codex keeps the file's last comment, our end
    # marker, at the very end when it adds a table (codex mcp add, a trusted project), so what follows
    # our lines inside the markers may be Codex's own.
    i = s.find(begin)
    if i < 0 or (i > 0 and s[i - 1] != "\n"):
        return None
    j = s.find("\n" + end, i)
    if j < 0:
        fail(f"{path}: an omabox guard block with no end marker: not touched")
    k = j + 1 + len(end) + (1 if s[j + 1 + len(end):j + 2 + len(end)] == "\n" else 0)
    return i, k, s[i:j].split("\n")[1:]
def strip(s):
    b = find(s)
    if not b:
        return s
    i, k, inner = b
    rest = [l for l in inner if not ours(l)]
    while rest and not rest[0].strip():
        rest.pop(0)
    # A table of our own: only whole tables may stay, as a key of someone else's left above them
    # would join the table before our block once our header is gone. Keys merged into the config's
    # own table (#221) have no header of ours: what is left there stays in that table.
    if inner and inner[0] == mine[0] and rest and not rest[0].startswith("["):
        fail(f"{path}: something was added inside the omabox guard block: not touched; remove the block by hand")
    kept = "\n".join(rest).rstrip("\n")
    if kept.startswith("[") and not s[:i].endswith("\n\n"):
        kept = "\n" + kept   # Codex's table, kept apart from the one ours were merged into
    if s[:i].endswith("\n\n") and not kept:
        i -= 1
    return s[:i] + (kept + "\n" if kept else "") + s[k:]
def locate(s):
    # (first line, end, prefix) of where its own shell_environment_policy.set is written, or None.
    ls = s.split("\n")
    heads = [n for n, l in enumerate(ls) if re.match(r"\s*\[", l)]
    after = lambda n: (n + 1, next((h for h in heads if h > n), len(ls)))
    for name, prefix in (("shell_environment_policy\\s*\\.\\s*set", ""), ("shell_environment_policy", "set.")):
        for n in heads:
            if re.fullmatch(rf"\s*\[\s*{name}\s*\]\s*(#.*)?", ls[n]):
                return (*after(n), prefix)
    top = heads[0] if heads else len(ls)
    first = next((n for n in range(top) if re.match(r"\s*shell_environment_policy\s*\.", ls[n])), None)
    return None if first is None else (first, top, PREFIXES[2])
def check(want, expect, msg):
    try:
        got = tomllib.loads(want)
    except tomllib.TOMLDecodeError as e:
        fail(msg.format(f"{e}"))
    if got != expect:
        fail(msg.format("it would not read back as the guard"))
    return want
def unmerge(s):
    # Our keys merged by hand, unmarked (omabox's own advice before #221), taken out: only when all
    # of them are there with the guard's values, so a key of the user's own stays.
    d = load(s, "is not valid TOML")
    if not has_guard(d):
        return s
    msg = f"{path}: the guard's keys in its shell_environment_policy.set ({{}}): not touched; take them out by hand:\n{by_hand(guard)}"
    loc = locate(s)
    if not loc:
        fail(msg.format("written in a form omabox does not edit"))
    a, z, p = loc
    key = re.compile(r"\s*(" + "|".join(re.escape(p + k) for k in guard) + r")\s*=")
    ls = s.split("\n")
    want = "\n".join(ls[:a] + [l for l in ls[a:z] if not key.match(l)] + ls[z:])
    d["shell_environment_policy"]["set"] = {k: v for k, v in pset(d).items() if k not in guard}
    return check(want, d, msg)
def merge(s):
    d = load(s, "is not valid TOML")
    have = pset(d)
    expect = copy.deepcopy(d)
    pol = expect.setdefault("shell_environment_policy", {})
    if isinstance(pol, dict) and isinstance(pol.setdefault("set", {}), dict):
        pol["set"].update(guard)
    if have is None:
        want = s + ("\n" if s and not s.endswith("\n\n") else "") + ("" if not s or s.endswith("\n") else "\n") + block + "\n"
        return check(want, expect, f"{path}: its own shell_environment_policy clashes with the guard's ({{}}): not touched; add these to its shell_environment_policy.set by hand:\n{by_hand(guard)}")
    if not isinstance(have, dict):
        fail(f"{path}: its shell_environment_policy.set is not a table: not touched")
    # A key of the user's own with another value is theirs: not overwritten. One with the guard's
    # value is theirs too: it stays as it is, outside our block.
    clash = [k for k in guard if k in have and have[k] != guard[k]]
    if clash:
        fail(f"{path}: its own shell_environment_policy.set has " + ", ".join(f"{k} = {json.dumps(have[k])}" for k in clash)
             + f", the guard needs {by_hand(clash).replace(chr(10), ', ')}: not touched; change or remove yours, then omabox guard on")
    missing = [k for k in guard if k not in have]
    msg = f"{path}: could not merge the guard into its own shell_environment_policy.set ({{}}): not touched; add these to it by hand:\n{by_hand(missing)}"
    loc = locate(s)
    if not loc:
        fail(msg.format("written in a form omabox does not edit, an inline table?"))
    a, _, p = loc
    ls = s.split("\n")
    mark = [lines[0]] + [p + l for l in mine[1:] if l.split(" = ")[0] in missing] + [end]
    return check("\n".join(ls[:a] + mark + ls[a:]), expect, msg)
data = load(cur, "is not valid TOML")
b = find(cur)
on = bool(b) and has_guard(data) and all(l in current for l in b[2] if ours(l))
if mode == "state":
    pol = pset(data) if isinstance(pset(data), dict) else {}
    if on or (not b and has_guard(data)):
        state = "on"   # merged by hand counts: `on` only adds its markers
    elif b or pol.get("WAYLAND_DISPLAY") == guard["WAYLAND_DISPLAY"]:
        state = "outdated"
        # A guard written from a checkout that is gone (a removed worktree) no longer stops gh --web.
        stand_in = pol.get("BROWSER")
        if isinstance(stand_in, str) and stand_in.endswith("/share/guard/xdg-open") and not os.access(stand_in, os.X_OK):
            state += f": its {os.path.dirname(stand_in)} is gone, so links open on the desktop again"
    else:
        state = "off"
    print(f"STATE Codex ({path}): {state}")
    sys.exit()
base = unmerge(strip(cur))
want = cur if mode == "on" and on else merge(base) if mode == "on" else base
if want == cur:
    print("SAME")
else:
    print("WANT"); sys.stdout.write(want + ".")   # the dot keeps $(...) from eating trailing newlines
