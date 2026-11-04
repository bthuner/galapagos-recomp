// Touch controls (#43): Galapagos on a phone or a tablet.
//
// The 1997 game was played with a mouse: a left click triggers an object, a
// right click touches Mendel, a drag looks around, and two keys do the rest
// (Esc pauses, Space held skips). A touch screen has none of those, so this
// module translates fingers into the same calls the mouse makes:
//
//   tap                      → left click   (view.input.click(x, y, 0))
//   long press (450 ms)      → right click  (view.input.click(x, y, 2)), a tick of vibration
//   one-finger drag          → mouse drag   (view.input.orbit)
//   two-finger drag          → shift-drag   (view.input.pan)
//   pinch                    → mouse wheel  (view.input.zoom)
//
// plus two on-screen buttons in the corners of the view: ☰ (Esc) and Skip
// (Space, for as long as the finger stays on it). Both send the very key
// events the keyboard would, so the menu and the skip logic don't know the
// difference.
//
// How the fingers are stolen from the mouse code: world-view.js listens for
// pointer events on the canvas. We listen on its parent, in the capture
// phase, which always runs first; a touch pointer we handle stops there.
// Mouse and pen events go through untouched. With the setting Off, touch
// pointers reach world-view.js too, which treats one finger as a mouse.

import { register } from "../qol.js";

const LONG_PRESS = 450; // ms: long enough not to fire on a slow tap
const SLOP = 10; // px a finger may wander and still be a tap or a press
const PAN_START = 24; // px of two-finger travel before a pan leaves follow mode

