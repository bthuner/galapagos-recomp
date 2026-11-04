// Frame-time meter: the first Quality of Life setting, and the template for
// the others (#37). It is deliberately harmless, so it can prove the
// registry end to end: a toggle in Pause ▸ Quality of Life, a shortcut,
// persistence across reloads, and a feature that only costs anything when on.
//
// Why frame *time* and not just fps: a steady 60 fps average can hide one
// 100 ms hitch per second, which the player feels as a stutter. So we show
// the average frame time and the worst frame of the last half second.

import { register } from "../qol.js";

export function frameMeter() {
  const setting = register({
    key: "frameMeter",
    label: "Show frame-time meter",
    help: "Frames per second and milliseconds per frame, in the top-right corner.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group: "Comfort",
    order: 100, // a diagnostic: after the real comforts
    shortcut: "Backquote",
  });

  const box = Object.assign(document.createElement("output"), { className: "frame-meter", hidden: true });
  document.body.append(box);

  // Our own requestAnimationFrame loop measures what the browser delivers,
  // without touching the game loop; it only runs while the meter is shown.
  let raf = 0, last = 0, frames = 0, sum = 0, worst = 0;
  const tick = (now) => {
    const dt = now - last;
    last = now;
    frames++, (sum += dt), (worst = Math.max(worst, dt));
    if (sum >= 500) {
      const avg = sum / frames;
      box.textContent = `${Math.round(1000 / avg)} fps · ${avg.toFixed(1)} ms · max ${worst.toFixed(0)} ms`;
      frames = sum = worst = 0;
    }
    raf = requestAnimationFrame(tick);
  };

  setting.on((on) => {
    box.hidden = !on;
    cancelAnimationFrame(raf);
    if (!on) return;
    box.textContent = "…";
    frames = sum = worst = 0;
    raf = requestAnimationFrame((now) => ((last = now), (raf = requestAnimationFrame(tick))));
  });
}
