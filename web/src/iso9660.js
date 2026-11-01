// A minimal ISO 9660 reader: just enough to find files on the Galapagos CD.
//
// A CD image is a sequence of 2048-byte sectors. Sector 16 holds the
// Primary Volume Descriptor (PVD); at byte 156 of it sits the root
// *directory record*. A directory is itself a file made of records:
//
//   off  size  field
//   0    1     record length (0 = padding until the next sector)
//   2    4     extent: first sector of the file      (LE; BE copy follows)
//   10   4     data length in bytes                  (LE; BE copy follows)
//   25   1     flags (bit 1 = directory)
//   32   1     name length
//   33   n     name, e.g. "WORLDS;1" ("\0" = self, "\1" = parent)
//
// `read(offset, length)` returns a Promise<Uint8Array>. In the browser it
// wraps File.slice(), so a 383 MB image is never loaded whole into memory.

const SECTOR = 2048;

export async function openIso(read) {
  const pvd = await read(16 * SECTOR, SECTOR);
  const magic = String.fromCharCode(...pvd.subarray(1, 6));
  if (pvd[0] !== 1 || magic !== "CD001") throw new Error("Not an ISO 9660 image");
  const root = parseRecord(pvd, 156);

  async function list(dir) {
    const raw = await read(dir.sector * SECTOR, dir.size);
    const entries = [];
    for (let off = 0; off < raw.length; ) {
      const len = raw[off];
      if (len === 0) {
        off = (Math.floor(off / SECTOR) + 1) * SECTOR; // skip sector padding
        continue;
      }
      const rec = parseRecord(raw, off);
      if (rec.name !== "\0" && rec.name !== "\x01") entries.push(rec);
      off += len;
    }
    return entries;
  }

  // Resolve "install/data/worlds" case-insensitively, ignoring ";1" versions.
  async function find(path) {
    let dir = root;
    for (const part of path.toLowerCase().split("/")) {
      const hit = (await list(dir)).find((e) => e.name === part);
      if (!hit) return null;
      dir = hit;
    }
    return dir;
  }

  async function readFile(path) {
    const rec = await find(path);
    if (!rec) throw new Error(`Missing on CD: ${path}`);
    return read(rec.sector * SECTOR, rec.size);
  }

  return { list, find, readFile, root };
}

function parseRecord(b, off) {
  const dv = new DataView(b.buffer, b.byteOffset + off);
  const nameLen = b[off + 32];
  let name = String.fromCharCode(...b.subarray(off + 33, off + 33 + nameLen));
  name = name.replace(/;\d+$/, "").replace(/\.$/, "").toLowerCase();
  return {
    name,
    sector: dv.getUint32(2, true),
    size: dv.getUint32(10, true),
    isDir: (b[off + 25] & 2) !== 0,
  };
}