export function touchControls(view) {
  const setting = register({
    key: "touch",
    label: "Touch controls",
    help: "Tap to click, long-press Mendel to touch him, drag to look, two fingers to pan, pinch to zoom. Adds Menu and Skip buttons. Auto turns on at the first touch.",
    type: "choice",
    options: [{ value: "auto", label: "Auto" }, { value: "on", label: "On" }, { value: "off", label: "Off" }],
    default: "auto",
    notOriginal: true,
    group: "Comfort",
    order: 50,
  });

  const canvas = document.getElementById("world");
  const frame = canvas.parentElement;
  const coarse = matchMedia("(pointer: coarse)");
  let mode = "auto", touched = false, active = false;
  const update = () => {
    active = mode === "on" || (mode === "auto" && (touched || coarse.matches));
    // One class on <html> switches the layout (style.css, "Touch controls").
    document.documentElement.classList.toggle("touch-ui", active);
  };
  coarse.addEventListener?.("change", update);
  setting.on((v) => ((mode = v), update()));

  // ——— Gestures ———
  const fingers = new Map(); // pointerId → { x, y }
  let press = null; // the single-finger gesture: { x0, y0, timer, moved, done }
  let pair = null; // the two-finger gesture: { mid, spread, start, panning }
  const two = () => {
    const [a, b] = [...fingers.values()];
    return { mid: [(a.x + b.x) / 2, (a.y + b.y) / 2], spread: Math.hypot(a.x - b.x, a.y - b.y) };
  };
  const cancelPress = () => {
    if (press) clearTimeout(press.timer);
    press = null;
  };
  const steal = (e) => {
    e.stopPropagation();
    e.preventDefault();
  };

  frame.addEventListener("pointerdown", (e) => {
    if (e.target !== canvas || e.pointerType !== "touch") return;
    if (!touched) (touched = true), update(); // Auto: the first touch decides
    if (!active) return;
    steal(e);
    try { canvas.setPointerCapture(e.pointerId); } catch { /* synthetic pointers */ }
    fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    view.input.hold(true);
    if (fingers.size === 1) {
      const p = (press = { x0: e.clientX, y0: e.clientY, moved: false, done: false });
      p.timer = setTimeout(() => {
        // Long press: the right click, i.e. touch Mendel where he is pressed.
        p.done = true;
        navigator.vibrate?.(15);
        view.input.click(p.x0, p.y0, 2);
      }, LONG_PRESS);
    } else {
      // A second finger: no tap, no long press, from now on until all lift.
      if (press) clearTimeout(press.timer), (press.done = true);
      if (fingers.size === 2) {
        const t = two();
        pair = { ...t, start: t.mid, panning: !view.following };
      }
    }
  }, true);

  frame.addEventListener("pointermove", (e) => {
    const f = fingers.get(e.pointerId);
    if (!f) return;
    steal(e);
    const dx = e.clientX - f.x, dy = e.clientY - f.y;
    [f.x, f.y] = [e.clientX, e.clientY];
    if (fingers.size === 1) {
      if (press && !press.moved && Math.hypot(e.clientX - press.x0, e.clientY - press.y0) > SLOP) {
        press.moved = true;
        clearTimeout(press.timer);
      }
      if (!press || press.moved) view.input.orbit(dx, dy);
    } else if (fingers.size === 2 && pair) {
      const now = two();
      if (now.spread > 0 && pair.spread > 0) view.input.zoom(pair.spread / now.spread);
      // A pinch always drifts a little (and the two fingers report their
      // moves one after the other, so the midpoint zig-zags): in follow
      // mode, only a midpoint that really travelled starts a pan, which
      // leaves follow mode as shift-drag does.
      const far = Math.hypot(now.mid[0] - pair.start[0], now.mid[1] - pair.start[1]) > PAN_START;
      if (far) pair.panning = true;
      if (pair.panning) view.input.pan(now.mid[0] - pair.mid[0], now.mid[1] - pair.mid[1]);
      Object.assign(pair, now);
    }
  }, true);

  const lift = (e) => {
    if (!fingers.has(e.pointerId)) return;
    steal(e);
    fingers.delete(e.pointerId);
    if (e.type === "pointerup" && press && !press.done && !press.moved && fingers.size === 0) {
      view.input.click(e.clientX, e.clientY, 0); // a tap: the left click
    }
    if (fingers.size < 2) pair = null;
    if (fingers.size === 1 && press) press.moved = true; // the finger left behind just looks
    if (fingers.size === 0) {
      cancelPress();
      view.input.hold(false);
    }
  };
  frame.addEventListener("pointerup", lift, true);
  frame.addEventListener("pointercancel", lift, true);

  // ——— On-screen buttons: the keys a phone doesn't have ———
  const key = (type, code, k) => dispatchEvent(new KeyboardEvent(type, { code, key: k, bubbles: true }));
  const button = (cls, label, text) => {
    const b = Object.assign(document.createElement("button"), { type: "button", className: `touch-button ${cls}`, textContent: text });
    b.setAttribute("aria-label", label);
    frame.append(b);
    return b;
  };
  const menu = button("touch-button--menu", "Pause menu (Esc)", "☰");
  menu.onclick = () => (key("keydown", "Escape", "Escape"), key("keyup", "Escape", "Escape"));

  // Skip is held, like Space: down on press, up on release (or if the
  // finger slides off and the browser cancels).
  const skip = button("touch-button--skip", "Skip (hold, like Space)", "Skip");
  let skipping = false;
  const hold = (on) => (e) => {
    e.preventDefault();
    if (on === skipping) return;
    skipping = on;
    skip.classList.toggle("is-on", on);
    if (on) try { skip.setPointerCapture(e.pointerId); } catch { /* synthetic pointers */ }
    key(on ? "keydown" : "keyup", "Space", " ");
  };
  skip.addEventListener("pointerdown", hold(true));
  skip.addEventListener("pointerup", hold(false));
  skip.addEventListener("pointercancel", hold(false));
  skip.addEventListener("contextmenu", (e) => e.preventDefault());

  // Holding the phone upright leaves a small 16:9 strip: suggest turning it.
  const hint = Object.assign(document.createElement("button"), {
    type: "button",
    className: "touch-rotate",
    textContent: "Turn your device sideways for a bigger view",
  });
  hint.onclick = () => hint.remove();
  frame.append(hint);
}
