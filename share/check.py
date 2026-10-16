#!/usr/bin/env python3
"""omabox check's helper (#252, finding 357): what `bin/omabox` asks of a diff and of two trees.

  check.py regions SPECK GAP < VERBOSE
      VERBOSE: magick's -connected-components verbose listing of a difference mask (white where
      two shots differ). Parts under SPECK pixels are specks (distance-field text varies by a few
      pixels a glyph from one start of an app to the next: never a part over 13 px measured), the
      rest are merged when GAP px apart or less. Prints "X,Y WxH PIXELS" a region, then
      "specks N PIXELS".
  check.py holes screen|window W H CORNER GAP [X Y RW RH] < VERBOSE
      VERBOSE: the listing of a W x H mask, white where a shot is see-through (the sentinel
      wallpaper in a screen shot, alpha 0 in a window's own pixels). A part touching a screen
      shot's edge is the open wallpaper (around and between the surfaces); one whose box is at most
      CORNER px a side and that fills under half of it is a rounded corner (outside an arc: 21% of
      its square). The rest are holes, merged when GAP px apart or less. X Y RW RH: only what is
      inside that region, in its own pixels (a -g checkpoint). Prints "X,Y WxH PIXELS" a hole,
      then "corners N".
  check.py tree BASE.json NEW.json [IGNORE]
      Two `omabox tree --json` dumps of one window, element by element (by place: see load):
      moved, resized, name, text, value or state changed, appeared, vanished. IGNORE: a file of element keys to leave out
      (what differed between the baseline's two runs). Prints a line a change; exit 1 when any.
  check.py tree-keys BASE.json NEW.json
      The keys of the elements that differ, "KEY  (role "name")" a line (for IGNORE); exit 0.
"""
import json
import sys


def parts(lines):
    """The white parts of a -connected-components verbose listing, as [x0, y0, x1, y1, area]."""
    for line in lines:
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
        yield [int(x), int(y), int(x) + int(w), int(y) + int(h), int(float(f[3]))]


def merge(boxes, gap):
    """Boxes GAP px apart or less as one, their areas added."""
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
    return sorted(boxes, key=lambda b: (b[1], b[0]))


def show(b):
    print(f"{b[0]},{b[1]} {b[2] - b[0]}x{b[3] - b[1]} {b[4]}")


def regions(speck, gap):
    boxes, specks, speck_px = [], 0, 0
    for b in parts(sys.stdin):
        if b[4] < speck:
            specks += 1
            speck_px += b[4]
            continue
        boxes.append(b)
    for b in merge(boxes, gap):
        show(b)
    print(f"specks {specks} {speck_px}")


def holes(kind, width, height, corner, gap, region):
    boxes, corners = [], 0
    for b in parts(sys.stdin):
        w, h = b[2] - b[0], b[3] - b[1]
        # The wallpaper around the surfaces and in the gaps between them reaches the screen's edge.
        if kind == "screen" and (b[0] == 0 or b[1] == 0 or b[2] == width or b[3] == height):
            continue
        if w <= corner and h <= corner and 2 * b[4] < w * h:
            corners += 1
            continue
        boxes.append(b)
    if region:
        rx, ry, rw, rh = region
        inside = []
        for b in boxes:
            x0, y0 = max(b[0], rx), max(b[1], ry)
            x1, y1 = min(b[2], rx + rw), min(b[3], ry + rh)
            if x0 >= x1 or y0 >= y1:
                continue
            # A hole the region cuts: its pixels counted as its share of its box.
            share = (x1 - x0) * (y1 - y0) / ((b[2] - b[0]) * (b[3] - b[1]))
            inside.append([x0 - rx, y0 - ry, x1 - rx, y1 - ry, max(1, round(b[4] * share))])
        boxes = inside
    for b in merge(boxes, gap):
        show(b)
    print(f"corners {corners}")


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


def main():
    if len(sys.argv) >= 4 and sys.argv[1] == "regions":
        regions(int(sys.argv[2]), int(sys.argv[3]))
        return 0
    if len(sys.argv) in (7, 11) and sys.argv[1] == "holes" and sys.argv[2] in ("screen", "window"):
        n = [int(a) for a in sys.argv[3:]]
        holes(sys.argv[2], n[0], n[1], n[2], n[3], n[4:] or None)
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
    print(__doc__.strip(), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
