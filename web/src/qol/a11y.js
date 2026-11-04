// Accessibility options (Quality of Life #49). None of this was in the 1997
// game, and all of it is off until the player asks for it.
//
//   Colour-blind presets   a colour matrix at the very end of the world's
//                          fragment shader (and an SVG filter on the title)
//   Keyboard-only play     Tab / Shift+Tab walk through what a click would
//                          set in motion, Enter clicks it, T touches Mendel
//   Visual sound cues      small screen-edge markers for Mendel's cries,
//                          predators (with their direction) and save pads
//   Reduce flashes         palette flashes at 35% of their strength
//
// The rule, as for every QoL feature: never invent a second game. A key
// press goes through the very path a mouse click takes (`view.click` calls
// the same onPick / onTouch), what is "clickable" is the engine's own answer
// (`sim_clickable`, `sim_hover`, see hover.js), and a sound cue is drawn
// from the same call that plays the sound.

import { register, get, keyName } from "../qol.js";
import { hoverAt, clickableObjects } from "./hover.js";

// ——— Colour-blind presets: daltonisation ———
//
// Two steps, both 3×3 matrices on linear RGB (row major):
//   S  simulates the deficiency: what a dichromat perceives (Machado,
//      Oliveira & Fernandes 2009, severity 1.0);
//   E = I − S is the information that is lost;
//   M  moves that lost part into channels the player still tells apart
//      (Fidaner et al.'s "daltonize": red-green error into green and blue;
//      for tritanopia, blue-yellow error into red and green).
// The preset is D = I + M·E: colours that would collapse onto one another
// are pushed apart, and greys stay grey (S keeps them, so E·grey = 0).
const SIMULATE = {
  protanopia: [0.152286, 1.052583, -0.204868, 0.114503, 0.786281, 0.099216, -0.003882, -0.048116, 1.051998],
  deuteranopia: [0.367322, 0.860646, -0.227968, 0.280085, 0.672501, 0.047413, -0.01182, 0.04294, 0.968881],
  tritanopia: [1.255528, -0.076749, -0.178779, -0.078411, 0.930809, 0.147602, 0.004733, 0.691367, 0.3039],
};
const RED_GREEN = [0, 0, 0, 0.7, 1, 0, 0.7, 0, 1];
const BLUE_YELLOW = [1, 0, 0.7, 0, 1, 0.7, 0, 0, 0];
const I = [1, 0, 0, 0, 1, 0, 0, 0, 1];
const mul = (a, b) => a.map((_, k) => [0, 1, 2].reduce((s, j) => s + a[(k / 3 | 0) * 3 + j] * b[j * 3 + (k % 3)], 0));
const daltonise = (s, m) => I.map((v, k) => v + mul(m, I.map((w, j) => w - s[j]))[k]);
export const PRESETS = {
  off: null,
  deuteranopia: daltonise(SIMULATE.deuteranopia, RED_GREEN),
  protanopia: daltonise(SIMULATE.protanopia, RED_GREEN),
  tritanopia: daltonise(SIMULATE.tritanopia, BLUE_YELLOW),
};

// The title is a 2-D canvas over the world: the same matrix as an SVG
// feColorMatrix (which also works in linear RGB by default), used through
// CSS `filter: url(#…)`.
function titleFilters() {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("aria-hidden", "true");
  svg.style.cssText = "position:absolute;width:0;height:0";
  for (const [name, m] of Object.entries(PRESETS)) {
    if (!m) continue;
    const f = document.createElementNS(NS, "filter");
    f.id = `a11y-${name}`;
    const c = document.createElementNS(NS, "feColorMatrix");
    c.setAttribute("type", "matrix");
    // 4×5: our 3×3, no alpha mixing, no offsets.
    c.setAttribute("values", [...m.slice(0, 3), 0, 0, ...m.slice(3, 6), 0, 0, ...m.slice(6, 9), 0, 0, 0, 0, 0, 1, 0].join(" "));
    f.append(c);
    svg.append(f);
  }
  document.body.append(svg);
}

