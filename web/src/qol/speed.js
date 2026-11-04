// Game speed ×1 / ×2 / ×4 (#44): fast-forward while Mendel paces about.
//
// The simulation is deterministic at 24 ticks per second, so fast-forward
// is not a different game, only more ticks per second of real time. The
// main loop keeps a clock of *game* seconds; this module scales the real
// time fed into it. Everything else follows from the fixed timestep:
//
// - Between-tick blending (#34) still works: the loop's `alpha` is "how far
//   into the next tick", measured in game time, so at ×4 it simply sweeps
//   0→1 four times faster. Nothing to change in the renderer.
// - Catch-up stays capped at 5 ticks per frame, the loop's existing limit,
//   counted in game time. At ×4 a 60 Hz frame needs 1.6 ticks, so the cap
//   only bites below ~19 fps; then the game runs slower than ×4 instead of
//   spending ever longer frames on ever more ticks (the "spiral of death").
// - Pausing still pauses: the pause menu and the time screen zero the
//   clock, whatever the speed. Menus, the time screen and the movies keep
//   their own (real-time) clocks and never see this factor.
// - The music streams in real time, so it is unaffected.
// - The title ("GALAPAGOS", an `Over` world object, see title.js) is a
//   presentation beat: while any such picture is up, the game runs at ×1.
//
// Sounds: at ×4 one frame can carry the events of several ticks. They play
// as they come, at their own pitch (no chipmunk effect), except that an
// identical sound event repeated within one frame plays once: four
// footsteps landing in the same 16 ms would only sum into one loud click.

import { register } from "../qol.js";

// Event kinds that only make a sound (see the loop in main.js).
const SOUNDS = new Set([3, 5, 8, 9, 10]);

export function gameSpeed() {
  const setting = register({
    key: "speed",
    label: "Game speed",
    help: "Fast-forward the world while Mendel paces about. Music and sounds keep their pitch.",
    type: "choice",
    options: [{ value: 1, label: "×1" }, { value: 2, label: "×2" }, { value: 4, label: "×4" }],
    default: 1, // pacing changes default off (#37)
    notOriginal: true,
    group: "Pacing",
    order: 0,
    shortcut: "KeyG",
  });

  // The corner badge, over the 3-D view (top-left: the frame meter has the
  // top-right).
  const badge = Object.assign(document.createElement("output"), { className: "speed-badge", hidden: true });
  badge.setAttribute("aria-label", "Game speed");
  (document.querySelector(".world-frame") || document.body).append(badge);

  let factor = 1;
  setting.on((v) => {
    factor = v;
    badge.textContent = `×${v}`;
    badge.hidden = v === 1;
  });

  return {
    /** How many game seconds pass per real second, this frame. */
    factor(engine) {
      const held = factor > 1 && !!engine.sim.overlay(640, 480);
      badge.classList.toggle("is-held", held);
      return held ? 1 : factor;
    },
    /** One frame's events, with repeated identical sounds dropped when fast. */
    thin(events) {
      if (factor === 1) return events; // ×1: exactly as before
      const seen = new Set();
      return events.filter((ev) => {
        if (!SOUNDS.has(ev.kind)) return true;
        const id = `${ev.kind}:${ev.a}:${ev.b}:${ev.c}`;
        return !seen.has(id) && seen.add(id);
      });
    },
  };
}
