#!/usr/bin/env python3
# omabox's journal stand-in (#234), started by share/session.sh in every box: a box has no journald,
# and a program that logs only to the journal failed there (dbus-broker-launch exits 1 when it cannot
# connect to /run/systemd/journal/socket, so a bus a program starts with a cleaned environment never
# came up). Two sockets in the box's own /run/systemd/journal (a tmpfs of the box's, never the host's):
#
#   socket   the native protocol: datagrams of KEY=value\n fields (or KEY\n, a 64-bit little-endian
#            length, the value, \n), a big entry as a memfd passed with SCM_RIGHTS
#   stdout   the stream protocol (systemd-cat, a unit's StandardOutput=journal): seven header lines
#            (identifier, unit, priority, level prefix, three forward flags), then the program's text
#
# Each entry's MESSAGE goes to ~/journal.log as `DATE TIME IDENTIFIER[PID]: MESSAGE`, one line per
# line of the message (`omabox log journal`); other fields are dropped. The log is bounded: past
# --max bytes it is renamed journal.1.log (the one before is dropped) and started again, so a chatty
# program costs at most twice that. Nothing here waits on a client, and a log that cannot be written
# loses the line, not the box.
#
#   journal.py [--dir DIR] [--log FILE] [--max BYTES]
import os
import selectors
import socket
import struct
import sys
import time

MAX_MSG = 64 * 1024   # of one entry's message, in the log
MAX_STREAMS = 64      # stdout connections at once; more wait in the backlog


def args():
    o = {"dir": "/run/systemd/journal", "log": os.path.expanduser("~/journal.log"), "max": 4 << 20}
    a = sys.argv[1:]
    while a:
        k = a.pop(0)
        if k in ("--dir", "--log", "--max") and a:
            v = a.pop(0)
            o[k[2:]] = int(v) if k == "--max" else v
        else:
            sys.exit(f"journal.py: unknown argument {k} (journal.py [--dir DIR] [--log FILE] [--max BYTES])")
    return o


class Log:
    def __init__(self, path, cap):
        self.path, self.cap, self.f = path, cap, None
        # journal.1.log: a name ending in .log, which `omabox save` leaves out as it does the others
        self.old = (path[:-4] if path.endswith(".log") else path) + ".1.log"
        try:   # there from the start: `omabox log journal` prints it empty rather than missing
            self.f = open(path, "ab")
        except OSError:
            pass

    def write(self, ident, pid, msg):
        if isinstance(msg, bytes):
            msg = msg.decode("utf-8", "replace")
        msg = msg[:MAX_MSG].rstrip("\n")
        if not msg:
            return
        who = f"{ident or 'unknown'}[{pid}]" if pid else (ident or "unknown")
        stamp = time.strftime("%F %T")
        text = "".join(f"{stamp} {who}: {line}\n" for line in msg.split("\n")).encode("utf-8", "replace")
        try:
            if self.f is None:
                self.f = open(self.path, "ab")
            if self.f.tell() and self.f.tell() + len(text) > self.cap:
                self.f.close()
                self.f = None
                os.replace(self.path, self.old)
                self.f = open(self.path, "ab")
            self.f.write(text)
            self.f.flush()
        except OSError:
            if self.f is not None:
                try:
                    self.f.close()
                except OSError:
                    pass
            self.f = None


def comm(pid):
    try:
        with open(f"/proc/{pid}/comm") as f:
            return f.read().strip()
    except OSError:
        return ""


def fields(data):
    """The native protocol's fields, as a dict of bytes."""
    out, i, n = {}, 0, len(data)
    while i < n:
        nl = data.find(b"\n", i)
        if nl < 0:
            nl = n
        line = data[i:nl]
        eq = line.find(b"=")
        if eq >= 0:
            out[line[:eq].decode("ascii", "replace")] = line[eq + 1:]
            i = nl + 1
        else:
            # KEY\n, then a little-endian 64-bit length, the value and a newline.
            if nl + 9 > n:
                break
            (size,) = struct.unpack_from("<Q", data, nl + 1)
            start = nl + 9
            out[line.decode("ascii", "replace")] = data[start:start + size]
            i = start + size + 1
    return out