// ——— Visual cues: what each sound means ———
// Mendel's cue sounds (creature bank, 0-based; creature.rs SOUND_CUES).
const MENDEL = ["agitated", "agitated", "sees danger", "sees danger", "lands", "notices something", "notices something", "calls out", "calls out", "calls out", "calls out"];
const CUE_MS = 1500; // fully visible, then a half-second fade
const FADE_MS = 500;
const SECTION_NONE = -1;

const el = (tag, className, text = "") => Object.assign(document.createElement(tag), { className, textContent: text });

/**
 * `game()` returns `{ engine, view, sfx, menu }` once a world is on screen
 * (main.js's `globalThis.galapagos`), or nothing before.
 */
export function a11y(game = () => globalThis.galapagos, canvas = document.getElementById("world"), title = document.getElementById("title")) {
  const group = "Accessibility";
  let raf = 0; // the one animation-frame loop below (run / tick)
  const colours = register({
    key: "colourBlind",
    label: "Colour-blind preset",
    help: "Pushes apart the colours a colour vision deficiency would merge.",
    type: "choice",
    options: [
      { value: "off", label: "Off" },
      { value: "deuteranopia", label: "Deuteranopia" },
      { value: "protanopia", label: "Protanopia" },
      { value: "tritanopia", label: "Tritanopia" },
    ],
    default: "off",
    notOriginal: true,
    group,
    order: 0,
  });
  const keyboard = register({
    key: "keyboardPlay",
    label: "Keyboard-only play",
    help: "Click the game view, then Tab / Shift+Tab to pick an object, Enter to click it, T to touch Mendel.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group,
    order: 1,
  });
  const cues = register({
    key: "soundCues",
    label: "Show important sounds",
    help: "Markers at the screen's edge for Mendel's cries, predators (from where they are) and save pads.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group,
    order: 2,
  });
  const flashes = register({
    key: "reduceFlashes",
    label: "Reduce flashes",
    help: "White-outs and coloured flashes at about a third of their strength.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group,
    order: 3,
  });

  // ——— Colours and flashes: two numbers handed to the view ———
  titleFilters();
  // The view exists only once a world is up: re-applied every frame by
  // tick() below, and right away when the setting changes.
  const applyView = () => {
    const v = game()?.view;
    v?.setColourMatrix(PRESETS[colours.get()]);
    v?.setFlashStrength(flashes.get() ? 0.35 : 1);
  };
  colours.on((name) => {
    title.style.filter = PRESETS[name] ? `url(#a11y-${name})` : "";
    applyView();
  });
  flashes.on(applyView);

  // ——— Keyboard-only play ———
  // Tab is only ours while the game view (or nothing) has focus and the
  // menu is closed (qol.js already skips shortcuts while it is open), so
  // Tab still walks through buttons and menus as usual.
  const gameFocused = () => {
    const a = document.activeElement;
    return keyboard.get() && !!game() && !canvas.hidden && (a === canvas || a === document.body || a === null);
  };
  const keyDef = (key, label, code, action, order) =>
    register({ key, label, type: "key", default: code, notOriginal: true, group, order, active: gameFocused, action });
  keyDef("keyboardNext", "Keyboard play: next object (Shift: previous)", "Tab", (e) => step(e?.shiftKey ? -1 : 1), 10);
  keyDef("keyboardClick", "Keyboard play: click it", "Enter", () => clickFocused(), 11);
  keyDef("keyboardTouch", "Keyboard play: touch Mendel", "KeyT", () => touchMendel(), 12);

  let focus = null; // the focused object: { id, draw, at, offset } (see targets())
  let section = SECTION_NONE; // ids mean something else in another section
  let touchSide = "right"; // the side touched last; T alternates
  const hint = el("output", "a11y-hint");
  hint.hidden = true;
  document.body.append(hint);
  // The hint sits in the view's bottom-left corner. A one-off message
  // ("nothing in view", "Mendel is out of reach") holds it for 2.5 s, then
  // the focused object's line comes back (or the hint goes).
  let noteUntil = 0;
  const say = (text, transient = false) => {
    const now = performance.now();
    if (!transient && now < noteUntil) return;
    if (transient) noteUntil = now + 2500;
    hint.textContent = text;
    hint.hidden = !text;
    const b = r();
    hint.style.left = `${b.left + 12}px`;
    hint.style.top = `${b.bottom - 12}px`;
    if (transient) setTimeout(() => (focus ? describe(!!onScreen(game(), focus)) : say("")), 2600);
  };

  // Where an object is on screen, if a click there would reach it right
  // now. Every candidate point is confirmed by the click's own pick
  // (hoverAt), which also rules out anything standing in front of it.
  //
  // Two ways to find that point. The object's origin, projected, is the
  // cheap one, but an origin need not lie on the object's polygons (a
  // door's hinge, a lift's base…). So targets() also sweeps a coarse grid
  // of picks over the view and remembers, per object, where on screen it
  // was hit, as an offset from its projected origin: the point then
  // follows the object as it (or the camera) moves.
  const r = () => canvas.getBoundingClientRect();
  const inside = (p, b) => p && p[0] > b.left + 1 && p[0] < b.right - 1 && p[1] > b.top + 1 && p[1] < b.bottom - 1;
  function onScreen(g, t) {
    const b = r();
    const o = g.view.project(g.engine.sim.object(t.id).position);
    const p = o && t.offset ? [o[0] + t.offset[0], o[1] + t.offset[1]] : t.at;
    return inside(p, b) && hoverAt(g.engine, g.view, p[0], p[1]) === t.id ? (t.at = p) : null;
  }
  const GRID = [24, 14]; // 336 picks, about 15 ms, only when Tab is pressed
  // Every object a click would set in motion that the player can see,
  // left to right (then top to bottom), as reading order goes.
  function targets(g) {
    const b = r();
    const found = new Map(); // id → { id, draw, at, offset }
    const add = (o, at) => {
      const p = g.view.project(o.position);
      found.set(o.id, { id: o.id, draw: o.draw, at, offset: p ? [at[0] - p[0], at[1] - p[1]] : null });
    };
    const all = new Map(clickableObjects(g.engine).filter((o) => o.draw >= 0).map((o) => [o.id, o]));
    for (const o of all.values()) {
      const p = g.view.project(o.position);
      if (inside(p, b) && hoverAt(g.engine, g.view, p[0], p[1]) === o.id) add(o, p);
    }
    if (found.size < all.size) {
      const hits = new Map(); // id → screen points that reach it
      for (let i = 0; i < GRID[0]; i++)
        for (let j = 0; j < GRID[1]; j++) {
          const x = b.left + ((i + 0.5) / GRID[0]) * b.width, y = b.top + ((j + 0.5) / GRID[1]) * b.height;
          const id = hoverAt(g.engine, g.view, x, y);
          if (all.has(id) && !found.has(id)) hits.has(id) ? hits.get(id).push([x, y]) : hits.set(id, [[x, y]]);
        }
      // The hit nearest the middle of an object's hits: well inside it.
      for (const [id, pts] of hits) {
        const mx = pts.reduce((s, p) => s + p[0], 0) / pts.length, my = pts.reduce((s, p) => s + p[1], 0) / pts.length;
        add(all.get(id), pts.reduce((a, c) => (Math.hypot(c[0] - mx, c[1] - my) < Math.hypot(a[0] - mx, a[1] - my) ? c : a)));
      }
    }
    return [...found.values()].sort((a, c) => a.at[0] - c.at[0] || a.at[1] - c.at[1]);
  }
  function step(dir) {
    const g = game();
    const list = targets(g);
    if (!list.length) {
      focus = null;
      g.view.setHighlight(null);
      return say("Nothing clickable in view. Esc: menu.", true);
    }
    const i = focus ? list.findIndex((o) => o.id === focus.id) : -1;
    const next = list[i < 0 ? (dir > 0 ? 0 : list.length - 1) : (i + dir + list.length) % list.length];
    focus = next;
    describe(true);
  }
  const k = (key) => keyName(get(key));
  const describe = (visible) =>
    say(focus ? `Object #${focus.id}${visible ? "" : " (out of reach)"} · ${k("keyboardClick")}: click · ${k("keyboardNext")}: next · ${k("keyboardTouch")}: touch Mendel` : "");
  function clickFocused() {
    const g = game();
    const p = focus && onScreen(g, focus);
    if (!p) return say(focus ? `Object #${focus.id} cannot be clicked from here.` : `${k("keyboardNext")} first, to pick an object.`, !focus);
    g.view.click(p[0], p[1], 0); // the mouse's own path: main.js's onPick
    say(`Clicked object #${focus.id}.`, true);
  }
  // A right click on the side we want: sample points of his mesh, ask the
  // engine (without touching) which side a click there would reach, then
  // right-click the middle of the wanted side's points.
  function touchMendel() {
    const g = game();
    const want = touchSide === "left" ? "right" : "left";
    const verts = g.engine.creature.vertices();
    const n = verts.length / 10;
    if (!n) return say("Mendel is not here.", true);
    const sides = { left: [], right: [] };
    const stride = Math.max(1, Math.floor(n / 80));
    for (let k = 0; k < n; k += stride) {
      const p = g.view.project([verts[k * 10], verts[k * 10 + 1], verts[k * 10 + 2]]);
      if (!p) continue;
      const side = g.engine.creature.side(...g.view.rayAt(p[0], p[1]));
      if (side) sides[side].push(p);
    }
    const pts = sides[want].length ? sides[want] : sides[touchSide];
    if (!pts.length) return say("Mendel is out of reach.", true);
    // The point nearest the middle of that side's points: well inside him.
    const mx = pts.reduce((s, p) => s + p[0], 0) / pts.length, my = pts.reduce((s, p) => s + p[1], 0) / pts.length;
    const p = pts.reduce((a, b) => (Math.hypot(b[0] - mx, b[1] - my) < Math.hypot(a[0] - mx, a[1] - my) ? b : a));
    touchSide = sides[want].length ? want : touchSide;
    g.view.click(p[0], p[1], 2); // the mouse's own path: main.js's onTouch
    say(`Touched Mendel's ${touchSide} side.`, true);
  }
  // The mouse takes over again on a real click, or as soon as it moves
  // while the hover highlight (#41) wants the same glow.
  const drop = () => {
    if (!focus) return;
    focus = null;
    game()?.view.setHighlight(null);
    say("");
  };
  canvas.addEventListener("pointerdown", drop);
  canvas.addEventListener("pointermove", (e) => (e.movementX || e.movementY) && get("hoverHighlight") && drop());
  keyboard.on((on) => {
    // A canvas is not focusable by default; this lets a click or Tab land on it.
    if (on) canvas.tabIndex = 0;
    else canvas.removeAttribute("tabindex");
    if (!on && focus) {
      focus = null;
      game()?.view.setHighlight(null);
    }
    say("");
    run();
  });

  // ——— Visual cues for sounds ———
  const layer = el("div", "a11y-cues");
  layer.setAttribute("aria-live", "polite");
  document.body.append(layer);
  const shown = new Map(); // key → { node, until, where }
  // `where()` returns a world position (an arrow towards it) or null (a
  // fixed spot at the bottom edge).
  function cue(key, text, where = () => null) {
    if (!cues.get()) return;
    let c = shown.get(key);
    if (!c) {
      const node = el("div", "a11y-cue");
      node.append(el("i", "a11y-cue__arrow", "▲"), el("span", "a11y-cue__text"));
      layer.append(node);
      c = { node };
      shown.set(key, c);
    }
    c.node.lastChild.textContent = text;
    c.node.classList.remove("is-fading");
    c.until = performance.now() + CUE_MS;
    c.where = where;
    run();
  }
  // The edge point in direction (dx, dy) from the view's centre, inset.
  function place(c, g, b) {
    const pos = c.where();
    const cx = b.left + b.width / 2, cy = b.top + b.height / 2;
    let dx = 0, dy = 1; // no position: the bottom edge, middle
    if (pos) {
      const p = g.view.project(pos);
      if (p && p[0] >= b.left && p[0] <= b.right && p[1] >= b.top && p[1] <= b.bottom) {
        [dx, dy] = [p[0] - cx, p[1] - cy]; // on screen: point at it
      } else {
        // Off screen: from the camera's frame. Behind the camera is "down".
        const ear = g.view.listener();
        const d = pos.map((v, i) => v - ear.eye[i]);
        const right = [ear.forward[1] * ear.up[2] - ear.forward[2] * ear.up[1], ear.forward[2] * ear.up[0] - ear.forward[0] * ear.up[2], ear.forward[0] * ear.up[1] - ear.forward[1] * ear.up[0]];
        const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        const ahead = dot(d, ear.forward);
        [dx, dy] = ahead > 0 ? [dot(d, right), -dot(d, ear.up)] : [dot(d, right), Math.abs(ahead)];
      }
      if (!dx && !dy) dy = -1;
    }
    // Keep the whole marker inside the view, 8 px clear of its edge.
    const mx = c.node.offsetWidth / 2 + 8, my = c.node.offsetHeight / 2 + 8;
    const t = Math.min((b.width / 2 - mx) / Math.abs(dx || 1e-9), (b.height / 2 - my) / Math.abs(dy || 1e-9));
    c.node.style.left = `${cx + dx * t}px`;
    c.node.style.top = `${cy + dy * t}px`;
    c.node.firstChild.style.transform = `rotate(${Math.atan2(dx, -dy)}rad)`;
    c.node.firstChild.hidden = !pos;
  }
  // The mixer's door: every sound main.js asks for passes by us first.
  // Wrapping the methods once per mixer keeps main.js to one line.
  const hooked = new WeakSet();
  function hook(g) {
    if (hooked.has(g.sfx)) return;
    hooked.add(g.sfx);
    const { creature, predator } = g.sfx;
    g.sfx.creature = (sound, ...rest) => {
      cue("mendel", `Mendel ${MENDEL[sound] ?? "calls out"}`, () => g.engine.creature.state().position);
      return creature.call(g.sfx, sound, ...rest);
    };
    g.sfx.predator = (sound, id, ...rest) => {
      cue(`predator:${id}`, sound === 0 ? "Predator growls" : "Predator bites", () => g.engine.sim.object(id).position);
      return predator.call(g.sfx, sound, id, ...rest);
    };
    const offer = g.menu.offerSave;
    g.menu.offerSave = (...a) => {
      cue("save", "Save pad: save offered");
      return offer.apply(g.menu, a);
    };
  }
  cues.on((on) => {
    if (!on) for (const [k, c] of shown) c.node.remove(), shown.delete(k);
    run();
  });

  // ——— One loop for all of it, only while something needs it ———
  function run() {
    if (!raf) raf = requestAnimationFrame(tick);
  }
  function tick(now) {
    raf = 0;
    const g = game();
    if (!g) return void (raf = requestAnimationFrame(tick)); // the CD is still loading
    hook(g);
    applyView(); // cheap: the view only stores the numbers
    // A new section: object ids no longer mean the same thing.
    const s = g.engine.section();
    if (s !== section) {
      section = s;
      if (focus) (focus = null), say("");
    }
    if (keyboard.get() && focus) {
      const ok = !!onScreen(g, focus);
      g.view.setHighlight(ok ? focus.draw : null);
      describe(ok);
    }
    if (cues.get()) {
      const pad = g.engine.game.onSavePad();
      if (pad) cue("save", "Mendel is on a save pad: click it to save");
      const b = r();
      for (const [k, c] of shown) {
        if (now > c.until + FADE_MS) {
          c.node.remove();
          shown.delete(k);
          continue;
        }
        if (now > c.until) c.node.classList.add("is-fading");
        place(c, g, b);
      }
    }
    // Colours and flashes need the view refreshed once; the rest per frame.
    if ((keyboard.get() && focus) || shown.size || cues.get()) raf = requestAnimationFrame(tick);
  }
  run();
}
