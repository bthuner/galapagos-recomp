// Fullscreen and a clean, game-like page (#38).
//
// Two settings in Pause ▸ Quality of Life ▸ Comfort:
//
//  • Fullscreen (a button, also in a corner of the world frame, shortcut F).
//    The Fullscreen API is pointed at `.world-frame`, not the whole page: the
//    world canvas, the title overlay and anything docked inside the frame come
//    along, and the developer controls around it do not. The pause menu, the
//    time screen and the other overlays are children of <body>, so they would
//    stay behind the fullscreen layer; while fullscreen we move them into the
//    frame, and put them back, in order, when it ends. They are position:
//    fixed, so they look the same in either parent.
//
//  • Developer mode (off). The World 0–7 buttons, "Wake every machine",
//    "Follow Mendel", the soundtrack buttons and the status log are tools for
//    building the recomp, not part of the game. With developer mode off they
//    are hidden (CSS: `body:not(.dev-mode) .dev-only`), never removed: the
//    follow camera keeps running and dragging still orbits. The few status
//    messages a player needs ("Saved …", "Opened …", "A new Mendel is
//    born.") then appear in a small transient toast instead of the log.
//
// Esc: browsers keep Esc for leaving fullscreen. Firefox and Safari use the
// first Esc to leave fullscreen and never tell the page, so the second one
// opens the pause menu. Chromium lets a fullscreen page ask for the key
// (Keyboard Lock API): there Esc opens the pause menu as usual, and holding
// Esc leaves fullscreen, as the browser's own banner says.

import { register } from "../qol.js";

export function fullscreen() {
  const frame = document.querySelector(".world-frame");
  const supported = !!frame?.requestFullscreen && document.fullscreenEnabled;

  // ——— Developer mode ———
  const dev = register({
    key: "devMode",
    label: "Developer mode",
    help: "World buttons, debug switches, soundtrack and the status log, under the game.",
    type: "toggle",
    default: false,
    // The controls it shows were never in the 1997 game; with it off, the
    // page is closer to the original, which is why off is the default.
    notOriginal: true,
    group: "Comfort",
    order: 90,
  });
  dev.on((on) => document.body.classList.toggle("dev-mode", on));

  // The status log is written by main.js and menu.js; we only listen. The
  // feedback for every click and touch is a developer's concern, so it stays
  // in the log; anything else is worth telling the player.
  const toast = Object.assign(document.createElement("output"), { className: "toast", hidden: true });
  toast.setAttribute("role", "status");
  document.body.append(toast);
  const NOISE = /^(Clicked object|Nothing clickable|You touched|Right-click Mendel)/;
  let hide = 0;
  const log = document.getElementById("log");
  if (log) {
    new MutationObserver(() => {
      const msg = log.textContent.split("\n").pop().trim();
      if (dev.get() || !msg || NOISE.test(msg)) return;
      toast.textContent = msg;
      toast.hidden = false;
      toast.classList.remove("is-out");
      clearTimeout(hide);
      hide = setTimeout(() => {
        toast.classList.add("is-out");
        hide = setTimeout(() => (toast.hidden = true), 400);
      }, 3200);
    }).observe(log, { childList: true, characterData: true, subtree: true });
  }

  if (!supported) return;

  // ——— Fullscreen ———
  const toggle = () => {
    if (document.fullscreenElement) document.exitFullscreen();
    else frame.requestFullscreen({ navigationUI: "hide" }).catch(() => {}); // refused without a click or key press
  };
  register({
    key: "fullscreen",
    label: "Fullscreen",
    help: "The world fills the screen, letterboxed, at the View Size chosen in Preferences (Full = edge to edge). F again to leave.",
    type: "button",
    buttonLabel: "Enter / leave",
    notOriginal: true, // the 1997 game ran in a window or switched display mode
    group: "Comfort",
    order: 0,
    shortcut: "KeyF",
    inMenu: true, // so F also leaves fullscreen from the pause menu
    action: toggle,
  });

  const button = Object.assign(document.createElement("button"), {
    type: "button",
    className: "fullscreen-button",
    title: "Fullscreen (F)",
    onclick: (e) => (e.currentTarget.blur(), toggle()), // blur: Space must stay the game's
  });
  button.setAttribute("aria-label", "Fullscreen");
  frame.append(button);

  // Overlays living in <body> follow the frame into fullscreen, including
  // ones added while it is fullscreen (a toast, a panel…).
  const STAY = new Set(["boot", "stage"]);
  const moved = [];
  const adopt = (el) => {
    if (el.nodeType !== 1 || STAY.has(el.id) || el.tagName === "SCRIPT" || el.parentNode !== document.body) return;
    moved.push(el);
    frame.append(el);
  };
  const late = new MutationObserver((records) => records.forEach((r) => r.addedNodes.forEach(adopt)));

  document.addEventListener("fullscreenchange", () => {
    const on = document.fullscreenElement === frame;
    button.classList.toggle("is-on", on);
    button.setAttribute("aria-label", on ? "Leave fullscreen" : "Fullscreen");
    button.title = on ? "Leave fullscreen (F)" : "Fullscreen (F)";
    if (on) {
      [...document.body.children].forEach(adopt);
      late.observe(document.body, { childList: true });
      navigator.keyboard?.lock?.(["Escape"]).catch(() => {}); // Chromium: Esc stays the game's
    } else {
      late.disconnect();
      // Same order as before, minus anything its owner removed meanwhile
      // (the extras' movie overlay, a closed panel…): no ghosts come back.
      document.body.append(...moved.splice(0).filter((el) => el.parentNode === frame));
      navigator.keyboard?.unlock?.();
    }
  });
}
