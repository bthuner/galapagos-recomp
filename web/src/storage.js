// Where saves live: the browser's private file system (OPFS), or IndexedDB
// when the page has no OPFS.
//
// OPFS only exists in a *secure context*: https://, or http://localhost. A
// phone that opens the game from a laptop on the home network, at
// http://192.168.1.20:8000, is neither, and `navigator.storage.getDirectory`
// is simply missing there. IndexedDB has no such rule, so `root()` falls
// back to a tiny stand-in that keeps each file as a record and offers the
// handful of OPFS calls the saves use:
//
//   dir.getDirectoryHandle(name, { create })
//   dir.getFileHandle(name, { create })     → throws NotFoundError if absent
//   dir.entries()                           → async [name, handle] pairs
//   handle.getFile()                        → a File, with lastModified
//   handle.createWritable()                 → { write(bytes), close() }
//
// Callers cannot tell the two apart, so menu.js and the autosave keep
// talking to "a directory".

let opened;
export function root() {
  opened ??= navigator.storage?.getDirectory?.().catch(() => idbRoot()) ?? Promise.resolve(idbRoot());
  return opened;
}

// ——— The IndexedDB stand-in ———
// One object store; a key is a path ("galapagos-saves/autosave.glps"), a
// value is { bytes, at }.

let db;
function store(mode) {
  db ??= new Promise((resolve, reject) => {
    const req = indexedDB.open("galapagos-files", 1);
    req.onupgradeneeded = () => req.result.createObjectStore("files");
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
  return db.then((d) => d.transaction("files", mode).objectStore("files"));
}
const done = (req) =>
  new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });

const idbRoot = () => dir("");

function dir(path) {
  return {
    kind: "directory",
    name: path.split("/").pop(),
    async getDirectoryHandle(name) {
      return dir(path + name + "/"); // directories exist as soon as a file does
    },
    async getFileHandle(name, { create = false } = {}) {
      const key = path + name;
      if (!create && !(await done((await store("readonly")).getKey(key)))) {
        throw new DOMException(`${key} not found`, "NotFoundError");
      }
      return file(key);
    },
    async *entries() {
      const keys = await done((await store("readonly")).getAllKeys());
      for (const key of keys) {
        const rest = key.startsWith(path) ? key.slice(path.length) : null;
        if (rest && !rest.includes("/")) yield [rest, file(key)];
      }
    },
  };
}

function file(key) {
  const name = key.split("/").pop();
  return {
    kind: "file",
    name,
    async getFile() {
      const rec = await done((await store("readonly")).get(key));
      return new File(rec ? [rec.bytes] : [], name, { lastModified: rec?.at ?? Date.now() });
    },
    async createWritable() {
      const parts = [];
      return {
        write: async (bytes) => parts.push(bytes),
        close: async () => {
          const bytes = new Uint8Array(await new Blob(parts).arrayBuffer());
          await done((await store("readwrite")).put({ bytes, at: Date.now() }, key));
        },
      };
    },
  };
}
