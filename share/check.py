#!/usr/bin/env python3
"""omabox check's helper (#252, finding 357): what `bin/omabox` asks of a diff and of two trees.

  check.py regions SPECK GAP < VERBOSE
      VERBOSE: magick's -connected-components verbose listing of a difference mask (white where
      two shots differ). Parts under SPECK pixels are specks (distance-field text varies by a few
      pixels a glyph from one start of an app to the next: never a part over 13 px measured), the
      rest are merged when GAP px apart or less. Prints "X,Y WxH PIXELS" a region, then
      "specks N PIXELS".
  check.py tree BASE.json NEW.json [IGNORE]
      Two `omabox tree --json` dumps of one window, element by element (by place: see load):
      moved, resized, name, text, value or state changed, appeared, vanished. IGNORE: a file of element keys to leave out
      (what differed between the baseline's two runs). Prints a line a change; exit 1 when any.
  check.py tree-keys BASE.json NEW.json
      The keys of the elements that differ, "KEY  (role "name")" a line (for IGNORE); exit 0.
  check.py sheet PLAN OUT.png [FONT]
      The report of a check (finding 360): one image of every change, each region cropped with some
      context from the baseline and the new shot side by side, inside what a model sees in one look.
      PLAN: tab-separated lines, "title TEXT", then per changed checkpoint "point ID BASE.png NEW.png
      BASE.tree.json NEW.tree.json UNSTABLE" (empty fields: none) and its "region X Y W H" or "size
      TEXT" lines. Prints magick's arguments NUL-separated, the first being what was left out ("" when
      nothing).
"""
import json
import sys


def regions(speck, gap):
    boxes, specks, speck_px = [], 0, 0
    for line in sys.stdin:
        f = line.split()
        # "  12: 5x3+100+40 102.0,41.0 9 gray(255)": id, geometry, centroid, area, colour (white:
        # gray(255), graya(255,...), srgb(255,...), white; the background is 0).
        if len(f) < 5 or not f[0].endswith(":"):
            continue
        c = f[-1]
        if not (c == "white" or c.split("(", 1)[-1].split(",")[0].rstrip(")") in ("255", "100%")):
            continue
        w, rest = f[1].split("x", 1)
        h, x, y = rest.split("+")
        area = int(float(f[3]))
        if area < speck:
            specks += 1
            speck_px += area
            continue
        boxes.append([int(x), int(y), int(x) + int(w), int(y) + int(h), area])
    merged = True
    while merged:
        merged = False
        out = []
        for b in boxes:
            for o in out:
                if b[0] <= o[2] + gap and o[0] <= b[2] + gap and b[1] <= o[3] + gap and o[1] <= b[3] + gap:
                    o[0], o[1] = min(o[0], b[0]), min(o[1], b[1])
                    o[2], o[3] = max(o[2], b[2]), max(o[3], b[3])
                    o[4] += b[4]
                    merged = True
                    break
            else:
                out.append(b)
        boxes = out
    for b in sorted(boxes, key=lambda b: (b[1], b[0])):
        print(f"{b[0]},{b[1]} {b[2] - b[0]}x{b[3] - b[1]} {b[4]}")
    print(f"specks {specks} {speck_px}")


def load(path):
    """The tree as {key: node}, a key being the node's place: role#N from the window down, N its
    count among its parent's children of that role. A name is not in it, so a label whose text
    changes is that label changed, not one gone and another come; an element put in before others of
    its role shifts theirs."""
    with open(path, encoding="utf-8") as f:
        nodes = json.load(f)
    out, stack, counts = {}, [], []
    for n in nodes:
        d = n["depth"]
        del stack[d:]
        while len(stack) < d:
            stack.append("?")
        del counts[d + 1:]
        while len(counts) <= d:
            counts.append({})
        i = counts[d].get(n["role"], 0) + 1
        counts[d][n["role"]] = i
        stack.append(f"{n['role']}#{i}")
        out["/".join(stack)] = n
    return out


def label(n):
    return n["role"] + (f' "{n["name"]}"' if n.get("name") else "")


def at(n):
    return f"@{n['x']},{n['y']} {n['w']}x{n['h']}" if "x" in n else ""


