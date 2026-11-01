import { loadCached, importIso, forget } from "./disc.js";
import { loadEngine } from "./engine.js";
import { playMovie } from "./movie.js";
import { createWorldView } from "./world-view.js";

const $ = (id) => document.getElementById(id);
const log = (msg) => ($("log").textContent += msg + "\n");
const bytesOf = async (file) => new Uint8Array(await file.arrayBuffer());

// Display preference for interlaced movies, remembered across visits.
const scanlines = () => localStorage.getItem("galapagos.scanlines") === "on";
function scanlineToggle() {
  const b = $("scanlines");
  const label = () => (b.textContent = scanlines() ? "Scanlines: original" : "Scanlines: smooth");
  label();
  b.onclick = () => {
    localStorage.setItem("galapagos.scanlines", scanlines() ? "off" : "on");
    label();
  };
}

// Browsers keep audio muted until the user interacts with the page,
// so the stage waits for one click before anything plays.
function waitForClick(el) {
  return new Promise((resolve) => el.addEventListener("click", resolve, { once: true }));
}

async function boot(files) {
  $("boot").hidden = true;
  $("stage").hidden = false;
  const engine = await loadEngine();
  const audio = new AudioContext();

  await waitForClick($("start"));
  $("start").hidden = true;
  await audio.resume();

  // File 0 is the Anark studio intro (640x240, line-interlaced to 640x480).
  scanlineToggle();
  await playMovie(engine, await bytesOf(files["0"]), $("screen"), audio, { scanlines });

  // The worlds, alive: the 24 Hz object simulation runs; clicks reach the
  // state machines. Mendel is not in yet.
  $("screen").parentElement.hidden = true;
  $("world").hidden = false;
  const worlds = engine.openWorlds(await bytesOf(files.worlds));
  const hz = engine.sim.ticksPerSecond();
  let clock = 0, last = performance.now(), current = 0;

  const music = (n) => {
    dispatchEvent(new KeyboardEvent("keydown")); // stop the current track
    if (n > 0 && files[String(n)]) bytesOf(files[String(n)]).then((b) => playMovie(engine, b, $("screen"), audio));
  };

  const view = createWorldView($("world"), {
    // Fixed timestep: run as many ticks as real time demands (max 5 per
    // frame, so a background tab doesn't fast-forward on return).
    onFrame() {
      const now = performance.now();
      clock = Math.min(clock + (now - last) / 1000, 5 / hz);
      last = now;
      const n = Math.floor(clock * hz);
      clock -= n / hz;
      if (!n) return;
      for (const ev of engine.sim.tick(n)) {
        if (ev.kind === 1) music(ev.a);
        if (ev.kind === 2) {
          status(`A dimensional gate opened: section ${current} → ${ev.a} (time screen of level ${ev.b}).`);
          setTimeout(() => show(ev.a), 0);
        }
      }
    },
    onPick(origin, dir) {
      const id = engine.sim.click(origin, dir);
      status(id ? `Clicked object #${id}.` : "Nothing clickable there.");
    },
  });
  const picker = $("sections");
  picker.hidden = false;
  $("wake").hidden = false;
  $("wake-all").onchange = (e) => engine.sim.wakeAll(e.target.checked);
  let summary = "";
  const status = (msg) => ($("log").textContent = summary + "\n" + msg);
  const show = (i) => {
    current = i;
    const w = worlds.build(i);
    view.load({ ...w, takeDirty: engine.sim.takeDirty });
    engine.sim.wakeAll($("wake-all").checked);
    picker.querySelectorAll("button").forEach((b, j) => b.classList.toggle("is-on", j === i));
    summary =
      `Section ${i}: ${w.objects} objects, ${w.vertices.length / 21} triangles, ${w.textureCount} textures, simulated at ${hz} Hz. ` +
      "Drag to orbit, wheel to zoom, right-drag to pan, click objects to trigger them.";
    status("");
  };
  for (let i = 0; i < worlds.sectionCount; i++) {
    const b = document.createElement("button");
    b.textContent = `World ${i}`;
    b.onclick = () => show(i);
    picker.append(b);
  }
  show(0);
  // Poke the running game from DevTools: galapagos.engine.sim.object(85)…
  globalThis.galapagos = { engine, worlds, view, show };
  jukebox(engine, files, audio);
}

// Files 1–10 are Smacker files whose 4x4 "video" is a placeholder:
// they are the game's music, stored as compressed audio.
function jukebox(engine, files, audio) {
  const list = $("jukebox");
  list.hidden = false;
  for (let i = 1; i <= 10; i++) {
    const b = document.createElement("button");
    b.textContent = String(i).padStart(2, "0");
    b.onclick = async () => {
      // Any keydown ends the current movie (see movie.js), so this stops
      // whatever is playing before the engine opens the next file.
      dispatchEvent(new KeyboardEvent("keydown"));
      list.querySelectorAll("button").forEach((x) => x.classList.toggle("is-on", x === b));
      await playMovie(engine, await bytesOf(files[String(i)]), $("screen"), audio);
      b.classList.remove("is-on");
    };
    list.append(b);
  }
}

async function onIso(file) {
  $("drop").hidden = true;
  $("progress").hidden = false;
  const bar = $("progress").querySelector("i");
  const label = $("progress").querySelector(".progress__label");
  try {
    const files = await importIso(file, (p, name) => {
      bar.style.transform = `scaleX(${p})`;
      label.textContent = `Copied ${name}`;
    });
    await boot(files);
  } catch (e) {
    label.textContent = e.message;
    $("drop").hidden = false;
  }
}

$("iso").addEventListener("change", (e) => e.target.files[0] && onIso(e.target.files[0]));
const drop = $("drop");
drop.addEventListener("dragover", (e) => (e.preventDefault(), drop.classList.add("is-over")));
drop.addEventListener("dragleave", () => drop.classList.remove("is-over"));
drop.addEventListener("drop", (e) => {
  e.preventDefault();
  drop.classList.remove("is-over");
  const f = e.dataTransfer.files[0];
  if (f) onIso(f);
});
$("forget-stage").addEventListener("click", async () => (await forget(), location.reload()));

const cached = await loadCached();
if (cached) boot(cached);
