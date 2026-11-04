// Photo mode (#46): stop the world, walk out of the camera, take a picture.
//
// The 1997 game had no such thing. Here, P (or Pause ▸ Quality of Life ▸
// Photo mode) freezes the simulation without opening the pause menu, hands
// the view to a free camera that starts exactly where the game's camera was,
// hides every overlay, and Enter saves a PNG of the 3-D view.
//
// Three small design decisions, worth knowing before changing this file:
//
// 1. The free camera lives in world-view.js (`freeCamera`, `fly`) and sits
//    *on top of* the orbit and follow cameras rather than replacing them. It
//    never writes to their state, so leaving photo mode restores the previous
//    view by doing nothing at all.
// 2. Taking the picture: the WebGL canvas is created without
//    `preserveDrawingBuffer` (keeping it would cost a buffer copy on every
//    frame, for a feature used once in a while), so after a frame has been
//    shown its pixels are gone. `view.snapshot()` draws one more frame and
//    copies it into a 2-D canvas in the same task, before the browser
//    composites and clears it. The GALAPAGOS title, a separate 2-D canvas
//    over the world, can be painted on top of that copy.
// 3. Keys: Escape belongs to the game (it opens the pause menu), and qol.js
//    refuses to bind it. So this module adds one capture listener of its own,
//    at import time: that is before menu.js adds its Esc listener and
//    main.js its Space listener, so while photo mode is on, it sees every
//    key first and stops it. Esc leaves photo mode instead of opening the
//    menu, and no key reaches movie.js, where any keydown ends the music.
//    (P itself still goes through qol.js's dispatcher, which runs earlier.)

import { register } from "../qol.js";

const FLY = { KeyW: [1, 0, 0], KeyS: [-1, 0, 0], KeyD: [0, 1, 0], KeyA: [0, -1, 0], KeyE: [0, 0, 1], KeyQ: [0, 0, -1] };

/**
 * Registers the setting now (so the row is in the menu from the start);
 * the returned `attach` is called once the world is running.
 */
