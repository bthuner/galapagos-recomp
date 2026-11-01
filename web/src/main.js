import { loadCached, importIso, forget } from "./disc.js";
import { loadEngine } from "./engine.js";
import { playMovie } from "./movie.js";
import { createWorldView } from "./world-view.js";
import { createSfx } from "./audio.js";
import { createMenu } from "./menu.js";

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
  // One volume knob for music and effects (Preferences ▸ Volume).
  const volume = audio.createGain();
  volume.connect(audio.destination);

  await waitForClick($("start"));
  $("start").hidden = true;
  await audio.resume();

  // File 0 is the Anark studio intro (640x240, line-interlaced to 640x480).
  scanlineToggle();
  await playMovie(engine, await bytesOf(files["0"]), $("screen"), audio, { scanlines });

  // The worlds, alive: the 24 Hz object simulation runs; clicks reach the
  // state machines; Mendel is spawned by the section's start portal and
  // walks, senses and learns on his own (crates/engine/src/creature.rs).
  $("screen").parentElement.hidden = true;
  $("world").hidden = false;
  const worlds = engine.openWorlds(await bytesOf(files.worlds));
  const hz = engine.sim.ticksPerSecond();
  let clock = 0, last = performance.now(), current = 0;

  const music = (n) => {
    dispatchEvent(new KeyboardEvent("keydown")); // stop the current track
    if (n > 0 && files[String(n)]) {
      bytesOf(files[String(n)]).then((b) => playMovie(engine, b, $("screen"), audio, { destination: volume }));
    }
  };

  const sfx = createSfx(engine, audio, volume);
  let paused = false; // while the pause menu is open

  const view = createWorldView($("world"), {
    // Fixed timestep: run as many ticks as real time demands (max 5 per
    // frame, so a background tab doesn't fast-forward on return).
    onFrame() {
      const ear = view.listener();
      if (ear) sfx.listen(ear.eye, ear.forward, ear.up);
      const now = performance.now();
      clock = paused ? 0 : Math.min(clock + (now - last) / 1000, 5 / hz);
      last = now;
      const n = Math.floor(clock * hz);
      clock -= n / hz;
      if (!n) return;
      for (const ev of engine.sim.tick(n)) {
        if (ev.kind === 1) music(ev.a);
        if (ev.kind === 3) sfx.emitter(ev.a, ev.b, ev.c); // a world object made a sound
        if (ev.kind === 5) sfx.creature(ev.a); // one of Mendel's cues
        if (ev.kind === 4) view.snap(); // a teleporter asked for a camera cut
        if (ev.kind === 6) menu.offerSave(); // a save pad was clicked under Mendel
        if (ev.kind === 2) {
          status(`A dimensional gate opened: section ${current} → ${ev.a} (time screen of level ${ev.b}).`);
          setTimeout(() => show(ev.a), 0);
        }
      }
      mendelWatch();
    },
    onPick(origin, dir) {
      const id = engine.sim.click(origin, dir);
      status(id ? `Clicked object #${id}.` : "Nothing clickable there.");
    },
    // Right click: touch Mendel. He feels it like bumping into something
    // on that side for 4 ticks, and his brain decides what to do with it.
    onTouch(origin, dir) {
      const side = engine.creature.touch(origin, dir);
      status(side ? `You touched Mendel's ${side} side.` : "Right-click Mendel to touch him.");
    },
    onFollowChange: (on) => ($("follow-mendel").checked = on),
    clearance: engine.creature.clearance,
  });
  // Follow Mendel by default as soon as he appears, unless the user turned
  // the chase camera off in this section.
  let mendelSeen = false, followDeclined = false;
  const mendelWatch = () => {
    const c = engine.creature.state();
    const here = c.mode !== "absent";
    if (here && !mendelSeen && !followDeclined) view.follow(true, engine.creature.state);
    mendelSeen = here;
  };
  $("follow").hidden = false;
  $("follow-mendel").onchange = (e) => {
    followDeclined = !e.target.checked;
    view.follow(e.target.checked, engine.creature.state);
  };
  // Space: the original's "skip" key (held = every STel object clicked).
  // Captured here so it doesn't also stop the music like other keys do.
  const skip = (down) => (e) => {
    if (e.code !== "Space" || e.repeat) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    engine.sim.holdSkip(down);
  };
  addEventListener("keydown", skip(true), { capture: true });
  addEventListener("keyup", skip(false), { capture: true });
  const picker = $("sections");
  picker.hidden = false;
  $("wake").hidden = false;
  $("wake-all").onchange = (e) => engine.sim.wakeAll(e.target.checked);
  let summary = "";
  const status = (msg) => ($("log").textContent = summary + "\n" + msg);
  // Display a section: build it fresh, or (after loading a saved game)
  // show the one the engine already rebuilt.
  const show = (i, { built = false } = {}) => {
    current = i;
    sfx.reset();
    const w = built ? worlds.current() : worlds.build(i);
    view.load({ ...w, takeDirty: engine.sim.takeDirty, creatureVertices: engine.creature.vertices });
    mendelSeen = false;
    engine.sim.wakeAll($("wake-all").checked);
    picker.querySelectorAll("button").forEach((b, j) => b.classList.toggle("is-on", j === i));
    summary =
      `Section ${i}: ${w.objects} objects, ${w.vertices.length / 21} triangles, ${w.textureCount} textures, simulated at ${hz} Hz. ` +
      "Drag to orbit, wheel to zoom, shift- or middle-drag to pan. Click objects to trigger them, " +
      "right-click Mendel to touch him, hold Space to skip the introduction.";
    status("");
  };
  for (let i = 0; i < worlds.sectionCount; i++) {
    const b = document.createElement("button");
    b.textContent = `World ${i}`;
    b.onclick = () => show(i);
    picker.append(b);
  }
  show(0);

  // The Pause Game Menu (Esc): new game, saves, brains, preferences.
  const VIEW_SIZES = [1, 0.85, 0.7, 0.55, 0.4]; // Full … Petite
  const VOLUMES = [0, 0.3, 0.6, 1]; // Off, Low, Medium, High
  const menu = createMenu({
    engine,
    get paused() { return paused; },
    set paused(v) { paused = v; },
    ready: () => true,
    status,
    newGame(age) {
      engine.newGame(age);
      show(0);
      status("A new Mendel is born.");
    },
    afterLoad: () => show(engine.section(), { built: true }),
    applyPrefs(p) {
      view.setResolution([240, 480, 0][p.resolution]);
      $("world").style.setProperty("--view-size", VIEW_SIZES[p.viewSize]);
      view.setBrightness(0.55 + p.brightness * 0.15); // Darkest 0.55 … Standard 1.0 … Lightest 1.45
      volume.gain.value = VOLUMES[p.volume];
      engine.setSoundQuality(p.fidelity + 1);
      sfx.flush();
    },
  });

  // Poke the running game from DevTools: galapagos.engine.sim.object(85)…
  globalThis.galapagos = { engine, worlds, view, show, sfx, menu };
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
