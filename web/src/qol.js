// Quality of Life: one registry for every optional comfort feature (#37).
//
// The 1997 game is the default. Everything here is an *addition*, so it
// lives in its own pause-menu panel (Pause ▸ Quality of Life), in its own
// localStorage key ("galapagos.qol", never mixed with the original's
// "galapagos.prefs"), and every setting says plainly whether the original
// had it.
//
// ——— Adding a Quality of Life feature ———
//
// 1. Write your feature as its own module, e.g. `web/src/qol/game-speed.js`,
//    exporting one function that main.js calls once (pass it what it needs:
//    the engine, the view…). That call is the only line you add elsewhere.
// 2. Inside it, register your setting(s) and react to them:
//
//      import { register } from "../qol.js";
//      const speed = register({
//        key: "speed",                 // storage key: never rename it
//        label: "Game speed",
//        help: "Fast-forward while Mendel paces about.",
//        type: "choice",               // "toggle" | "choice" | "button" | "key"
//        options: [{ value: 1, label: "×1" }, { value: 2, label: "×2" }, { value: 4, label: "×4" }],
//        default: 1,
//        notOriginal: true,            // shows "Not in the 1997 game"
//        group: "Pacing",
//        shortcut: "KeyF",             // optional: cycles the choice
//      });
//      speed.on((v) => (ticksPerSlot = v)); // also called right away
//
// 3. Defaults policy (#37). Anything that changes gameplay or pacing
//    (speed, rewind, hints…) defaults OFF. Pure comfort that never changes
//    what the original did (autosave, Continue…) may default ON.
// 4. Label honestly: `notOriginal: true` for anything the 1997 game did not
//    have. `false` is for faithful options that merely live here.
// 5. Add a line to the making-of journal.
//
// Shortcuts are `KeyboardEvent.code` strings ("KeyF", "Backquote", "F2"):
// physical keys, so they sit in the same place on AZERTY and QWERTY. They are
// dispatched here, centrally, so features never add their own keydown
// listeners and never fight over a key. A shortcut is ignored while the
// player types in a text field, on auto-repeat, with Ctrl/Alt/Meta held
// (those belong to the browser), and while the pause menu is open unless the
// setting says `inMenu: true`. Escape and Space are the game's own and
// cannot be taken.

const STORE = "galapagos.qol";
const RESERVED = new Set(["Escape", "Space"]);
// Panel order of the groups #37 names; any other group follows, in the order
// it was first registered.
const GROUPS = ["Comfort", "Pacing", "Explore", "Accessibility"];

const settings = new Map(); // key → normalised definition, in registration order
const listeners = new Map(); // key → Set of functions
let values = {};
try {
  values = JSON.parse(localStorage.getItem(STORE) || "{}") || {};
} catch {
  values = {}; // a corrupt entry must not stop the game from booting
}
const persist = () => localStorage.setItem(STORE, JSON.stringify(values));

// Options may be given as plain values ("Off") or as { value, label }.
const normOption = (o) => (typeof o === "object" ? { value: o.value, label: String(o.label ?? o.value) } : { value: o, label: String(o) });

// Is a stored value still acceptable? Options change between versions; a
// value that no longer exists falls back to the default rather than breaking.
function valid(def, v) {
  switch (def.type) {
    case "toggle": return typeof v === "boolean";
    case "choice": return def.options.some((o) => o.value === v);
    case "key": return typeof v === "string" && !RESERVED.has(v);
    default: return false; // buttons hold no value
  }
}

/**
 * Declare a setting. Returns a handle: { key, get(), set(v), on(fn) }.
 *
 *   key          unique storage key (stable forever: it is the saved name)
 *   label        what the panel shows
 *   help         optional one-line explanation under the label
 *   type         "toggle" (boolean), "choice" (one of `options`),
 *                "button" (an action, e.g. "Reset hints"; holds no value),
 *                "key" (a rebindable shortcut: the value is a key code)
 *   options      for "choice": values or { value, label }
 *   default      initial value (ignored for buttons)
 *   notOriginal  true if the 1997 game had no such thing (required)
 *   group        panel heading, default "Comfort"
 *   order        sort order inside the group (default 0; then registration)
 *   shortcut     optional key code: flips a toggle, cycles a choice,
 *                presses a button
 *   inMenu       the shortcut also works while the pause menu is open
 *   action       for "button" and "key": what pressing it does; a "key"
 *                action receives the KeyboardEvent (to read e.shiftKey)
 *   active       optional () => boolean: the shortcut is only taken while
 *                it returns true (otherwise the key keeps its usual job,
 *                e.g. Tab still moves focus outside the game view)
 */
