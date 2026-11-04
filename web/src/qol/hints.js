// First-time hints (#42): one small line, once, the first time the player
// meets a mechanic. The 1997 game taught nothing on screen (the manual did),
// so this is off by default and labelled as an addition.
//
// How it works, in four ideas:
//
// 1. Signals, not hooks. A hint watches what the page already exposes: the
//    engine handle main.js publishes as `globalThis.galapagos` (Mendel's
//    state, the section, "is he on a save pad?") and the DOM (is the pause
//    menu, the movie or the time screen up?). So the game itself needs no
//    new code, and this file is the whole feature.
// 2. One watcher. A 4 Hz timer looks at those signals, but only while the
//    setting is on: off, the feature costs nothing.
// 3. Once means once. A hint is remembered in localStorage
//    ("galapagos.hints") the moment it is shown, and also when the player
//    proves they know it already (pressing Esc before we suggest it).
//    "Reset hints" forgets them all.
// 4. Never in the way. Bottom-left of the 3-D view (the centre is Mendel's,
//    the bottom centre belongs to toasts), one at a time, gone after a few
//    seconds or on ×, and held back while a menu, a movie or the time screen
//    is up: a hint's clock only runs while the player can see the world.

import { register } from "../qol.js";

const STORE = "galapagos.hints";
const SHOW_MS = 9000; // how long a hint stays (paused while hidden)
const GAP_MS = 4000; // breathing room between two hints

// The hints, in priority order (the first one that is due wins).
//   when(s)  is it due, given the current signals `s`?
//   until(s) optional: once shown, withdraw it when this becomes true
//            (the moment has passed, e.g. the intro ride ended)
const HINTS = [
  {
    id: "skip",
    text: ["Hold ", kbd("Space"), " to skip the introduction"],
    // World 0 before Mendel is born: the capsule ride. Give the opening
    // a couple of seconds first.
    when: (s) => s.section === 0 && s.mendel === "absent" && s.playMs > 2500,
    until: (s) => s.section !== 0 || s.mendel !== "absent",
  },
  {
    id: "click",
    text: ["Click objects to change the world"],
    when: (s) => s.mendel === "alive",
  },
  {
    id: "touch",
    text: ["Right-click Mendel to touch him"],
    when: (s) => s.mendel === "alive" && s.aliveMs > 20000,
  },
  {
    id: "savePad",
    text: ["Click the save pad he's standing on to save"],
    when: (s) => s.onSavePad,
    until: (s) => !s.onSavePad,
  },
  {
    id: "menu",
    text: [kbd("Esc"), " opens the menu"],
    // After half a minute, but not in the middle of the intro ride: once
    // Mendel has been around for a moment (or after 90 s regardless).
    when: (s) => (s.playMs > 30000 && s.aliveMs > 8000) || s.playMs > 90000,
  },
  {
    id: "gate",
    text: ["A gate takes Mendel to the next world"],
    when: (s) => s.gatesSeen > 0,
  },
];

function kbd(name) {
  return Object.assign(document.createElement("kbd"), { textContent: name });
}

let seen = {};
try {
  seen = JSON.parse(localStorage.getItem(STORE) || "{}") || {};
} catch {
  seen = {};
}
const remember = (id) => {
  if (seen[id]) return;
  seen[id] = true;
  localStorage.setItem(STORE, JSON.stringify(seen));
};

