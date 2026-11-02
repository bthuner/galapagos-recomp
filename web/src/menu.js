// The pause menu, preferences and saved games.
//
// In 1997 these were Windows chrome: a File / Edit / Help menu bar
// (resource 128 in galapagos.exe), a Preferences dialog (resource 129) and
// the common Open / Save As dialogs. We keep their structure and their
// wording, but draw them as one overlay, opened with Esc like the original's
// "Pause Game Menu". The title is the chrome GALAPAGOS strip from the game's
// own archive (picture 0).
//
// Saved games live in the browser's private file system (OPFS), next to the
// cached CD data, as `<name>.gal` files.

// ——— Preferences (dialog 129), with the original labels ———
export const PREFS = {
  resolution: { label: "Resolution", options: ["320 × 240", "640 × 480", "Native"], default: 2 },
  viewSize: { label: "View Size", options: ["Full", "Large", "Medium", "Small", "Petite"], default: 0 },
  brightness: {
    label: "Brightness",
    options: ["Darkest", "Darker", "Dark", "Standard", "Light", "Lighter", "Lightest"],
    default: 3,
  },
  volume: { label: "Volume", options: ["Off", "Low", "Medium", "High"], default: 3 },
  fidelity: { label: "Fidelity", options: ["Low", "Medium", "High", "CD Quality"], default: 3 },
};
// The "New Mendel Age" slider (0..255) and its five labels (strings 30005-30009),
// split at the same points the exe uses (0x4171ad).
export function ageLabel(v) {
  return v === 0 ? "Newborn" : v < 85 ? "1 hour" : v < 170 ? "2 hours" : v < 255 ? "4 hours" : "7 hours";
}

const KEY = "galapagos.prefs";
export function loadPrefs() {
  const saved = JSON.parse(localStorage.getItem(KEY) || "{}");
  const p = { age: 128 };
  for (const [k, d] of Object.entries(PREFS)) p[k] = d.default;
  return { ...p, ...saved };
}
const savePrefs = (p) => localStorage.setItem(KEY, JSON.stringify(p));

// ——— Saved games in OPFS ———
async function savesDir() {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle("galapagos-saves", { create: true });
}
async function listSaves() {
  const out = [];
  for await (const [name, h] of (await savesDir()).entries()) {
    if (!name.endsWith(".gal")) continue;
    const f = await h.getFile();
    out.push({ name: name.slice(0, -4), date: new Date(f.lastModified) });
  }
  return out.sort((a, b) => b.date - a.date);
}
async function writeSave(name, bytes) {
  const w = await (await (await savesDir()).getFileHandle(`${name}.gal`, { create: true })).createWritable();
  await w.write(bytes);
  await w.close();
}
async function readSave(name) {
  const f = await (await (await savesDir()).getFileHandle(`${name}.gal`)).getFile();
  return new Uint8Array(await f.arrayBuffer());
}