def entry(log, data, pid):
    f = fields(data)
    msg = f.get("MESSAGE")
    if msg is None:
        return
    spid = f.get("SYSLOG_PID", b"").decode("ascii", "replace")
    pid = pid or (int(spid) if spid.isdigit() else 0)
    ident = f.get("SYSLOG_IDENTIFIER", b"").decode("utf-8", "replace") or (comm(pid) if pid else "")
    log.write(ident, pid, msg)


def recv_native(sock, log):
    while True:
        try:
            data, anc, _, _ = sock.recvmsg(1 << 18, socket.CMSG_SPACE(16 * 4) + socket.CMSG_SPACE(12))
        except (BlockingIOError, InterruptedError):
            return
        except OSError:
            return
        pid, fds = 0, []
        for level, kind, payload in anc:
            if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
                fds += list(struct.unpack(f"{len(payload) // 4}i", payload[: len(payload) // 4 * 4]))
            elif level == socket.SOL_SOCKET and kind == socket.SCM_CREDENTIALS and len(payload) >= 12:
                pid = struct.unpack_from("i", payload)[0]
        try:
            # A big entry: an empty datagram and the entry in a sealed memfd (read from its start).
            if not data and len(fds) == 1:
                data = os.pread(fds[0], MAX_MSG + 4096, 0)
        except OSError:
            data = b""
        for fd in fds:
            os.close(fd)
        if data:
            entry(log, data, pid)


class Stream:
    """One stdout connection: its header, then its text, line by line."""

    def __init__(self, conn):
        self.conn, self.buf, self.head = conn, b"", []
        try:
            creds = conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
            self.pid = struct.unpack("3i", creds)[0]
        except OSError:
            self.pid = 0

    def feed(self, log, data):
        self.buf += data
        while True:
            nl = self.buf.find(b"\n")
            if nl < 0:
                break
            line, self.buf = self.buf[:nl], self.buf[nl + 1:]
            if len(self.head) < 7:
                self.head.append(line.decode("utf-8", "replace"))
                continue
            self.line(log, line)
        if len(self.buf) > MAX_MSG:   # a line with no end: written in pieces
            self.line(log, self.buf)
            self.buf = b""

    def line(self, log, raw):
        ident = (self.head[0] if self.head else "") or (comm(self.pid) if self.pid else "")
        if len(self.head) > 3 and self.head[3] == "1" and raw[:1] == b"<":
            end = raw.find(b">")   # a <N> level prefix
            if 0 < end < 4:
                raw = raw[end + 1:]
        log.write(ident, self.pid, raw)


def listen(path, kind):
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    s = socket.socket(socket.AF_UNIX, kind | socket.SOCK_NONBLOCK | socket.SOCK_CLOEXEC)
    s.bind(path)
    if kind == socket.SOCK_DGRAM:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED, 1)
        try:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 8 << 20)
        except OSError:
            pass
    else:
        s.listen(16)
    return s


def main():
    o = args()
    try:
        with open("/proc/self/comm", "w") as f:   # `pkill python3` in a box does not end it
            f.write("omabox-journal")
    except OSError:
        pass
    os.makedirs(o["dir"], exist_ok=True)
    log = Log(o["log"], o["max"])
    native = listen(os.path.join(o["dir"], "socket"), socket.SOCK_DGRAM)
    stdout = listen(os.path.join(o["dir"], "stdout"), socket.SOCK_STREAM)
    sel = selectors.DefaultSelector()
    sel.register(native, selectors.EVENT_READ, "native")
    sel.register(stdout, selectors.EVENT_READ, "accept")
    streams = 0
    while True:
        for key, _ in sel.select():
            if key.data == "native":
                recv_native(native, log)
            elif key.data == "accept":
                try:
                    conn, _ = stdout.accept()
                except OSError:
                    continue
                conn.setblocking(False)
                sel.register(conn, selectors.EVENT_READ, Stream(conn))
                streams += 1
                if streams >= MAX_STREAMS:
                    sel.unregister(stdout)
            else:
                st = key.data
                try:
                    data = st.conn.recv(1 << 16)
                except (BlockingIOError, InterruptedError):
                    continue
                except OSError:
                    data = b""
                if data:
                    st.feed(log, data)
                    continue
                if st.buf:
                    st.line(log, st.buf)
                sel.unregister(st.conn)
                st.conn.close()
                if streams >= MAX_STREAMS:
                    sel.register(stdout, selectors.EVENT_READ, "accept")
                streams -= 1


if __name__ == "__main__":
    main()