def changes(base, new):
    """(key, line) for each difference, in the base's order, then what appeared."""
    for key, b in base.items():
        n = new.get(key)
        if n is None:
            yield key, f"vanished  {label(b)}" + (f"  (was {at(b)})" if "x" in b else "")
            continue
        if b.get("name", "") != n.get("name", ""):
            yield key, f"name  {b['role']}  {json.dumps(b.get('name', ''))} -> {json.dumps(n.get('name', ''))}"
        short = label(n)
        if "x" in b and "x" in n:
            dx, dy = n["x"] - b["x"], n["y"] - b["y"]
            dw, dh = n["w"] - b["w"], n["h"] - b["h"]
            what = []
            if dx or dy:
                what.append(f"moved {dx:+d},{dy:+d}")
            if dw or dh:
                what.append(f"resized {dw:+d}x{dh:+d}")
            if what:
                yield key, f"{' '.join(what)}  {short}  {at(n)}"
        for field in ("text", "value"):
            if b.get(field, "") != n.get(field, ""):
                yield key, f"{field}  {short}  {json.dumps(b.get(field, ''))} -> {json.dumps(n.get(field, ''))}"
        if b.get("states", []) != n.get("states", []):
            yield key, f"state  {short}  [{','.join(b.get('states', []))}] -> [{','.join(n.get('states', []))}]"
    for key, n in new.items():
        if key not in base:
            yield key, f"appeared  {label(n)}  {at(n)}".rstrip()


# The report (finding 360): what a model sees in one look is 1568 px a side and ~1.15 MP (finding
# 243), so the sheet is 1456 px wide at most and its area under that; each side of a pair 706 px wide
# and 360 high at most, a region cropped with CTX px around it, crops of one checkpoint merged while
# they are near, a small crop shown 2x (a 2 px move is 4 there). More than CROPS_MAX crops in one
# checkpoint: the whole shots instead (a change of everything reads better as one picture).
SHEET_W, SHEET_PX, SHEET_H = 1456, 1150000, 1568
HALF_W, HALF_H = 706, 360
CTX, CROPS_MAX = 24, 6
ELEMENT_W, ELEMENT_H = 400, 120   # a region's element in its crop up to this
PAD, GAP, SEP = 6, 10, 8
PT, LH, CW = 14, 18, 8.5          # pointsize, line height, a monospace character's width at it
TEXT_MAX = 560                    # a card wider than its pair for its text, up to this
RED, BLUE, INK = "#ff2020", "#1a6fe0", "#111111"


def png_size(path):
    with open(path, "rb") as f:
        head = f.read(24)
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"not a PNG: {path}")
    return int.from_bytes(head[16:20], "big"), int.from_bytes(head[20:24], "big")


def overlaps(a, b, slack=0):
    return a[0] < b[0] + b[2] + slack and b[0] < a[0] + a[2] + slack and a[1] < b[1] + b[3] + slack and b[1] < a[1] + a[3] + slack


def bounds(n):
    return (n["x"], n["y"], n["w"], n["h"]) if n and "x" in n else None


def union(a, b):
    x0, y0 = min(a[0], b[0]), min(a[1], b[1])
    return (x0, y0, max(a[0] + a[2], b[0] + b[2]) - x0, max(a[1] + a[3], b[1] + b[3]) - y0)


def node_line(n):
    st = f" [{' '.join(n['states'])}]" if n.get("states") else ""
    return f"{label(n)}{st} {at(n)}"


def clean(text):
    """One printable line (names come from the app)."""
    return "".join(c if c.isprintable() else " " for c in text)


