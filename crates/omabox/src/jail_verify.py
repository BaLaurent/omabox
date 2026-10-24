import json, os, re, sys, time
jpid, jns, bpid, cfile, cdir, pol = sys.argv[1:7]
srcs = [s.encode() for s in sys.argv[7:]]
if not (jpid.isdigit() and jns and bpid.isdigit()):
    sys.exit("no jail process to read its mounts from")
pol = json.loads(pol)
roots = {r["path"].encode() for r in pol["roots"]}
listed = [m.encode() for m in pol.get("masks", [])]
covers = {tuple(int(x) for x in c.split(":")) for c in (cfile, cdir)}
# Each folder as the host has it (one of the jail's: a mount point there, which the jail cannot move).
src_ids = {}
for s in srcs:
    st = os.stat(s)
    src_ids[s] = (st.st_dev, st.st_ino)
P = os.O_PATH | os.O_NOFOLLOW | os.O_CLOEXEC

def mounts(pid):
    with open(f"/proc/{pid}/mountinfo", "rb") as f:
        data = f.read()
    out = []
    for line in data.split(b"\n"):
        f = line.split(b" ")
        if len(f) > 4:
            out.append((int(f[0]), int(f[1]), re.sub(rb"\\([0-7]{3})", lambda m: bytes([int(m.group(1), 8)]), f[4])))
    return out

def mnt_id(fd):
    with open(f"/proc/self/fdinfo/{fd}") as f:
        for line in f:
            if line.startswith("mnt_id:"):
                return int(line.split()[1])
    return -1

def place(root, path):
    # (device, inode of the folder holding PATH, its name), the mount that folder is seen on, and an
    # O_PATH fd of what is there (mounts followed, a link not), walked from ROOT one name at a time,
    # never through a link.
    parts = path.split(b"/")[1:]
    if not parts or any(c in (b"", b".", b"..") for c in parts):
        raise OSError("not a plain path")
    fd = os.open(".", P | os.O_DIRECTORY, dir_fd=root)
    try:
        for c in parts[:-1]:
            n = os.open(c, P | os.O_DIRECTORY, dir_fd=fd)
            os.close(fd)
            fd = n
        st = os.fstat(fd)
        return (st.st_dev, st.st_ino, parts[-1]), mnt_id(fd), os.open(parts[-1], P, dir_fd=fd)
    finally:
        os.close(fd)

def under(p, dirs):
    return any(p.startswith(d + b"/") for d in dirs)

def hidden():
    # {place: (path, the folders SRCS holding it)}: what the jail hides in them, from its own mounts.
    root = os.open(f"/proc/{jpid}/root", os.O_PATH | os.O_CLOEXEC)
    try:
        ms = mounts(jpid)
        if os.readlink(f"/proc/{jpid}/ns/mnt") != jns:
            sys.exit("the jail's process is gone")
        mask = {m[0] for m in ms if m[2] not in roots and under(m[2], roots)}
        res = {}
        for m in ms:
            # Its own (not one inside another hidden one, whose cover hides it), in a folder the box mounts.
            if m[0] not in mask or m[1] in mask or not under(m[2], srcs):
                continue
            stack, grew = {m[0]}, True
            while grew:
                grew = False
                for x in ms:
                    if x[0] not in stack and x[1] in stack and x[2] == m[2]:
                        stack.add(x[0])
                        grew = True
            pl, _, fd = place(root, m[2])
            try:
                if mnt_id(fd) not in stack:
                    raise OSError("moved")
            finally:
                os.close(fd)
            res[pl] = (m[2], [s for s in srcs if m[2].startswith(s + b"/")])
        # A folder the policy says hides paths, where the jail's mounts show none: not trusted.
        for s in srcs:
            if any(m.startswith(s + b"/") for m in listed) and not any(s in v[1] for v in res.values()):
                sys.exit(f"the jail's mounts show nothing hidden in {s.decode(errors='replace')}")
        return res
    finally:
        os.close(root)

def covered():
    # The box's covered places, each with the mount its folder is seen on, and the box's mounts of
    # each folder SRC (each one is checked: a folder mounted twice is two ways in).
    root = os.open(f"/proc/{bpid}/root", os.O_PATH | os.O_CLOEXEC)
    try:
        res, of = set(), {s: set() for s in srcs}
        for m in mounts(bpid):
            if m[2] == b"/":
                continue
            # A mount that can't be walked could be one of SRC's: not checked is not covered.
            pl, on, fd = place(root, m[2])
            try:
                st, top = os.fstat(fd), mnt_id(fd)
            finally:
                os.close(fd)
            if (st.st_dev, st.st_ino) in covers:
                res.add((on,) + pl)
            if top == m[0]:
                for s, i in src_ids.items():
                    if (st.st_dev, st.st_ino) == i:
                        of[s].add(m[0])
        return res, of
    finally:
        os.close(root)

missing = [b"(the jail's mounts could not be walked)"]
for attempt in range(6):
    try:
        h = hidden()
        c, of = covered()
        missing = []
        for pl, (path, ss) in h.items():
            for s in ss:
                if not of[s]:
                    missing.append(path + b" (no mount of its folder found in the box)")
                missing += [path for mid in of[s] if (mid,) + pl not in c]
        if not missing:
            sys.exit(0)
    except OSError:
        pass
    time.sleep(0.05 * (attempt + 1))
for p in dict.fromkeys(missing):
    print(p.decode(errors="replace").replace("\n", "?"))
sys.exit(1)
