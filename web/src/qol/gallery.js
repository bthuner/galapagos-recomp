// Worlds, music & movies (#47): the player-facing face of three developer
// tools. The stage under the game already has "World 0…7" buttons and
// soundtrack buttons 01–10; they are for us, and jump anywhere. This is the
// same power in the pause menu, shaped like a game's extras screen:
//
//   World select  the worlds this browser has reached ("galapagos.reached"),
//                 each with its own time-screen picture as a thumbnail.
//                 "Unlock all worlds" (Explore, off) opens the other ones.
//   Jukebox       the ten soundtrack files, with play and pause.
//   Gallery       the Anark intro, once more.
//
// None of it is in the 1997 game, which had no extras screen at all.
//
// ——— How it plugs in ———
// main.js makes four one-line calls, because the feature needs the running
// game and the game only exists inside boot():
//
//   const extras = gallery();          // at load: settings, key handling
//   extras.reached(i)                  // in show(): remember the section
//   const music = extras.music(fn)     // wraps the game's own music player
//   extras.attach({ engine, … })       // at the end of boot()
//
// ——— One movie at a time ———
// The engine has one Smacker decoder slot. The game's music *is* a movie (a
// 4×4 placeholder picture over compressed audio, see main.js), and so is the
// intro. So before the jukebox decodes a track or the intro plays, the
// game's music is stopped, and afterwards the game's track starts again.
// A soundtrack file cannot be paused halfway (nothing in movie.js keeps its
// place), so "restoring" means the game's track begins from the top: the
// same thing the original does when a section starts.
//
// The jukebox itself does not go through playMovie: it decodes a track's
// audio once into an AudioBuffer and plays that, which is what makes a
// real pause possible (stop, remember the offset, start again from it).

import { register } from "../qol.js";
import { playMovie, toAudioBuffer } from "../movie.js";

const REACHED = "galapagos.reached";
const TRACKS = 10; // Smacker files 1–10
const bytesOf = async (file) => new Uint8Array(await file.arrayBuffer());
const el = (tag, props = {}, ...children) => {
  const e = Object.assign(document.createElement(tag), props);
  e.append(...children);
  return e;
};
const clock = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

function loadReached() {
  try {
    const list = JSON.parse(localStorage.getItem(REACHED) || "[]");
    return new Set([0, ...list.filter(Number.isInteger)]); // World 0 is where every game starts
  } catch {
    return new Set([0]);
  }
}

