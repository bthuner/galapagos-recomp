// Stats: Pause ▸ Stats (#50). Not in the 1997 game.
//
// What it keeps, per world (section 0–7) and overall: time played and
// active time, deaths by cause, completions and the best time, clicks on
// objects, touches on Mendel, saves and loads, the furthest landmark
// reached, distance walked, leaps, afflictions caught and the GA
// generations his brain evolved.
//
// ——— Where the numbers come from ———
//
// Two sources, and the line between them is the point of the design.
//
// * The engine keeps what only the simulation can see: *why* Mendel died
//   (a lethal surface and its kind, a fall, a predator and its class),
//   leaps, afflictions, generations, distance. That is `Tally`
//   (crates/engine/src/tally.rs), read through `engine.stats()`: a fixed
//   array of 52 numbers that only ever grow. We never ask "how many deaths
//   in total?"; we ask "how many since the last reading?" and add the
//   difference to the world Mendel is in. Subtraction makes the engine's
//   counters per-world for free.
// * The browser sees what the player does: clicks, right-click touches,
//   saves and loads, time. We watch them by wrapping the engine calls
//   main.js already makes (`engine.sim.tick`, `.click`, `.creature.touch`,
//   `.game.save`, `.game.load`): each wrapper calls the original, counts,
//   and returns the original's result untouched. No game code changes.
//
// ——— Why nothing is counted twice ———
//
// The tally is not part of a saved game nor of the respawn snapshot. A
// respawn rewinds the world, not the tally; opening a save (from the
// Open panel, or Continue once #39 lands) restores the game, not the
// tally. So a load adds exactly one to "games loaded" and changes nothing
// else: the time and deaths before that save were counted when they
// happened. Only File ▸ New starts the engine's tally from zero, and a
// reading smaller than the last one is simply taken as a new start.
//
// Stats never feed back into the game: every wrapper returns what the
// engine returned, and reading the tally changes nothing in it.
//
// Saved in localStorage under "galapagos-stats", away from the game's
// saves and from the QoL settings; Reset in the panel erases only this.

const STORE = "galapagos-stats";
const VERSION = 1;

// ——— The engine's snapshot (Tally::snapshot) ———
const S = {
  deaths: 1, fall: 2, predator: 3, floor: 8, affliction: 24,
  leaps: 37, landings: 38, generations: 39, bumps: 40, ticksAlive: 41, walked: 42,
  bestCost: 43, individual: 46, present: 49,
};
const GROWING = 43; // slots 1..42 only grow

const PREDATORS = ["Pred", "Ball (PBal)", "Fork (PFrk)", "Man (PMan)", "Pole (PPol)"];
// Lethal surfaces are named by the transition effect they play
// (crates/engine/src/transition.rs).
const FLOORS = ["", "fade", "burst", "squash", "jolt", "melt", "stretch", "shatter"];
const AFFLICTIONS = ["", "shield", "fast muscles", "speed", "camouflage", "blind", "confused",
  "agitated", "agitated", "agitated", "low gravity", "no gravity", "heavy gravity"];

// ——— Landmarks: how far into a world Mendel got ———
// Objects he can stand on, in the order the world is crossed; the
// furthest one he has stood on is the world's "furthest point". The
// gate is reached when the section ends through it.
// TODO sections 1–7: only section 0 is mapped so far.
const LANDMARKS = {
  0: [
    [57, "start disc"],
    [83, "arrival pad"],
    [84, "save pad"],
    [94, "hall walkway"],
    [79, "white corridor"],
    [111, "lift"],
    [110, "jump platform"],
  ],
};
const GATE = "gate";

// ——— The record ———
const blankWorld = () => ({
  played: 0, active: 0,
  deaths: 0, fall: 0, predator: Array(5).fill(0), floor: Array(16).fill(0),
  completions: 0, best: null, // seconds, the game's own level clock
  clicks: 0, touches: 0, saves: 0, loads: 0,
  furthest: -1, // index into LANDMARKS[world]; its length = the gate
  walked: 0, leaps: 0, landings: 0, bumps: 0, generations: 0,
  afflictions: Array(13).fill(0),
});
const blank = () => ({ version: VERSION, since: Date.now(), worlds: [], mendel: { bestCost: [0, 0, 0], individual: [0, 0, 0] } });