def fit(text, width):
    n = max(4, int(width // CW))
    return text if len(text) <= n else text[: n - 1] + "…"


def marks_of(pt):
    """The checkpoint's marks: [box, colour, tree line or "", the element's box or None] for each
    region, then for each tree change with bounds no region covers (a move under a speck's size
    shows only there)."""
    marks = [[r, RED, "", None] for r in pt["regions"]]
    bt, nt = pt["trees"]
    if not (bt and nt):
        return marks
    base, new = load(bt), load(nt)
    root = next(iter(new.values()), None)
    if not root or (root.get("w"), root.get("h")) != pt["size"]:
        return marks   # the tree's pixels are not the shot's (a scale): no marks from it
    ignore = set()
    if pt["unstable"]:
        with open(pt["unstable"], encoding="utf-8") as f:
            ignore = {l[5:].rstrip("\n").split("  (", 1)[0] for l in f if l.startswith("tree ")}
    changed = {}
    for key, line in changes(base, new):
        if key in ignore or key in changed:
            continue
        b, n = bounds(base.get(key)), bounds(new.get(key))
        if b or n:
            changed[key] = (union(b, n) if b and n else (b or n), line)
    for m in marks:
        hits = [c for c in changed.values() if overlaps(c[0], m[0])]
        if hits:
            m[3], m[2] = min(hits, key=lambda c: c[0][2] * c[0][3])
        else:
            els = [n for n in new.values() if n.get("depth", 0) > 0 and bounds(n) and overlaps(bounds(n), m[0])]
            if els:
                n = min(els, key=lambda n: n["w"] * n["h"])
                m[2], m[3] = node_line(n), bounds(n)
    for box, line in changed.values():
        if box[2] > 0 and box[3] > 0 and not any(overlaps(box, r, 2) for r in pt["regions"]):
            marks.append([box, BLUE, line, box])
    return marks


def crops_of(pt, marks):
    """Boxes around the marks, CTX px of context, merged while they are under CTX apart and fit one
    side (a card's lines cost more than the pixels between two near crops). A mark's element is in
    its crop when that stays small (the whole label whose last word changed)."""
    W, H = pt["size"]
    boxes = []
    for box, _, _, el in marks:
        if el:
            u = union(box, el)
            if u[2] <= ELEMENT_W and u[3] <= ELEMENT_H:
                box = u
        x, y, w, h = box
        x0, y0 = max(0, x - CTX), max(0, y - CTX)
        boxes.append((x0, y0, min(W, x + w + CTX) - x0, min(H, y + h + CTX) - y0))
    merged = True
    while merged:
        merged = False
        out = []
        for b in boxes:
            for i, o in enumerate(out):
                u = union(o, b)
                if overlaps(o, b, CTX) and u[2] <= HALF_W and u[3] <= HALF_H:
                    out[i] = u
                    merged = True
                    break
            else:
                out.append(b)
        boxes = out
    return sorted(boxes, key=lambda b: (b[1], b[0]))


def card(pt, box, marks, whole=False, lim=1):
    """A card: its lines, its pair's box and scale, its size."""
    cx, cy, cw, ch = box
    s = min(HALF_W * lim / cw, HALF_H * lim / ch)
    s = 2 if s >= 2 and cw <= 160 and ch <= 120 else (1 if s >= 1 else s)
    sw, sh = max(1, round(cw * s)), max(1, round(ch * s))
    inside = [m for m in marks if overlaps(m[0], box)]
    regs = [m for m in inside if m[1] == RED]
    if pt["sizechange"]:
        head = f"{pt['id']}: size {pt['sizechange']} (both whole, at {round(s * 100)}%)"
    elif whole:
        head = f"{pt['id']}: {len(regs)} regions (the whole shots, at {round(s * 100)}%)"
    elif regs:
        head = f"{pt['id']}: {f'{len(regs)} regions, ' if len(regs) > 1 else ''}" + ", ".join(
            f"at {x},{y} {w}x{h}" for (x, y, w, h), *_ in regs)
    else:
        head = f"{pt['id']}: in the tree only (under a speck's size in pixels)"
    lines = [(clean(head), INK)]
    seen = []
    for _, colour, line, _ in inside:
        if line and line not in seen:
            seen.append(line)
            if len(lines) < 4:
                lines.append(("tree: " + clean(line), colour))
    if len(seen) > 3:
        lines[-1] = (f"tree: +{len(seen) - 2} more (check run's output lists them)", INK)
    pair = 2 * sw + SEP
    textw = max(len(t) for t, _ in lines) * CW
    w = max(pair, min(textw, TEXT_MAX)) + 2 * PAD
    h = PAD + len(lines) * LH + 4 + sh + PAD
    return {"pt": pt, "box": box, "s": s, "sw": sw, "sh": sh, "marks": inside, "lines": lines, "w": int(w), "h": int(h)}


def wrap(text, width):
    """Lines of at most width px, broken at spaces."""
    n, lines = max(8, int(width // CW)), [""]
    for word in text.split(" "):
        if lines[-1] and len(lines[-1]) + 1 + len(word) > n:
            lines.append("")
        lines[-1] += (" " if lines[-1] else "") + word
    return lines


def layout(cards, footer, title):
    """Rows, left to right: [(card, x, y)], the sheet's width and height, and the title's lines
    (wrapped to the cards' width, 360 px at least: a legend wider than a small report costs more
    than its lines)."""
    x, y, rowh, out, W = GAP, 0, 0, [], 360
    for c in cards:
        if x > GAP and x + c["w"] + GAP > SHEET_W:
            x, y, rowh = GAP, y + rowh + GAP, 0
        out.append([c, x, y])
        x += c["w"] + GAP
        rowh = max(rowh, c["h"])
        W = max(W, x)
    lines = wrap(clean(title), W - 2 * GAP)
    top = GAP + len(lines) * LH + 4
    for o in out:
        o[2] += top
    H = top + y + rowh + GAP + footer * LH
    return out, W, H, lines


def corners(x0, y0, x1, y1):
    """A mark's four corners, not its whole box: its edges would hide what is beside the change (the
    digit before a time's changed ones)."""
    ax, ay = max(3, min(12, (x1 - x0) // 4)), max(3, min(12, (y1 - y0) // 4))
    return " ".join(f"line {x},{y} {x + dx * ax},{y} line {x},{y} {x},{y + dy * ay}"
                    for x, y, dx, dy in ((x0, y0, 1, 1), (x1, y0, -1, 1), (x0, y1, 1, -1), (x1, y1, -1, -1)))


def esc(text):
    """-annotate reads % and \\ as escapes, and a text starting with @ as a file's name."""
    text = text.replace("\\", "\\\\").replace("%", "%%")
    return " " + text if text.startswith("@") else text


def cards_of(points, marks, lim):
    """Each checkpoint's cards, in order, each side of a pair at most lim of HALF_W x HALF_H."""
    per = []
    for pt, mk in zip(points, marks):
        if pt["sizechange"]:
            bw, bh = png_size(pt["base"])
            nw, nh = pt["size"]
            # Both at one scale, so their sizes compare: the box is the bigger of the two.
            per.append([card(pt, (0, 0, max(bw, nw), max(bh, nh)), [], lim=lim)])
            continue
        boxes = crops_of(pt, mk)
        if len(boxes) > CROPS_MAX:
            per.append([card(pt, (0, 0) + pt["size"], mk, whole=True, lim=lim)])
        else:
            per.append([card(pt, b, mk, lim=lim) for b in boxes])
    return per


def choose(per, title):
    """Round by round (each checkpoint's first card, then its second...), the cards that fit, as
    {(checkpoint, card)}; laid out in order."""
    keep = set()
    for r in range(max((len(c) for c in per), default=0)):
        for i, cs in enumerate(per):
            if r >= len(cs):
                continue
            trial = keep | {(i, r)}
            _, W, H, _ = layout([c for j, cs2 in enumerate(per) for k, c in enumerate(cs2) if (j, k) in trial], 2, title)
            if W <= SHEET_W and H <= SHEET_H and W * H <= SHEET_PX:
                keep = trial
    return keep


def sheet(plan, out_png, font):
    title, points = "", []
    with open(plan, encoding="utf-8") as f:
        for line in f:
            p = line.rstrip("\n").split("\t")
            if p[0] == "title":
                title = p[1]
            elif p[0] == "point":
                points.append({"id": p[1], "base": p[2], "new": p[3], "trees": (p[4], p[5]), "unstable": p[6],
                               "regions": [], "sizechange": ""})
                points[-1]["size"] = png_size(p[3])
            elif p[0] == "region":
                points[-1]["regions"].append(tuple(int(v) for v in p[1:5]))
            elif p[0] == "size":
                points[-1]["sizechange"] = p[1]
    marks = [marks_of(pt) if not pt["sizechange"] else [] for pt in points]
    # At full size first; when that leaves some out, each side at half the size, if that shows more.
    best = None
    for lim in (1, 0.5):
        per = cards_of(points, marks, lim)
        keep = choose(per, title)
        if best is None or len(keep) > len(best[1]):
            best = (per, keep)
        if len(keep) == sum(len(cs) for cs in per):
            break
    per, keep = best
    shown = [c for j, cs in enumerate(per) for k, c in enumerate(cs) if (j, k) in keep]
    left = []
    for j, cs in enumerate(per):
        n = sum(1 for k in range(len(cs)) if (j, k) not in keep)
        if n == len(cs):
            left.append(cs[0]["pt"]["id"])
        elif n:
            left.append(f"{cs[0]['pt']['id']} ({n} of {len(cs)} crops)")
    left_text = ", ".join(left)
    placed, W, H, tlines = layout(shown, 2 if left else 0, title)
    a = ["-size", f"{W}x{H}", "xc:white"] + (["-font", font] if font else []) + ["-pointsize", str(PT)]

    def text(x, y, t, colour):
        a.extend(["-stroke", "none", "-fill", colour, "-annotate", f"+{x}+{y + LH - 5}", esc(t)])

    for i, t in enumerate(tlines):
        text(GAP, GAP + i * LH, t, INK)
    for c, x, y in placed:
        a.extend(["-stroke", "none", "-fill", "#e6e6e6", "-draw", f"rectangle {x},{y} {x + c['w'] - 1},{y + c['h'] - 1}"])
        for i, (t, colour) in enumerate(c["lines"]):
            text(x + PAD, y + PAD + i * LH, fit(t, c["w"] - 2 * PAD), colour)
        iy = y + PAD + len(c["lines"]) * LH + 4
        cx, cy, cw, ch = c["box"]
        s = c["s"]
        for k, path in enumerate((c["pt"]["base"], c["pt"]["new"])):
            ix = x + PAD + k * (c["sw"] + SEP)
            if c["pt"]["sizechange"]:
                iw, ih = png_size(path)
                geo, crop = f"{max(1, round(iw * s))}x{max(1, round(ih * s))}!", []
            else:
                geo, crop = f"{c['sw']}x{c['sh']}!", ["-crop", f"{cw}x{ch}+{cx}+{cy}", "+repage"]
            a.extend(["(", path, "-alpha", "off"] + crop + (["-scale", geo] if s >= 1 else ["-resize", geo]))
            a.extend(["-fill", "none", "-strokewidth", "2"])
            for (mx, my, mw, mh), colour, *_ in c["marks"]:
                x0, y0 = int((mx - cx) * s) - 3, int((my - cy) * s) - 3
                x1, y1 = int((mx + mw - cx) * s + 0.999) + 2, int((my + mh - cy) * s + 0.999) + 2
                a.extend(["-stroke", colour, "-draw", corners(x0, y0, x1, y1)])
            a.extend([")", "-geometry", f"+{ix}+{iy}", "-composite"])
    if left:
        fy = H - GAP - 2 * LH + 4
        text(GAP, fy, fit("not shown, no room (what changed there is in check's output):", W - 2 * GAP), INK)
        text(GAP, fy + LH, fit(left_text, W - 2 * GAP), INK)
    a.extend(["-alpha", "off", "-depth", "8", f"PNG:{out_png}"])
    sys.stdout.write("\0".join([left_text] + a) + "\0")


def main():
    if len(sys.argv) >= 4 and sys.argv[1] == "regions":
        regions(int(sys.argv[2]), int(sys.argv[3]))
        return 0
    if len(sys.argv) in (4, 5) and sys.argv[1] in ("tree", "tree-keys"):
        base, new = load(sys.argv[2]), load(sys.argv[3])
        ignore = set()
        if len(sys.argv) == 5:
            with open(sys.argv[4], encoding="utf-8") as f:
                # "KEY  (what it is)" lines: the key is before the two spaces.
                ignore = {line.rstrip("\n").split("  (", 1)[0] for line in f if line.strip()}
        found = 0
        keys = []
        what = {}
        for key, line in changes(base, new):
            what.setdefault(key, label(base.get(key) or new[key]))
            if key in ignore:
                continue
            found += 1
            if sys.argv[1] == "tree":
                print(line)
            elif key not in keys:
                keys.append(key)
        for key in keys:
            print(f"{key}  ({what[key]})")
        return 1 if found and sys.argv[1] == "tree" else 0
    if len(sys.argv) in (4, 5) and sys.argv[1] == "sheet":
        sheet(sys.argv[2], sys.argv[3], sys.argv[4] if len(sys.argv) == 5 else "")
        return 0
    print(__doc__.strip(), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
