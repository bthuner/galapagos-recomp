// Autosave and Continue (#39): not in the 1997 game.
//
// In 1997 a game survived only if you saved it on a save pad. In a browser
// tab that is harsh: one closed tab and Mendel's whole education is gone.
// So, every few minutes and whenever the page is hidden, we write the whole
// game (the world, the clocks, Mendel and his brain: the same GLPS bytes a
// save pad writes) to one reserved file, and the start screen offers
// **Continue** when that file exists.
//
// Faithfulness, the one subtle point. The original's Save also takes a new
// *snapshot*: Mendel's current spot becomes the place he respawns after a
// death (docs/re/saves.md §3). An autosave must not do that, or every death
// would bring him back next to where he died instead of at the last save
// pad. So we call `engine.game.autosave()`, which writes the same bytes
// without touching the snapshot; the file carries the old snapshot along,
// and Continue restores it too. Dying after Continue is exactly as harsh as
// dying before the tab was closed.
//
// When we do *not* save, so the file never holds a broken moment:
//   - before the world exists (the start screen, the intro movie);
//   - while a Continue is still being loaded (we would overwrite the very
//     file we are about to open with a fresh World 0);
//   - during the time screen between levels (the next section is not built
//     yet: a save there would reopen the finished level);
//   - while Mendel is not plainly alive: absent (before his capsule lands),
//     appearing, or vanishing (dying, or leaving through a gate). The last
//     good autosave is kept instead; at worst it is one interval old.
//   - when nothing moved since the last autosave (the pause menu is open):
//     the bytes would be the same.
//
// The file is `galapagos-saves/autosave.glps`: beside the player's saves,
// but with another extension. Player saves are always `<name>.gal` and the
// Open dialog lists only `.gal` files, so the two can never collide and the
// autosave never clutters the list. It is reached through Continue only.
//
// Cost: serialising is synchronous but tiny (about 55 KB in half a
// millisecond in World 0); the file write is asynchronous, off the frame.
// OPFS `createWritable` writes to a scratch copy and swaps it in on
// `close()`, so a tab closed mid-write leaves the previous autosave intact.
//
// Leaving the page is the one moment async is not enough: on a reload or a
// closed tab the browser tears the page down before an OPFS write can
// finish (we watched it happen: the file kept the previous minute's save).
// localStorage is synchronous, so on the way out the same bytes are also
// parked there, base64-encoded, under "galapagos.autosave.exit". On the
// next visit the newer of the two wins and is moved into OPFS.

import { register } from "../qol.js";

const DIR = "galapagos-saves";
const FILE = "autosave.glps"; // never `.gal`: see above
const EXIT = "galapagos.autosave.exit"; // { at, b64 }: the save made on the way out

async function handle(create) {
  const root = await navigator.storage.getDirectory();
  const dir = await root.getDirectoryHandle(DIR, { create });
  return dir.getFileHandle(FILE, { create });
}

let writing = Promise.resolve();
function writeAutosave(bytes) {
  // One write at a time: an exit save waits for an interval save in flight.
  writing = writing.then(async () => {
    const w = await (await handle(true)).createWritable();
    await w.write(bytes);
    await w.close();
  }).catch((err) => console.warn("Autosave failed:", err));
  return writing;
}

const toB64 = (bytes) => {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
};
const fromB64 = (b64) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));

function parkExit(bytes) {
  try {
    localStorage.setItem(EXIT, JSON.stringify({ at: Date.now(), b64: toB64(bytes) }));
  } catch {
    // storage full: the OPFS write may still make it
  }
}
function readExit() {
  try {
    const { at, b64 } = JSON.parse(localStorage.getItem(EXIT));
    return { bytes: fromB64(b64), date: new Date(at) };
  } catch {
    return null; // none, or unreadable
  }
}

// The newest autosave, or null. `section` is read from the GLPS header
// ourselves (u8[4] "GLPS", u32 version, u32 section, little-endian), so the
// start screen can name the world before the engine has loaded.
async function readAutosave() {
  let save = null;
  try {
    const file = await (await handle(false)).getFile();
    save = { bytes: new Uint8Array(await file.arrayBuffer()), date: new Date(file.lastModified) };
  } catch {
    // no file yet (or no OPFS)
  }
  const exit = readExit();
  if (exit && (!save || exit.date > save.date)) {
    save = exit;
    await writeAutosave(exit.bytes); // into OPFS, where it belongs
  }
  localStorage.removeItem(EXIT);
  if (!save) return null;
  const { bytes } = save;
  const ok = bytes.length >= 12 && String.fromCharCode(...bytes.subarray(0, 4)) === "GLPS";
  return { ...save, ok, section: ok ? new DataView(bytes.buffer).getUint32(8, true) : -1 };
}