function load() {
  try {
    const r = JSON.parse(localStorage.getItem(STORE) || "null");
    if (r && r.version === VERSION && Array.isArray(r.worlds)) {
      // Fill in fields a newer version added, so old records keep working.
      r.worlds = r.worlds.map((w) => (w ? { ...blankWorld(), ...w } : null));
      return { ...blank(), ...r };
    }
  } catch {
    // a corrupt entry must not stop the game: start afresh
  }
  return blank();
}

/**
 * Start keeping statistics, and fill `menu.statsPanel` each time
 * Pause ▸ Stats opens. `engine` is the object loadEngine() returned.
 */
export function stats({ engine, menu }) {
  let rec = load();
  const hz = engine.sim.ticksPerSecond();
  const world = (i = engine.section()) => (rec.worlds[i] ||= blankWorld());
  let dirty = false;
  const persist = () => {
    if (!dirty) return;
    localStorage.setItem(STORE, JSON.stringify(rec));
    dirty = false;
  };

  // ——— Engine counters: add the difference since the last reading ———
  let last = null;
  function sample() {
    const now = engine.stats();
    if (!last) last = new Uint32Array(now.length);
    const w = world();
    const d = (k) => {
      const v = now[k], before = last[k];
      return v >= before ? v - before : v; // smaller: File ▸ New restarted the tally
    };
    const n = d(S.deaths);
    if (n) {
      w.deaths += n;
      w.fall += d(S.fall);
      for (let k = 0; k < 5; k++) w.predator[k] += d(S.predator + k);
      for (let k = 0; k < 16; k++) w.floor[k] += d(S.floor + k);
    }
    for (let k = 1; k < 13; k++) w.afflictions[k] += d(S.affliction + k);
    w.leaps += d(S.leaps);
    w.landings += d(S.landings);
    w.generations += d(S.generations);
    w.bumps += d(S.bumps);
    w.walked += d(S.walked);
    if (now[S.present]) {
      rec.mendel.bestCost = [0, 1, 2].map((k) => now[S.bestCost + k] | 0);
      rec.mendel.individual = [0, 1, 2].map((k) => now[S.individual + k]);
    }
    for (let k = 1; k < GROWING; k++) if (now[k] !== last[k]) dirty = true;
    last.set(now);
    landmark(w);
  }

  // The furthest landmark: what he stands on, ranked by the world's list.
  function landmark(w) {
    const list = LANDMARKS[engine.section()];
    const ground = list && engine.creature.state().ground;
    if (!ground) return;
    const k = list.findIndex(([id]) => id === ground.object);
    if (k > w.furthest) (w.furthest = k), (dirty = true);
  }

  // ——— Wrappers: count, then hand back exactly what the engine said ———
  const wrap = (obj, name, after, before) => {
    const original = obj[name];
    obj[name] = (...args) => {
      before?.();
      const r = original.apply(obj, args);
      after(r, args);
      return r;
    };
  };
  let frames = 0, ticked = false;
  wrap(engine.sim, "tick", (events) => {
    ticked = true; // the world ran this second: it was not paused
    for (const ev of events) {
      // A dimensional gate (2) or the last portal (7): the world is done.
      // Read the level clock now, before main.js builds the next section.
      if (ev.kind === 2 || ev.kind === 7) {
        const w = world();
        const t = engine.game.times().levelTicks / hz;
        w.completions++;
        w.best = w.best === null ? t : Math.min(w.best, t);
        w.furthest = (LANDMARKS[engine.section()] || []).length; // the gate
        dirty = true;
      }
    }
    // Every fourth frame is plenty: the counters cannot be missed, only delayed.
    if (++frames % 4 === 0 || events.length) sample();
  });
  wrap(engine.sim, "click", (id) => id && (world().clicks++, (dirty = true)));
  wrap(engine.creature, "touch", (side) => side && (world().touches++, (dirty = true)));
  wrap(engine.game, "save", () => {
    if (quiet) return;
    sample();
    world().saves++;
    dirty = true;
    persist();
  });
  wrap(engine.game, "load", (section) => {
    world(section).loads++;
    dirty = true;
    persist();
  }, sample); // settle the old world's account before the save replaces it

  // ——— Time played: wall-clock seconds with the world on screen ———
  // Played counts paused time too (the menu, the time screen); active
  // only the seconds in which the simulation ran. Neither counts while
  // the tab is hidden: a forgotten tab is not time played. Wall-clock, not
  // ticks: a slow machine that runs fewer ticks still played those seconds.
  let wall = performance.now();
  setInterval(() => {
    const now = performance.now();
    const dt = Math.min((now - wall) / 1000, 5);
    wall = now;
    const ran = ticked;
    ticked = false;
    if (document.hidden) return;
    world().played += dt;
    if (ran) world().active += dt;
    dirty = true;
    persist();
  }, 1000);
  addEventListener("pagehide", () => (sample(), persist()));
  document.addEventListener("visibilitychange", () => {
    wall = performance.now();
    if (document.hidden) persist();
  });

  // ——— The panel ———
  let confirming = false;
  const render = () => {
    sample();
    persist();
    menu.statsPanel.replaceChildren(...panel(rec, {
      confirming,
      ask: () => ((confirming = true), render()),
      cancel: () => ((confirming = false), render()),
      reset: () => {
        rec = blank();
        localStorage.removeItem(STORE);
        confirming = false;
        render();
      },
    }));
  };
  menu.onShow("stats", () => ((confirming = false), render()));
}

