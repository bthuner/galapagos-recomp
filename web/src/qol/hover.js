// Highlight clickable objects under the cursor (Quality of Life #41).
//
// Half of Galapagos is guessing what is interactive: the 1997 game gave no
// hint. With this on, whatever a left click would set in motion glows
// softly and the cursor becomes a pointer. Off by default (#37: it is a
// hint, so it changes how the game plays).
//
// It must never lie, so the question goes to the engine, which answers it
// with the very pick a click uses and the object's own state machine
// (crates/engine/src/sim/hover.rs):
//
//   sim_hover(ray)  → the object id a click would reach, if a transition of
//                     its current state would fire because of that click
//                     (Mendel aboard the lift, the right timer window…);
//                     -1 otherwise. Read-only: asking never clicks.
//
// Cost: one pick walks the section's polygons, about 0.05 ms in world 0.
// We ask at most once per animation frame, only while the setting is on
// and the mouse is over the view, never while dragging and never for touch
// (a finger does not hover). We ask even when the mouse rests, because the
// answer changes on its own: Mendel steps onto a lift, the camera turns.

import { register } from "../qol.js";

/**
 * `game()` returns `{ engine, view }` once a world is on screen (main.js's
 * `globalThis.galapagos`), or nothing before.
 */
export function hover(game = () => globalThis.galapagos, canvas = document.getElementById("world")) {
  const setting = register({
    key: "hoverHighlight",
    label: "Highlight clickable objects",
    help: "Whatever a click would set in motion glows under the cursor.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group: "Comfort",
  });

  let pointer = null; // last mouse position over the view, or null
  let raf = 0;

  // Set every frame (two integer writes): a new section resets the view's
  // highlight, and this puts it back if the cursor still rests on something.
  const show = (id) => {
    const g = game();
    const draw = id > 0 && g ? g.engine.wasm.sim_object_draw(id) : -1;
    g?.view.setHighlight(draw >= 0 ? draw : null);
    canvas.style.cursor = draw >= 0 ? "pointer" : "";
  };

  // One pick per animation frame, at most.
  const tick = () => {
    raf = requestAnimationFrame(tick);
    const g = game();
    // No cursor over the view: leave() already cleared ours, and the
    // highlight may now belong to keyboard play (#49).
    if (pointer && g) show(hoverAt(g.engine, g.view, pointer.x, pointer.y));
  };

  const move = (e) => {
    // Touch has no hover; a held button is a drag (orbit, look-around).
    if (e.pointerType === "touch" || e.buttons) return void leave();
    pointer = { x: e.clientX, y: e.clientY };
  };
  const leave = () => {
    pointer = null;
    show(-1);
  };

  setting.on((on) => {
    cancelAnimationFrame(raf);
    canvas.removeEventListener("pointermove", move);
    canvas.removeEventListener("pointerleave", leave);
    canvas.removeEventListener("pointerdown", leave);
    leave();
    if (!on) return;
    canvas.addEventListener("pointermove", move);
    canvas.addEventListener("pointerleave", leave);
    canvas.addEventListener("pointerdown", leave);
    raf = requestAnimationFrame(tick);
  });
}

/**
 * The object (1-based id) a left click at page point (x, y) would reach and
 * set in motion, or -1. Exactly the ray `onPick` builds for a real click.
 */
export function hoverAt(engine, view, x, y) {
  if (!view.listener()) return -1; // no frame drawn yet: no camera to aim from
  const [o, d] = view.rayAt(x, y);
  if (!d.every(Number.isFinite)) return -1;
  return engine.wasm.sim_hover(o[0], o[1], o[2], d[0], d[1], d[2]);
}

/**
 * Every awake object a click would currently matter for, for keyboard-only
 * play (#49): `[{ id, draw, position }]`, in registry order. It ignores
 * line of sight: keep those `view.project(position)` puts on screen, and
 * confirm one with `hoverAt` on its projected point (that also tells you
 * nothing is in front of it). One pass over the registry, so cheap enough
 * to call each time the player presses "next".
 */
export function clickableObjects(engine) {
  const n = engine.wasm.sim_clickable();
  const ids = [...new Uint32Array(engine.wasm.memory.buffer, engine.wasm.sim_clickable_ptr(), n)];
  return ids.map((id) => ({ id, draw: engine.wasm.sim_object_draw(id), position: engine.sim.object(id).position }));
}