export function register(def) {
  if (settings.has(def.key)) throw new Error(`QoL setting "${def.key}" is registered twice`);
  if (typeof def.notOriginal !== "boolean") throw new Error(`QoL setting "${def.key}": say whether it is notOriginal`);
  const d = {
    group: "Comfort",
    order: 0,
    ...def,
    options: (def.options || []).map(normOption),
  };
  if (d.type === "key" && !valid(d, d.default)) throw new Error(`QoL key "${d.key}": a default key code is required`);
  if (d.shortcut && RESERVED.has(d.shortcut)) throw new Error(`QoL setting "${d.key}": ${d.shortcut} is the game's own key`);
  settings.set(d.key, d);
  render(); // a panel already on screen picks the new row up
  return {
    key: d.key,
    get: () => get(d.key),
    set: (v) => set(d.key, v),
    on: (fn) => on(d.key, fn),
  };
}

function def(key) {
  const d = settings.get(key);
  if (!d) throw new Error(`Unknown QoL setting "${key}" (register it first)`);
  return d;
}

/** The current value of `key`: what the player chose, or the default. */
export function get(key) {
  const d = def(key);
  if (d.type === "button") return undefined;
  return valid(d, values[key]) ? values[key] : d.default;
}

/**
 * Change `key`, save it, and tell everyone listening. For a button, `set`
 * presses it (the value is ignored).
 */
export function set(key, value) {
  const d = def(key);
  if (d.type === "button") {
    d.action?.();
    notify(key, undefined);
    return;
  }
  if (!valid(d, value)) throw new Error(`QoL setting "${key}" cannot be ${JSON.stringify(value)}`);
  if (get(key) === value) return;
  if (value === d.default) delete values[key]; // only store what differs: new defaults reach old players
  else values[key] = value;
  persist();
  notify(key, value);
}

/**
 * Call `fn(value)` whenever `key` changes, and once now with its current
 * value, so a feature needs no separate "initial state" code path. (Buttons:
 * called on each press only.) Returns a function that unsubscribes.
 */
export function on(key, fn) {
  const d = def(key);
  if (!listeners.has(key)) listeners.set(key, new Set());
  listeners.get(key).add(fn);
  if (d.type !== "button") fn(get(key));
  return () => listeners.get(key).delete(fn);
}

function notify(key, value) {
  for (const fn of listeners.get(key) || []) fn(value);
  sync.get(key)?.(value);
}

// ——— The panel ———
// Rendered from the registry, so a feature never touches index.html.

let mounted = null; // { root, menuOpen }
const sync = new Map(); // key → refresh the control after a change from elsewhere (a shortcut)
let capturing = null; // the "key" setting waiting for the player to press a key

/**
 * Show the registry inside `root` (the pause menu's Quality of Life panel).
 * `menuOpen()` tells the shortcut dispatcher whether the menu is up.
 */
export function mount(root, { menuOpen = () => false } = {}) {
  mounted = { root, menuOpen };
  render();
}

const el = (tag, props = {}, ...children) => {
  const e = Object.assign(document.createElement(tag), props);
  e.append(...children);
  return e;
};
// "KeyF" → "F", "Digit2" → "2", "Backquote" → "`": what is printed on the key.
const NAMES = {
  Backquote: "`", Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]", Semicolon: ";",
  Quote: "'", Comma: ",", Period: ".", Slash: "/", Backslash: "\\",
  ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
};
export const keyName = (code) => NAMES[code] || code.replace(/^Key|^Digit|^Numpad/, "");

function render() {
  if (!mounted) return;
  const { root } = mounted;
  sync.clear();
  const byGroup = new Map();
  for (const d of settings.values()) {
    if (!byGroup.has(d.group)) byGroup.set(d.group, []);
    byGroup.get(d.group).push(d);
  }
  const rank = (g) => (GROUPS.includes(g) ? GROUPS.indexOf(g) : GROUPS.length);
  const groups = [...byGroup.keys()].sort((a, b) => rank(a) - rank(b)); // stable: unknown groups keep their order
  const out = [];
  if (!groups.length) out.push(el("p", { className: "menu__note", textContent: "Nothing here yet." }));
  for (const g of groups) {
    const rows = byGroup.get(g).slice().sort((a, b) => a.order - b.order);
    out.push(el("h3", { className: "qol__group", textContent: g }), ...rows.map(row));
  }
  root.replaceChildren(...out);
}