/** Run `fn` without counting a save (for autosave, #39). */
let quiet = false;
export function uncounted(fn) {
  quiet = true;
  try {
    return fn();
  } finally {
    quiet = false;
  }
}

// ——— Drawing ———

const el = (tag, props = {}, ...children) => {
  const e = Object.assign(document.createElement(tag), props);
  e.append(...children);
  return e;
};
const int = (n) => Math.round(n).toLocaleString("en");
function clock(s) {
  s = Math.floor(s);
  const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, sec = s % 60;
  const two = (v) => String(v).padStart(2, "0");
  return h ? `${h}:${two(m)}:${two(sec)}` : `${m}:${two(sec)}`;
}
const sum = (ws, f) => ws.reduce((a, w) => a + (w ? f(w) : 0), 0);
const sumArr = (ws, key, n) => Array.from({ length: n }, (_, k) => sum(ws, (w) => w[key][k]));
const furthestName = (i, w) => {
  const list = LANDMARKS[i] || [];
  return w.furthest < 0 ? "—" : w.furthest >= list.length ? GATE : list[w.furthest][1];
};

function causes(fall, predator, floor) {
  const out = [];
  if (fall) out.push(["fall", fall]);
  predator.forEach((n, k) => n && out.push([PREDATORS[k], n]));
  floor.forEach((n, k) => n && out.push([`lethal floor: ${FLOORS[k] || `kind ${k}`}`, n]));
  return out;
}
const pills = (pairs, empty) =>
  pairs.length
    ? el("ul", { className: "stats__pills" }, ...pairs.map(([label, n]) => el("li", {}, el("b", { textContent: int(n) }), ` ${label}`)))
    : el("p", { className: "menu__note", textContent: empty });

