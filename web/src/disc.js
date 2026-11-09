// Getting the game data into the browser, legally.
//
// We never host Galapagos' data: the player brings their own CD image.
// The files we need are copied once into the Origin Private File System
// (OPFS), a sandboxed disk the browser gives every site, so later visits
// boot instantly without asking for the ISO again.
//
// Or the data sits next to the page, in ./disc/, on a server the player
// owns (tools/serve.py --disc, see loadServed below). That is how a phone
// plays: no ISO to carry, nothing copied.

import { openIso } from "./iso9660.js";

// Everything the runtime reads. The executable itself is not needed:
// we are re-implementing it.
export const DATA_FILES = [
  "worlds",
  ...Array.from({ length: 11 }, (_, i) => String(i)), // Smacker videos
];

async function cacheDir() {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle("galapagos", { create: true });
}

export async function loadCached() {
  try {
    const dir = await cacheDir();
    const files = {};
    for (const name of DATA_FILES) {
      const h = await dir.getFileHandle(name);
      files[name] = await h.getFile();
    }
    return files;
  } catch {
    return null; // first visit, or the cache was cleared
  }
}

// The disc served by the player's own server, if there is one: a
// `disc/worlds` next to index.html (`tools/serve.py --disc`, or the data
// folder copied there by hand; web/disc/ is gitignored). Nothing is
// downloaded up front. Each entry only looks like a File: the game reads
// files through `arrayBuffer()`, so a movie is fetched when it plays, and
// the browser's HTTP cache does the rest. No OPFS needed, which matters on
// a phone at http://192.168.…, where there is none (see storage.js).
export async function loadServed() {
  const url = (name) => new URL(`../disc/${name}`, import.meta.url);
  try {
    const head = await fetch(url("worlds"), { method: "HEAD", cache: "no-cache" });
    if (!head.ok) return null;
  } catch {
    return null; // offline, or a file:// page
  }
  const files = {};
  for (const name of DATA_FILES) {
    files[name] = {
      name,
      async arrayBuffer() {
        const res = await fetch(url(name));
        if (!res.ok) throw new Error(`The server has no disc/${name} (${res.status})`);
        return res.arrayBuffer();
      },
    };
  }
  return files;
}

export async function importIso(file, onProgress = () => {}) {
  const read = async (off, len) =>
    new Uint8Array(await file.slice(off, off + len).arrayBuffer());
  const iso = await openIso(read);
  const dir = await cacheDir();
  let done = 0;
  for (const name of DATA_FILES) {
    const bytes = await iso.readFile(`install/data/${name}`);
    const out = await (await dir.getFileHandle(name, { create: true })).createWritable();
    await out.write(bytes);
    await out.close();
    onProgress(++done / DATA_FILES.length, name);
  }
  navigator.storage.persist?.(); // ask the browser not to evict 140 MB
  return loadCached();
}

export async function forget() {
  const root = await navigator.storage.getDirectory();
  await root.removeEntry("galapagos", { recursive: true });
}