// A file chosen by the player (for Import Mendel NERM).
function pickFile(accept) {
  return new Promise((resolve) => {
    const input = Object.assign(document.createElement("input"), { type: "file", accept });
    input.onchange = async () => resolve(input.files[0] ? new Uint8Array(await input.files[0].arrayBuffer()) : null);
    input.click();
  });
}
// Hand bytes to the browser as a download (Export Mendel NERM).
function download(bytes, name) {
  const url = URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
  Object.assign(document.createElement("a"), { href: url, download: name }).click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/**
 * `app` is the running game:
 *   engine, paused (get/set), newGame(age), applyPrefs(prefs), afterLoad()
 */
export function createMenu(app) {
  const root = document.getElementById("menu");
  const $ = (sel) => root.querySelector(sel);
  const prefs = loadPrefs();
  let open = false;

  // Title: the chrome GALAPAGOS strip, straight from the archive.
  const strip = app.engine.picture(0);
  if (strip) {
    const c = $(".menu__title");
    [c.width, c.height] = [strip.width, strip.height];
    c.getContext("2d").putImageData(strip, 0, 0);
  }

  function show(panel) {
    root.querySelectorAll(".menu__panel").forEach((p) => (p.hidden = p.dataset.panel !== panel));
    $(".menu__save-hint").hidden = app.engine.canSave();
  }
  function setOpen(v) {
    open = v;
    root.hidden = !v;
    app.paused = v;
    if (v) show("main");
  }

  // ——— Main panel (the File / Edit / Help menus, flattened) ———
  const actions = {
    resume: () => setOpen(false),
    new: async () => {
      app.newGame(prefs.age);
      setOpen(false);
    },
    open: async () => {
      const list = $(".menu__saves");
      list.replaceChildren();
      const saves = await listSaves();
      if (!saves.length) list.append(Object.assign(document.createElement("li"), { textContent: "No saved games yet. Save on a save pad: the blue, shimmering floor panels." }));
      for (const s of saves) {
        const b = Object.assign(document.createElement("button"), { textContent: s.name });
        b.append(Object.assign(document.createElement("small"), { textContent: s.date.toLocaleString() }));
        b.onclick = async () => {
          if (app.engine.loadGame(await readSave(s.name))) {
            app.afterLoad();
            setOpen(false);
          }
        };
        const li = document.createElement("li");
        li.append(b);
        list.append(li);
      }
      show("open");
    },
    import: async () => {
      const bytes = await pickFile(".mnd");
      if (bytes) app.status(app.engine.importBrain(bytes) ? "Mendel has a new mind." : "That file is not a Mendel NERM.");
      setOpen(false);
    },
    export: () => {
      download(app.engine.exportBrain(), "mendel.mnd");
      app.status("Mendel's NERM exported as mendel.mnd.");
    },
    prefs: () => show("prefs"),
    about: () => show("about"),
    quit: () => location.reload(),
    back: () => show("main"),
  };
  root.addEventListener("click", (e) => {
    const a = e.target.closest("[data-action]")?.dataset.action;
    if (a) actions[a]();
  });

  // ——— Preferences panel ———
  const form = $(".menu__prefs");
  for (const [key, def] of Object.entries(PREFS)) {
    const label = Object.assign(document.createElement("label"), { textContent: def.label });
    const select = document.createElement("select");
    def.options.forEach((o, i) => select.append(new Option(o, i)));
    select.value = prefs[key];
    select.onchange = () => {
      prefs[key] = +select.value;
      savePrefs(prefs);
      app.applyPrefs(prefs);
    };
    label.append(select);
    form.append(label);
  }
  const age = $(".menu__age input");
  const ageOut = $(".menu__age output");
  age.value = prefs.age;
  ageOut.textContent = ageLabel(prefs.age);
  age.oninput = () => {
    prefs.age = +age.value;
    ageOut.textContent = ageLabel(prefs.age);
    savePrefs(prefs);
  };

  // ——— The save-pad dialog (engine event 6) ———
  const saveForm = $(".menu__save-form");
  saveForm.onsubmit = async (e) => {
    e.preventDefault();
    const name = saveForm.name.value.trim().replace(/[\\/:*?"<>|]/g, "_");
    const bytes = name && app.engine.saveGame();
    if (bytes) {
      await writeSave(name, bytes);
      app.status(`Saved “${name}”.`);
    }
    setOpen(false);
  };

  // Esc opens and closes the menu (the original's Pause Game Menu).
  addEventListener("keydown", (e) => {
    if (e.key !== "Escape" || !app.ready()) return;
    e.stopImmediatePropagation();
    setOpen(!open);
  }, true);

  app.applyPrefs(prefs);
  return {
    prefs,
    // A click on a save pad while Mendel stands on it.
    offerSave() {
      setOpen(true);
      show("save");
      saveForm.name.value = `Mendel ${new Date().toLocaleDateString()}`;
      saveForm.name.select();
    },
    get open() {
      return open;
    },
  };
}