function panel(rec, ui) {
  const ws = rec.worlds;
  const played = sum(ws, (w) => w.played);
  const active = sum(ws, (w) => w.active);
  const deaths = sum(ws, (w) => w.deaths);
  const reached = ws.filter((w) => w && w.played > 0).length;
  const generations = sum(ws, (w) => w.generations);

  const tile = (label, value, note) =>
    el("div", { className: "stats__tile" },
      el("span", { className: "stats__label", textContent: label }),
      el("strong", { className: "stats__value", textContent: value }),
      note ? el("small", { textContent: note }) : "");
  const cost = rec.mendel.bestCost.map((c) => (c ? (c / 65536).toFixed(2) : "–")).join(" · ");
  const tiles = el("div", { className: "stats__tiles" },
    tile("Time played", clock(played), `${clock(active)} active`),
    tile("Deaths", int(deaths)),
    tile("Worlds reached", `${reached} / 8`),
    tile("Mendel's age", `${int(generations)} gen.`, `best cost ${cost}`),
    tile("Walked", int(sum(ws, (w) => w.walked)), "world units"),
    tile("Leaps", int(sum(ws, (w) => w.leaps)), `${int(sum(ws, (w) => w.landings))} hard landings`),
  );

  const death = causes(sum(ws, (w) => w.fall), sumArr(ws, "predator", 5), sumArr(ws, "floor", 16));
  const ill = sumArr(ws, "afflictions", 13).map((n, k) => [AFFLICTIONS[k], n]).filter(([l, n]) => l && n);

  // The per-world table; a row opens its details.
  const head = el("tr", {}, ...["World", "Played", "Deaths", "Done", "Best", "Furthest"].map((t) => el("th", { textContent: t })));
  const body = el("tbody");
  for (let i = 0; i < Math.max(8, ws.length); i++) {
    const w = ws[i];
    if (!w) {
      body.append(el("tr", { className: "stats__unreached" }, el("td", { textContent: String(i) }), el("td", { colSpan: 5, textContent: "not reached" })));
      continue;
    }
    const detail = el("tr", { className: "stats__detail", hidden: true },
      el("td", { colSpan: 6 },
        el("p", { textContent:
          `${clock(w.active)} active · ${int(w.clicks)} clicks · ${int(w.touches)} touches · ` +
          `${int(w.saves)} saves · ${int(w.loads)} loads · walked ${int(w.walked)} · ${int(w.leaps)} leaps · ` +
          `${int(w.generations)} generations` }),
        pills(causes(w.fall, w.predator, w.floor), "No deaths here.")));
    const name = el("button", { type: "button", className: "stats__world", textContent: String(i), title: "Details" });
    name.setAttribute("aria-expanded", "false");
    name.onclick = () => {
      detail.hidden = !detail.hidden;
      name.setAttribute("aria-expanded", String(!detail.hidden));
    };
    body.append(
      el("tr", {},
        el("td", {}, name),
        el("td", { textContent: clock(w.played) }),
        el("td", { textContent: int(w.deaths) }),
        el("td", { textContent: int(w.completions) }),
        el("td", { textContent: w.best === null ? "—" : clock(w.best) }),
        el("td", { textContent: furthestName(i, w) })),
      detail);
  }

  const reset = ui.confirming
    ? el("div", { className: "stats__confirm", role: "alert" },
        el("span", { textContent: "Erase every statistic? Saved games are kept." }),
        el("button", { type: "button", className: "qol__button stats__danger", textContent: "Erase", onclick: ui.reset }),
        el("button", { type: "button", className: "qol__button", textContent: "Keep", onclick: ui.cancel }))
    : el("button", { type: "button", className: "qol__button", textContent: "Reset stats", onclick: ui.ask });

  return [
    el("p", { className: "menu__note" },
      el("span", { className: "qol__tag", textContent: "Not in the 1997 game" }),
      ` Kept on this browser since ${new Date(rec.since).toLocaleDateString()}.`),
    tiles,
    el("h3", { className: "qol__group", textContent: "Deaths by cause" }),
    pills(death, "Mendel has not died yet."),
    el("h3", { className: "qol__group", textContent: "Afflictions caught" }),
    pills(ill, "None yet."),
    el("h3", { className: "qol__group", textContent: "Per world" }),
    el("table", { className: "stats__table" }, el("thead", {}, head), body),
    el("div", { className: "stats__reset" }, reset),
  ];
}
