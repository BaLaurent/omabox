#!/usr/bin/env python3
"""spike/visual/treediff.py BASE.tree NEW.tree

Two `omabox tree` dumps of one window, element by element: what moved, resized, appeared,
vanished or changed state. An element is known by its path of role "name" from the frame down,
with an index among same-path siblings, so two "all" list items stay two.
"""
import re
import sys

LINE = re.compile(r'^(?P<indent> *)(?P<role>[a-z][a-z ]*?)(?: "(?P<name>(?:[^"\\]|\\.)*)")?'
                  r'(?: \[(?P<states>[^\]]*)\])?(?: @(?P<x>-?\d+),(?P<y>-?\d+) (?P<w>\d+)x(?P<h>\d+))?\s*$')


def parse(path):
    out, stack, seen = {}, [], {}
    for raw in open(path, encoding="utf-8"):
        m = LINE.match(raw.rstrip("\n"))
        if not m:
            continue
        depth = len(m["indent"]) // 2
        label = m["role"] + (f' "{m["name"]}"' if m["name"] is not None else "")
        stack[depth:] = [label]
        key = " > ".join(stack)
        n = seen.get(key, 0)
        seen[key] = n + 1
        if n:
            key += f" #{n + 1}"
        box = tuple(int(m[k]) for k in "xywh") if m["x"] is not None else None
        out[key] = (box, m["states"] or "")
    return out


def main():
    base, new = parse(sys.argv[1]), parse(sys.argv[2])
    found = 0
    for key, (box, states) in base.items():
        short = key.split(" > ")[-1]
        if key not in new:
            print(f"vanished  {short}  (was @{box[0]},{box[1]} {box[2]}x{box[3]})" if box
                  else f"vanished  {short}")
            found += 1
            continue
        nbox, nstates = new[key]
        if box and nbox and box != nbox:
            dx, dy, dw, dh = (nbox[i] - box[i] for i in range(4))
            what = []
            if dx or dy:
                what.append(f"moved {dx:+d},{dy:+d}")
            if dw or dh:
                what.append(f"resized {dw:+d}x{dh:+d}")
            print(f"{' '.join(what):<22}{short}  @{nbox[0]},{nbox[1]} {nbox[2]}x{nbox[3]}")
            found += 1
        if states != nstates:
            print(f"state     {short}  [{states}] -> [{nstates}]")
            found += 1
    for key, (box, _) in new.items():
        if key not in base:
            short = key.split(" > ")[-1]
            print(f"appeared  {short}" + (f"  @{box[0]},{box[1]} {box[2]}x{box[3]}" if box else ""))
            found += 1
    sys.exit(1 if found else 0)


if __name__ == "__main__":
    main()