export function gallery() {
  const reachedSet = loadReached();
  let app = null; // the running game, once attach() is called
  let refresh = () => {}; // redraws the panel when it is on screen

  const unlockAll = register({
    key: "unlockAll",
    label: "Unlock all worlds",
    help: "Every world in Worlds, music & movies, not just the ones Mendel has reached.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group: "Explore",
    order: 1, // right under the screen it unlocks
  });
  unlockAll.on(() => refresh());
  register({
    key: "extras",
    label: "Worlds, music & movies…",
    help: "Start a world you have reached, play the soundtrack, watch the intro.",
    type: "button",
    buttonLabel: "Open",
    notOriginal: true,
    group: "Explore",
    order: 0, // the screen first, its toggle after
    action: () => app?.menu.show("gallery"),
  });

  // ——— Who owns the speakers ———
  // `holders` counts the extras currently playing (the jukebox, the intro).
  // While it is above zero, the game's music requests are remembered, not
  // played; when it drops to zero, the last one is played.
  let holders = 0, gameTrack = 0, playGame = () => {};
  const take = () => {
    if (holders++ === 0) playGame(0); // stop the game's track (frees the decoder)
  };
  const release = () => {
    if (--holders === 0 && gameTrack > 0) playGame(gameTrack);
  };

  // ——— The intro movie, in its own overlay above the menu ———
  const stage = el("div", { className: "extras-movie", hidden: true });
  const screen = el("canvas", { width: 640, height: 480 });
  stage.append(screen, el("p", { textContent: "Any key or a click to stop" }));
  document.body.append(stage);
  stage.addEventListener("click", (e) => e.target !== screen && screen.click());
  // Registered at load, before the pause menu's own Esc listener: while the
  // intro plays, every key ends it (Esc included) and goes no further, so it
  // neither closes the menu nor makes Mendel skip.
  addEventListener("keydown", (e) => {
    if (stage.hidden || !e.isTrusted) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    screen.click(); // playMovie stops on a click on its canvas
  }, true);

  async function watchIntro() {
    if (!stage.hidden) return;
    take(); // before pausing the jukebox, so the game's track stays quiet
    jukebox.pause();
    stage.hidden = false;
    try {
      await playMovie(app.engine, await bytesOf(app.files["0"]), screen, app.audio, {
        scanlines: app.scanlines,
        destination: app.out,
      });
    } finally {
      stage.hidden = true;
      release();
    }
  }

  // ——— The jukebox ———
  const jukebox = {
    track: 0, // 1..10, or 0 before the first press
    buffer: null, // the current track, decoded
    source: null, // playing now (null while paused)
    startedAt: 0, // audio time when `offset` 0 would have started
    offset: 0, // where a paused track resumes
    loading: false,
    get playing() { return !!this.source; },
    async toggle(n) {
      if (this.loading) return;
      if (n === this.track && this.buffer) return this.playing ? this.pause() : this.play();
      take(); // first: the decoder must be free, and the game's track must not restart in between
      this.pause();
      this.loading = true;
      this.track = n;
      refresh();
      try {
        const movie = app.engine.openMovie(await bytesOf(app.files[String(n)]));
        try {
          this.buffer = toAudioBuffer(app.audio, movie.decodeAudio(), movie.sampleRate, movie.channels);
        } finally {
          movie.close();
        }
        durations[n] = this.buffer.duration;
        this.offset = 0;
        this.loading = false;
        this.play(); // takes its own hold
      } catch (err) {
        this.loading = false;
        this.buffer = null;
        app.status(`Track ${n} could not be played: ${err.message}.`);
      } finally {
        release(); // the hold taken for decoding
      }
    },
    play() {
      if (this.playing || !this.buffer) return;
      take();
      const src = app.audio.createBufferSource();
      src.buffer = this.buffer;
      src.connect(app.out);
      src.onended = () => {
        if (this.source !== src) return; // paused or replaced: already handled
        this.source = null;
        this.offset = 0;
        release();
        refresh();
      };
      this.startedAt = app.audio.currentTime - this.offset;
      src.start(0, this.offset);
      this.source = src;
      refresh();
    },
    pause() {
      if (!this.playing) return;
      const src = this.source;
      this.source = null; // first, so onended knows it was us
      src.stop();
      this.offset = (app.audio.currentTime - this.startedAt) % this.buffer.duration;
      release();
      refresh();
    },
  };
  const durations = {};

  // ——— The panel ———
  function draw(root) {
    const { engine, sections } = app;
    const open = unlockAll.get();

    const worlds = el("div", { className: "extras__worlds" });
    for (let i = 0; i < sections; i++) {
      const known = reachedSet.has(i);
      const card = el("button", { type: "button", className: "extras__world", disabled: !known && !open });
      card.classList.toggle("is-current", i === engine.section());
      const thumb = thumbnail(engine, i);
      if (thumb) card.append(thumb);
      card.append(el("strong", { textContent: `World ${i}` }));
      card.append(el("small", { textContent: known ? (i === engine.section() ? "Here now" : "Reached") : open ? "Unlocked" : "Not reached yet" }));
      card.onclick = () => {
        app.menu.close();
        app.show(i);
        app.status(`World ${i}, from the start. Mendel keeps his mind.`);
      };
      worlds.append(card);
    }

    const tracks = el("ol", { className: "extras__tracks" });
    for (let n = 1; n <= TRACKS; n++) {
      const current = n === jukebox.track;
      const state = current && jukebox.loading ? "…" : current && jukebox.playing ? "❚❚" : "▶";
      const b = el("button", { type: "button" });
      b.setAttribute("aria-pressed", String(current && jukebox.playing));
      b.classList.toggle("is-on", current);
      b.append(
        el("span", { className: "extras__state", textContent: state, ariaHidden: "true" }),
        el("span", { textContent: `Track ${String(n).padStart(2, "0")}` }),
        el("small", { textContent: durations[n] ? clock(durations[n]) : "" }),
      );
      b.setAttribute("aria-label", `${current && jukebox.playing ? "Pause" : "Play"} track ${n}`);
      b.onclick = () => jukebox.toggle(n);
      tracks.append(el("li", {}, b));
    }

    const intro = el("button", { type: "button", className: "extras__movie", textContent: "▶ Anark intro" });
    intro.append(el("small", { textContent: "File 0 · 640 × 480, interlaced · Scanlines follow the stage setting" }));
    intro.onclick = watchIntro;

    root.replaceChildren(
      el("p", { className: "menu__note", textContent: "Not in the 1997 game: an extras screen for playing today. A world starts fresh; Mendel keeps his mind, as he does at a gate." }),
      el("h3", { className: "qol__group", textContent: "Worlds" }),
      worlds,
      el("p", { className: "menu__note", textContent: open ? "All worlds unlocked (Quality of Life ▸ Explore)." : `${reachedSet.size} of ${sections} reached in this browser. Quality of Life ▸ Explore can unlock them all.` }),
      el("h3", { className: "qol__group", textContent: "Jukebox" }),
      tracks,
      el("h3", { className: "qol__group", textContent: "Movies" }),
      intro,
    );
  }

  return {
    // Every section shown counts as reached (gates, loads, New game, dev buttons).
    reached(i) {
      if (reachedSet.has(i)) return;
      reachedSet.add(i);
      localStorage.setItem(REACHED, JSON.stringify([...reachedSet].sort((a, b) => a - b)));
    },
    // Wrap the game's music function: requests made while an extra holds
    // the speakers wait; the last one plays when it lets go.
    music(play) {
      playGame = play;
      return (n) => {
        gameTrack = n;
        if (holders === 0) play(n);
      };
    },
    // `app`: engine, files, audio, out (the volume node), show(i), menu,
    // status(msg), scanlines(), sections (how many worlds).
    attach(running) {
      app = running;
      const root = document.querySelector(".extras");
      refresh = () => !root.closest(".menu__panel").hidden && draw(root);
      app.menu.onShow("gallery", () => draw(root));
    },
  };
}

// The time-screen picture of a level (the one shown when Mendel leaves it),
// at the smallest size the game ships (144 × 108, 8-bit set): a thumbnail
// that comes straight from the CD. Its glyph strip is cut off.
const thumbs = new Map();
function thumbnail(engine, level) {
  if (!thumbs.has(level)) {
    const layout = engine.game.timeScreen(level, 160, 120);
    const pic = layout && engine.picture(layout.picture);
    let canvas = null;
    if (pic) {
      canvas = el("canvas", { width: pic.width, height: layout.visibleHeight, ariaHidden: "true" });
      canvas.getContext("2d").putImageData(pic, 0, 0);
    }
    thumbs.set(level, canvas);
  }
  const c = thumbs.get(level);
  if (!c) return null;
  // A copy: one canvas element can only sit in one place.
  const copy = el("canvas", { width: c.width, height: c.height, ariaHidden: "true" });
  copy.getContext("2d").drawImage(c, 0, 0);
  return copy;
}