function row(d) {
  const id = `qol-${d.key}`;
  const name = el("label", { className: "qol__label", htmlFor: id, textContent: d.label });
  if (d.shortcut) name.append(" ", el("kbd", { textContent: keyName(d.shortcut) }));
  const text = el("div", { className: "qol__text" }, name);
  if (d.notOriginal) text.append(el("span", { className: "qol__tag", textContent: "Not in the 1997 game" }));
  if (d.help) text.append(el("small", { className: "qol__help", textContent: d.help }));
  return el("div", { className: "qol__row" }, text, control(d, id));
}

function control(d, id) {
  if (d.type === "toggle") {
    // A real checkbox (keyboard and screen readers for free), drawn as a switch.
    const c = el("input", { type: "checkbox", id, className: "qol__switch", checked: get(d.key) });
    c.setAttribute("role", "switch");
    c.onchange = () => set(d.key, c.checked);
    sync.set(d.key, (v) => (c.checked = v));
    return c;
  }
  if (d.type === "choice") {
    const s = el("select", { id });
    d.options.forEach((o, i) => s.append(new Option(o.label, i)));
    const pick = () => (s.value = d.options.findIndex((o) => o.value === get(d.key)));
    pick();
    // <select> values are strings: map back through the index so numbers stay numbers.
    s.onchange = () => set(d.key, d.options[+s.value].value);
    sync.set(d.key, pick);
    return s;
  }
  if (d.type === "button") {
    return el("button", { id, type: "button", className: "qol__button", textContent: d.buttonLabel || d.label, onclick: () => set(d.key) });
  }
  if (d.type === "key") {
    const b = el("button", { id, type: "button", className: "qol__button qol__key" });
    const show = (msg) => b.replaceChildren(msg ? msg : el("kbd", { textContent: keyName(get(d.key)) }));
    show();
    b.onclick = () => {
      capturing = { d, show };
      show("Press a key…");
    };
    sync.set(d.key, () => show());
    return b;
  }
  throw new Error(`QoL setting "${d.key}": unknown type "${d.type}"`);
}

// ——— Shortcuts, dispatched centrally ———

const typing = (t) =>
  t instanceof HTMLElement &&
  (t.isContentEditable || t.tagName === "TEXTAREA" || t.tagName === "SELECT" ||
    (t.tagName === "INPUT" && !["checkbox", "radio", "range", "button", "submit"].includes(t.type)));

function press(d, e) {
  if (d.type === "toggle") set(d.key, !get(d.key));
  else if (d.type === "choice") {
    const i = d.options.findIndex((o) => o.value === get(d.key));
    set(d.key, d.options[(i + 1) % d.options.length].value);
  } else if (d.type === "button") set(d.key);
  else d.action?.(e); // "key": the binding itself is the value
}

// Registered when this module is first imported, before menu.js adds its Esc
// listener: a handled key is stopped here, so it neither opens the menu nor
// reaches movie.js (where any keydown ends the music).
addEventListener("keydown", (e) => {
  if (capturing) {
    // Rebinding: the next key is the answer. Esc cancels (and keeps the menu open).
    e.preventDefault();
    e.stopImmediatePropagation();
    const { d, show } = capturing;
    if (e.code === "Escape") return (capturing = null), show();
    const taken = [...settings.values()].find((o) => o !== d && (o.shortcut === e.code || (o.type === "key" && get(o.key) === e.code)));
    if (RESERVED.has(e.code) || taken) return show(`${keyName(e.code)} is taken${taken ? ` (${taken.label})` : ""}`);
    capturing = null;
    set(d.key, e.code);
    show();
    return;
  }
  if (e.repeat || e.ctrlKey || e.altKey || e.metaKey || typing(e.target)) return;
  const menuOpen = mounted?.menuOpen() ?? false;
  for (const d of settings.values()) {
    const code = d.type === "key" ? get(d.key) : d.shortcut;
    if (code !== e.code || (menuOpen && !d.inMenu) || (d.active && !d.active())) continue;
    e.preventDefault();
    e.stopImmediatePropagation();
    press(d, e);
    return;
  }
}, true);