export function photoMode() {
  let game = null; // { view, menu, pause(v), paused(), section(), world, title }
  let on = false;
  let wasPaused = false;
  let keep = false; // "Include title"
  const held = new Set();
  let raf = 0;

  const setting = register({
    key: "photo",
    label: "Photo mode",
    help: "Freeze the world, fly a free camera and save a picture. P or Esc to leave.",
    type: "button",
    buttonLabel: "Enter",
    notOriginal: true,
    group: "Explore",
    shortcut: "KeyP",
    action: () => (on ? leave() : enter()),
  });

  // ——— The photo HUD: one line of help and two buttons ———
  const el = (tag, props = {}, ...kids) => {
    const e = Object.assign(document.createElement(tag), props);
    e.append(...kids);
    return e;
  };
  const hud = el("div", { className: "photo-hud", hidden: true });
  hud.setAttribute("role", "toolbar");
  hud.setAttribute("aria-label", "Photo mode");
  const titleBox = el("input", { type: "checkbox", onchange: () => setKeep(titleBox.checked) });
  const titleRow = el("label", { className: "photo-hud__title" }, titleBox, " Include title");
  const shot = el("button", { type: "button", className: "photo-hud__shoot", onclick: () => takePhoto() }, "Take photo ", el("kbd", { textContent: "Enter" }));
  const exit = el("button", { type: "button", className: "photo-hud__leave", onclick: () => leave() }, "Leave ", el("kbd", { textContent: "P" }));
  const note = el("output", { className: "photo-hud__note" });
  hud.append(
    el("strong", { textContent: "Photo mode" }),
    el("span", { className: "photo-hud__help", textContent: "WASD · Q/E to fly (Shift: faster) · drag to look · shift-drag to slide · wheel to dolly" }),
    titleRow, shot, exit, note,
  );
  document.body.append(hud);
  const flash = el("div", { className: "photo-flash", hidden: true });
  document.body.append(flash);

  function setKeep(v) {
    keep = v;
    document.body.classList.toggle("is-photo-title", v);
  }

  function enter() {
    if (on || !game) return;
    // The time screen pauses the game too: no photos of a world behind it.
    if (game.menu.open) document.querySelector('#menu [data-action="resume"]').click();
    else if (game.paused()) return;
    wasPaused = game.paused();
    game.pause(true);
    game.view.freeCamera(true);
    on = true;
    document.body.classList.add("is-photo");
    setKeep(keep);
    hud.hidden = false;
    note.value = "";
    last = performance.now();
    raf = requestAnimationFrame(move);
  }

  function leave() {
    if (!on) return;
    on = false;
    cancelAnimationFrame(raf);
    held.clear();
    game.view.freeCamera(false);
    game.pause(wasPaused);
    document.body.classList.remove("is-photo", "is-photo-title");
    hud.hidden = true;
  }

  // WASD/QE, every display frame, in world units per second. Speeds scale
  // with the section (its half-diagonal), within limits that keep Mendel's
  // close-ups controllable.
  let last = 0;
  function move(now) {
    raf = requestAnimationFrame(move);
    const dt = Math.min(0.1, (now - last) / 1000);
    last = now;
    // The title can only be included while it is actually on screen.
    titleRow.hidden = game.title.hidden;
    if (!held.size) return;
    const speed = Math.max(600, Math.min(4000, game.view.size * 0.1)) * (held.has("Shift") ? 4 : 1) * dt;
    const d = [0, 0, 0];
    for (const k of held) FLY[k]?.forEach((x, i) => (d[i] += x));
    game.view.fly(d[0] * speed, d[1] * speed, d[2] * speed);
  }

  /** The picture: the 3-D view, plus the title if asked. Resolves to a PNG Blob. */
  function capture() {
    const out = game.view.snapshot();
    const t = game.title;
    if (keep && !t.hidden && t.width) {
      // Where the title sits over the world, in the world canvas's pixels.
      const w = game.world.getBoundingClientRect(), r = t.getBoundingClientRect();
      const s = out.width / w.width;
      const ctx = out.getContext("2d");
      ctx.imageSmoothingEnabled = false; // the 1997 blit: hard pixels
      ctx.drawImage(t, (r.left - w.left) * s, (r.top - w.top) * s, r.width * s, r.height * s);
    }
    return new Promise((resolve, reject) => out.toBlob((b) => (b ? resolve(b) : reject(new Error("toBlob failed"))), "image/png"));
  }

  // galapagos-world0-20261104-153012.png: sortable, and says where it was.
  function fileName() {
    const d = new Date(), p = (n) => String(n).padStart(2, "0");
    const stamp = `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`;
    return `galapagos-world${game.section()}-${stamp}.png`;
  }

  async function takePhoto() {
    if (!on) return;
    try {
      const blob = await capture();
      const name = fileName();
      const url = URL.createObjectURL(blob);
      Object.assign(document.createElement("a"), { href: url, download: name }).click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      note.value = `Saved ${name}`;
      flash.hidden = true;
      void flash.offsetWidth; // restart the CSS animation, even mid-flash
      flash.hidden = false;
      flash.onanimationend = () => (flash.hidden = true);
    } catch (err) {
      note.value = `Could not take the photo: ${err.message}`;
    }
  }

  // See note 3 above: registered now, ahead of menu.js and main.js.
  addEventListener("keydown", (e) => {
    if (!on || e.ctrlKey || e.altKey || e.metaKey) return; // browser shortcuts stay the browser's
    if (e.target instanceof HTMLInputElement && e.target.type === "text") return;
    e.stopImmediatePropagation();
    if (e.key === "Shift") held.add("Shift");
    if (FLY[e.code]) held.add(e.code);
    if (e.code === "Escape" || e.code === "Enter" || e.code === "NumpadEnter" || FLY[e.code] || e.code === "Space") e.preventDefault();
    if (e.repeat) return;
    if (e.code === "Escape") leave();
    else if (e.code === "Enter" || e.code === "NumpadEnter") takePhoto();
  }, true);
  addEventListener("keyup", (e) => {
    if (!on) return;
    e.stopImmediatePropagation(); // e.g. main.js's Space release
    held.delete(e.key === "Shift" ? "Shift" : e.code);
  }, true);
  addEventListener("blur", () => held.clear()); // keys released elsewhere never come back up

  return {
    setting,
    /** Called once the world runs: what photo mode needs from main.js. */
    attach(g) { game = g; },
    get on() { return on; },
    capture: () => (on ? capture() : Promise.reject(new Error("not in photo mode"))),
  };
}
