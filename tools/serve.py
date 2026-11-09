#!/usr/bin/env python3
"""Serve web/ for development, telling the browser to revalidate every file.

`python3 -m http.server` sends no Cache-Control header, so browsers guess how
long a file stays fresh (often hours for a file that hasn't changed in a
while) and keep running yesterday's JavaScript after you pull. That is how a
merged feature can "not show up". With `Cache-Control: no-cache` the browser
still caches, but asks the server first; an unchanged file costs a 304.

With --disc, the server also hands out your own copy of the game under
/disc/, and the page boots from it without asking for the ISO. That is the
way to play on a phone: run this on your computer, open the printed
http://192.168.…:PORT address on the phone, same Wi-Fi. The disc can be the
ISO itself (files are read straight out of it, nothing is extracted) or a
folder holding `worlds` and `0`…`10` (the CD's install/data). Anyone on your
network can reach the server while it runs; stop it with Ctrl+C.

Usage: python3 tools/serve.py [port] [--disc "Galapagos PC.iso" | --disc DIR]
       (default port 8000, serves ./web)
"""
import argparse
import functools
import http.server
import pathlib
import socket
import struct
import subprocess

SECTOR = 2048
DATA_FILES = ["worlds", *map(str, range(11))]  # same list as web/src/disc.js


def iso_files(iso):
    """{name: (offset, size)} for install/data/* in an ISO 9660 image.

    The same walk as web/src/iso9660.js: the root directory record sits at
    byte 156 of the Primary Volume Descriptor (sector 16); a directory is a
    file of records (length, extent at +2, size at +10, name at +33).
    """
    def records(f, sector, size):
        f.seek(sector * SECTOR)
        raw = f.read(size)
        off = 0
        while off < len(raw):
            n = raw[off]
            if n == 0:  # padding until the next sector
                off = (off // SECTOR + 1) * SECTOR
                continue
            ext, length = struct.unpack_from("<I", raw, off + 2)[0], struct.unpack_from("<I", raw, off + 10)[0]
            name = raw[off + 33 : off + 33 + raw[off + 32]].decode("latin-1")
            name = name.split(";")[0].rstrip(".").lower()
            if name not in ("\0", "\1"):
                yield name, ext, length
            off += n

    with open(iso, "rb") as f:
        f.seek(16 * SECTOR)
        pvd = f.read(SECTOR)
        if pvd[1:6] != b"CD001":
            raise SystemExit(f"{iso}: not an ISO 9660 image")
        where = struct.unpack_from("<I", pvd, 156 + 2)[0], struct.unpack_from("<I", pvd, 156 + 10)[0]
        for part in ("install", "data"):
            hit = {n: (e, s) for n, e, s in records(f, *where)}.get(part)
            if not hit:
                raise SystemExit(f"{iso}: no install/data folder, is this the Galapagos CD?")
            where = hit
        return {n: (e * SECTOR, s) for n, e, s in records(f, *where) if n in DATA_FILES}


class Handler(http.server.SimpleHTTPRequestHandler):
    disc = None  # {name: (path, offset, size)}

    def end_headers(self):
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()

    def send_head(self):
        path = self.path.split("?")[0]
        if not path.startswith("/disc/"):
            return super().send_head()
        hit = (self.disc or {}).get(path[len("/disc/"):])
        if not hit:
            self.send_error(404)
            return None
        file, offset, size = hit
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        self.send_header("Content-Length", str(size))
        self.end_headers()
        f = open(file, "rb")
        f.seek(offset)
        return Slice(f, size)

    def copyfile(self, source, outputfile):
        if isinstance(source, Slice):
            source.copy_to(outputfile)
        else:
            super().copyfile(source, outputfile)


class Slice:
    """`size` bytes of an open file from its current position."""

    def __init__(self, f, size):
        self.f, self.size = f, size

    def copy_to(self, out):
        left = self.size
        while left:
            chunk = self.f.read(min(left, 1 << 20))
            if not chunk:
                break
            out.write(chunk)
            left -= len(chunk)

    def close(self):
        self.f.close()


def lan_addresses():
    """[(address, interface)] a phone on the same network could use.

    Linux: every global IPv4 address `ip` knows, minus container bridges.
    Elsewhere: the address of the default route (a VPN can win that one,
    hence the full list where we can get it).
    """
    try:
        out = subprocess.run(["ip", "-4", "-o", "addr", "show", "scope", "global"],
                             capture_output=True, text=True, check=True).stdout
        found = [(line.split()[3].split("/")[0], line.split()[1]) for line in out.splitlines()]
        return [(a, i) for a, i in found if not i.startswith(("docker", "br-", "veth"))]
    except (OSError, subprocess.CalledProcessError, IndexError):
        pass
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        try:
            s.connect(("192.0.2.1", 9))  # no packet is sent: this only picks a route
            return [(s.getsockname()[0], "default route")]
        except OSError:
            return []


args = argparse.ArgumentParser(description=__doc__.split("\n")[0])
args.add_argument("port", nargs="?", type=int, default=8000)
args.add_argument("--disc", type=pathlib.Path, help="your Galapagos ISO, or a folder with worlds and 0…10")
args = args.parse_args()

if args.disc:
    if args.disc.is_dir():
        Handler.disc = {}
        for name in DATA_FILES:
            p = args.disc / name
            if not p.is_file():
                raise SystemExit(f"{args.disc}: {name} is missing")
            Handler.disc[name] = (p, 0, p.stat().st_size)
    else:
        Handler.disc = {n: (args.disc, off, size) for n, (off, size) in iso_files(args.disc).items()}
        missing = set(DATA_FILES) - set(Handler.disc)
        if missing:
            raise SystemExit(f"{args.disc}: missing {', '.join(sorted(missing))}")

web = pathlib.Path(__file__).resolve().parent.parent / "web"
if args.disc and (web / "disc").exists():
    print("note: --disc takes over /disc/, the web/disc folder is ignored")
print(f"serving {web} on http://localhost:{args.port} (Cache-Control: no-cache)")
if args.disc:
    print(f"disc: {args.disc}\non your phone (same Wi-Fi), one of:")
    for address, interface in lan_addresses():
        print(f"  http://{address}:{args.port}   ({interface})")
print(flush=True)
handler = functools.partial(Handler, directory=str(web))
http.server.ThreadingHTTPServer(("", args.port), handler).serve_forever()
