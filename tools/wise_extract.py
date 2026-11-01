#!/usr/bin/env python3
"""Unpack the Galapagos CD installer (setup.exe, a 1997 Wise Installer stub).

How Wise packs files
--------------------
A Wise setup.exe is a tiny 28 KB Win32 stub followed by an *overlay*: data
appended after the last PE section, which Windows ignores when loading the
program. The overlay is a short header and then every installed file,
back to back, each one as

    [raw DEFLATE stream][CRC-32 of the uncompressed bytes, u32 little-endian]

There is no directory: file names live in the (also compressed) install
script. We don't need names, though. We find the first stream, then hop
from stream to stream, and identify each payload by its magic bytes.

Usage: python3 tools/wise_extract.py <setup.exe> <out_dir>
"""
import os
import struct
import sys
import zlib

MAGIC = [
    (b"MZ", "exe"),
    (b"SMK2", "smk"),
    (b"?_\x03\x00", "hlp"),
]


def overlay_start(d):
    """The overlay begins right after the furthest PE section on disk."""
    pe = struct.unpack_from("<I", d, 0x3C)[0]
    count = struct.unpack_from("<H", d, pe + 6)[0]
    opt_size = struct.unpack_from("<H", d, pe + 20)[0]
    table = pe + 24 + opt_size
    end = 0
    for i in range(count):
        raw_size, raw_ptr = struct.unpack_from("<II", d, table + i * 40 + 16)
        end = max(end, raw_ptr + raw_size)
    return end


def inflate(d, pos):
    """Inflate one raw DEFLATE stream; return (data, compressed_length)."""
    z = zlib.decompressobj(-15)
    out = bytearray()
    p = pos
    while not z.eof and p < len(d):
        out += z.decompress(d[p : p + (1 << 20)])
        p += 1 << 20
    if not z.eof:
        raise zlib.error("truncated stream")
    return bytes(out), (min(p, len(d)) - pos) - len(z.unused_data)


def first_stream(d, start):
    """The overlay header has no documented size, so probe for the first
    stream whose CRC trailer matches. False positives (tiny 'valid' streams)
    are rejected by the CRC check."""
    for pos in range(start, start + 0x10000):
        try:
            data, used = inflate(d, pos)
        except zlib.error:
            continue
        crc = struct.unpack_from("<I", d, pos + used)[0]
        if len(data) > 16 and crc == zlib.crc32(data):
            return pos
    raise SystemExit("no Wise stream found")


def main(src, out):
    d = open(src, "rb").read()
    os.makedirs(out, exist_ok=True)
    pos = first_stream(d, overlay_start(d))
    n = 0
    while pos < len(d) - 8:
        try:
            data, used = inflate(d, pos)
        except zlib.error:
            # Whatever follows the last stream is not DEFLATE: we're done.
            break
        ext = next((e for m, e in MAGIC if data.startswith(m)), "bin")
        name = os.path.join(out, f"{n:03d}.{ext}")
        open(name, "wb").write(data)
        trailer = d[pos + used : pos + used + 4]
        crc_ok = trailer == struct.pack("<I", zlib.crc32(data))
        print(f"{name}  {len(data):>10} bytes  {'crc ok' if crc_ok else 'no crc'}")
        pos += used + (4 if crc_ok else 0)
        n += 1


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit(__doc__)
    main(sys.argv[1], sys.argv[2])
