// The picture over the 3-D view: world 0's huge chrome "GALAPAGOS".
//
// In 1997 this was not part of the intro movie, nor a 3-D object, but a
// world object of class `Over` (section 0, #59): 40 ticks after the world
// appears it puts the archive's title strip on the screen with a white
// flash, 150 ticks later it takes it down (crates/engine/src/overlay.rs).
// The renderer kept it in one DirectDraw surface with its top-left pixel as
// the colour key and blitted it over every frame, centred on the screen
// (0x436270). We do the same with a 2-D canvas over the WebGL one: the
// palette flash tints the world, not this canvas, as in the footage.

import { loadPrefs } from "./menu.js";

/** Keeps `overlay` (a canvas over `world`) in step with the engine. */
export function startTitleOverlay(engine, world, overlay) {
  const ctx = overlay.getContext("2d");
  let drawn = -1; // picture index currently in the canvas

  function paint(i, height) {
    const pic = engine.picture(i);
    if (!pic) return false;
    // The colour key: every pixel equal to the top-left one is see-through.
    const d = pic.data;
    const [r, g, b] = d;
    for (let p = 0; p < d.length; p += 4) {
      if (d[p] === r && d[p + 1] === g && d[p + 2] === b) d[p + 3] = 0;
    }
    // (A time-screen picture would lose its glyph strip: `height`.)
    [overlay.width, overlay.height] = [pic.width, height];
    ctx.putImageData(pic, 0, 0);
    drawn = i;
    return true;
  }

  function frame() {
    requestAnimationFrame(frame);
    // Cheap test first: is anything up at all?
    if (world.hidden || !engine.sim.overlay(640, 480)) {
      overlay.hidden = true;
      return;
    }
    // The original picks the strip by the view's size in pixels (0x41a2d0:
    // 504×30 from 560×400, 256×15 from 288×200, 128×8 from 144×100) and
    // centres it on the screen. Our "screen" is the world canvas: 480 rows
    // tall (240 at 320×240), shrunk by the View Size preference.
    const viewSize = parseFloat(getComputedStyle(world).getPropertyValue("--view-size")) || 1;
    const sh = (loadPrefs().resolution === 0 ? 240 : 480) * viewSize;
    const sw = (sh * 4) / 3;
    const o = engine.sim.overlay(Math.round(sw), Math.round(sh));
    if (!o || (o.picture !== drawn && !paint(o.picture, o.height))) {
      overlay.hidden = true;
      return;
    }
    const s = world.clientHeight / sh; // CSS pixels per original pixel
    const x = (o.centred ? Math.trunc((sw - o.width) / 2) : 0) + o.x;
    const y = (o.centred ? Math.trunc((sh - o.height) / 2) : 0) + o.y;
    const left = world.offsetLeft + (world.clientWidth - sw * s) / 2 + x * s;
    Object.assign(overlay.style, {
      left: `${left}px`,
      top: `${world.offsetTop + y * s}px`,
      width: `${o.width * s}px`,
      height: `${o.height * s}px`,
    });
    overlay.hidden = false;
  }
  requestAnimationFrame(frame);
}