export function hints() {
  const setting = register({
    key: "hints",
    label: "Hints",
    help: "A short tip, once, the first time Mendel or you meet something new.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group: "Comfort",
    order: 30,
  });
  register({
    key: "resetHints",
    label: "Reset hints",
    help: "Show every hint again, as on a first visit.",
    type: "button",
    buttonLabel: "Reset",
    notOriginal: true,
    group: "Comfort",
    order: 31,
    action: () => {
      seen = {};
      localStorage.removeItem(STORE);
      close(true);
    },
  });

  const $ = (id) => document.getElementById(id);

  // ——— The box ———
  const box = document.createElement("aside");
  box.className = "hint";
  box.hidden = true;
  box.setAttribute("role", "status"); // read out politely, never steals focus
  const text = document.createElement("p");
  const dismiss = Object.assign(document.createElement("button"), {
    type: "button",
    className: "hint__close",
    textContent: "×",
    title: "Dismiss",
    onclick: () => close(),
  });
  dismiss.setAttribute("aria-label", "Dismiss hint");
  box.append(text, dismiss);

  // ——— The player shows they know ———
  // These run whether hints are on or off: a player who already pressed Esc
  // never needs to be told about it. Capture phase, passive: we only look.
  const learnt = (id) => {
    remember(id);
    if (current?.id === id) close();
  };
  addEventListener("keydown", (e) => {
    // Esc also skips the intro movie: only count it once the menu really opened.
    if (e.code === "Escape") setTimeout(() => $("menu")?.hidden === false && learnt("menu"));
    if (e.code === "Space" && current?.id === "skip") close(); // doing it: thanks, bye
  }, true);
  addEventListener("pointerdown", (e) => {
    if (e.target?.id !== "world") return;
    if (e.button === 2) learnt("touch");
    else if (e.button === 0 && current?.id === "click") close();
  }, true);

  // ——— Signals ———
  const s = { section: -1, mendel: "absent", onSavePad: false, playMs: 0, aliveMs: 0, gatesSeen: 0 };
  let timeScreenWasUp = false;

  // Can the player see the world right now? (No menu, movie or time screen.)
  const clear = () => {
    const world = $("world");
    const movie = $("screen")?.parentElement;
    return !!globalThis.galapagos && world && !world.hidden && (!movie || movie.hidden) &&
      $("menu")?.hidden !== false && $("time-screen")?.hidden !== false && !document.hidden;
  };

  // ——— The watcher ———
  let timer = 0, last = 0;
  let current = null; // { id, left (ms of visible time remaining), until }
  let quietMs = 0; // time since the last hint closed

  function close(all = false) {
    current = null;
    quietMs = 0;
    box.classList.remove("is-on");
    if (all) box.hidden = true;
  }

  function open(h) {
    remember(h.id); // once means once, even if the tab is closed right now
    current = { id: h.id, left: SHOW_MS, until: h.until };
    text.replaceChildren(...h.text.map((t) => (typeof t === "string" ? t : t.cloneNode(true))));
    // The world frame, so the hint sits on the 3-D view and moves with it.
    const frame = $("world")?.parentElement || document.body;
    if (box.parentElement !== frame) frame.append(box);
    box.hidden = false;
    requestAnimationFrame(() => box.classList.add("is-on")); // fade in
  }

  function tick() {
    const now = performance.now();
    const dt = Math.min(now - last, 1000); // a background tab is not "play time"
    last = now;

    const tsUp = $("time-screen")?.hidden === false;
    if (timeScreenWasUp && !tsUp) s.gatesSeen++;
    timeScreenWasUp = tsUp;
    // The Save Pad dialog came up: he found one and the player clicked it.
    if ($("menu")?.hidden === false && document.querySelector('[data-panel="save"]')?.hidden === false) remember("savePad");

    if (!clear()) {
      box.classList.remove("is-on"); // hidden, timer paused, shown again after
      return;
    }
    const { engine } = globalThis.galapagos;
    const c = engine.creature.state();
    s.section = engine.section();
    s.mendel = c.mode;
    s.onSavePad = c.mode === "alive" && engine.game.onSavePad();
    s.playMs += dt;
    s.aliveMs = c.mode === "alive" ? s.aliveMs + dt : s.aliveMs;

    if (current) {
      box.classList.add("is-on");
      current.left -= dt;
      if (current.left <= 0 || current.until?.(s)) close();
      return;
    }
    quietMs += dt;
    if (quietMs < GAP_MS) return;
    const due = HINTS.find((h) => !seen[h.id] && h.when(s));
    if (due) open(due);
  }

  setting.on((on) => {
    clearInterval(timer);
    if (!on) return close(true);
    last = performance.now();
    quietMs = GAP_MS; // the first hint need not wait
    timer = setInterval(tick, 250);
  });
}