const ago = (date) => {
  const min = Math.round((Date.now() - date) / 60000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} min ago`;
  const h = Math.round(min / 60);
  return h < 24 ? `${h} h ago` : date.toLocaleDateString();
};

// Runs once per animation frame until `test()` holds, then resolves.
const until = (test, each = () => {}) =>
  new Promise((resolve) => {
    const step = () => (test() ? resolve() : (each(), requestAnimationFrame(step)));
    step();
  });

export function autosave() {
  const enabled = register({
    key: "autosave",
    label: "Autosave",
    help: "Keeps the game (Mendel's brain included) when the tab closes; Continue on the start screen brings it back. Dying still sends him to the last save pad.",
    type: "toggle",
    default: true, // pure comfort (#37): it never changes what the game does
    notOriginal: true,
    group: "Comfort",
    order: 10,
  });
  const interval = register({
    key: "autosaveMinutes",
    label: "Autosave interval",
    help: "Also saved whenever you leave the tab.",
    type: "choice",
    options: [
      { value: 1, label: "Every minute" },
      { value: 2, label: "Every 2 minutes" },
      { value: 5, label: "Every 5 minutes" },
    ],
    default: 1,
    notOriginal: true,
    group: "Comfort",
    order: 11,
  });

  // ——— Saving ———
  let live = false; // the world is up and no Continue is pending
  let lastTicks = -1;
  const game = () => globalThis.galapagos; // set by main.js once the world runs

  // Returns a reason not to save, or "" when the moment is a good one.
  function unsafe() {
    const g = game();
    if (!live || !g) return "no world yet";
    if (!document.getElementById("time-screen").hidden) return "time screen";
    if (g.engine.creature.state().mode !== "alive") return "Mendel is not alive";
    if (g.engine.game.times().totalTicks === lastTicks) return "nothing changed";
    return "";
  }

  function saveNow(why) {
    if (!enabled.get() || unsafe()) return;
    const { engine } = game();
    const t0 = performance.now();
    const bytes = engine.game.autosave();
    if (!bytes.length) return;
    lastTicks = engine.game.times().totalTicks;
    if (why !== "interval") parkExit(bytes); // leaving: synchronous copy (see above)
    // For the curious, in DevTools: galapagos.lastAutosave.
    game().lastAutosave = { why, bytes: bytes.length, ms: +(performance.now() - t0).toFixed(2), at: new Date() };
    writeAutosave(bytes);
  }

  let timer = 0;
  const schedule = () => {
    clearInterval(timer);
    if (enabled.get()) timer = setInterval(() => saveNow("interval"), interval.get() * 60_000);
  };
  enabled.on(schedule);
  interval.on(schedule);
  // Leaving the tab: switching away, minimising, reloading or closing all
  // pass through "hidden" first; pagehide covers the rest.
  document.addEventListener("visibilitychange", () => document.visibilityState === "hidden" && saveNow("hidden"));
  addEventListener("pagehide", () => saveNow("pagehide"));

  // ——— Continue, on the start screen ———
  const start = document.getElementById("start");
  const log = (msg) => (document.getElementById("log").textContent += `\n${msg}`);

  readAutosave().then((save) => {
    // A plain start: the world goes live as soon as main.js publishes it.
    let continuing = false;
    until(() => continuing || game()).then(() => !continuing && (live = true));
    if (!save || !enabled.get() || start.hidden) return;

    const b = Object.assign(document.createElement("button"), { type: "button", className: "continue" });
    b.append("Continue");
    b.append(Object.assign(document.createElement("small"), {
      textContent: save.ok && save.section >= 0 ? `World ${save.section} · ${ago(save.date)}` : "Autosave",
    }));
    start.after(b);
    start.addEventListener("click", () => b.remove(), { once: true });

    b.onclick = async () => {
      continuing = true;
      b.remove();
      // This click is the gesture the browser wants before sound plays:
      // passing it to "Press to begin" lets main.js resume audio as usual.
      // (Pressed again each frame until main.js hides it: the engine may
      // still be loading, and main.js listens only once it is ready.)
      await until(() => start.hidden, () => start.click());
      // Skip the intro movie: any keydown ends it (movie.js). Keep pressing
      // until the world canvas replaces the movie screen.
      const world = document.getElementById("world");
      await until(() => !world.hidden, () => dispatchEvent(new KeyboardEvent("keydown")));
      await until(game);
      const g = game();
      try {
        if (!save.ok) throw new Error("not a Galapagos save");
        g.game.load(save.bytes);
        log(`Continued from the autosave of ${save.date.toLocaleString()}.`);
      } catch (err) {
        // Corrupt, or written by an engine this one cannot read: start over,
        // and keep the old file until the first new autosave replaces it.
        g.engine.newGame(g.menu.prefs.age);
        g.show(0);
        log(`Could not continue (${err.message}): a new game starts instead.`);
      }
      lastTicks = g.engine.game.times().totalTicks; // the file already holds this moment
      live = true;
    };
  });
}
