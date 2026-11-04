// "Mendel's mind": a live panel of what he senses, feels and decides (#48).
// Not in the 1997 game, which never let you see any of this.
//
// What you are looking at, top to bottom (the making-of explains each part:
// making-of/#mind-panel):
//
//   SENSES   his two infrared eyes, seen from above: three rays each (high,
//            middle, and the "ledge" ray under the floor), coloured by what
//            they hit; the ray the eye believed is bold. The two touch
//            points at his front corners light up when he bumps into
//            something, or when you right-click him.
//   MUSCLES  the three NERM controllers (posture, speed, turn). Each is a
//            10-state Markov chain (the dots: the lit one is where the chain
//            ended this tick). Its output is the muscle's *target* (the
//            hollow marker); the bar is the muscle after slew limiting,
//            chasing it smoothly.
//   LEARNING which of the 30 genomes is on trial, how far its trial is, its
//            running cost (lower is fitter), and when the genetic algorithm
//            next breeds. The whole panel flashes when it does.
//   COST     a sparkline of the running costs over the last two minutes.
//   STATE    mode, affliction and its countdown, pain, deaths, his floor.
//   MOOD     one plain-language line read off all of the above. It is *our*
//            interpretation, and labelled so: the game has no mood variable.
//
// Cost when closed: none. The setting's listener stops our animation
// frame loop, so nothing reads the engine or draws. Open, it reads one
// 109-float snapshot per frame (crates/engine/src/creature/mind.rs; the
// pointer never moves, so no allocation) and only redraws when it changed.

import { register } from "../qol.js";

const HZ = 24; // simulation ticks per second
const W = 320; // CSS pixels: the canvas is drawn at devicePixelRatio

// ——— The snapshot's layout (keep in step with creature/mind.rs) ———
const M = {
  version: 0, mode: 1, modeTicks: 2, ticksAlive: 3, heading: 4, speed: 5, turn: 6, step: 7, turnRate: 8,
  gait: 9, stance: 10, stanceHeight: 11, grounded: 12, groundObject: 13, groundPolygon: 14, fallTicks: 15,
  savePad: 16, walked: 17, eyeAngle: 18, eyeRange: 19,
  eyes: 20, // 2 × EYE floats
  bump: 50, clickSide: 52, clickTicks: 53, touches: 54, pains: 55, deaths: 56, hazardDelay: 57,
  affliction: 58, afflictionLeft: 59, afflictionDuration: 60, generationsTotal: 61,
  controllers: 62, // 3 × CTRL floats
  clicks: 107, lastClickSide: 108,
};
const EYE = 15; // value, source, winner, then 3 rays × 4 (dist, class, seen, value)
const CTRL = 15; // state, output, muscle, velocity, genome, progress, cost, last, best, worst,
//                  trialTicks, generationTicks, trials, generations, rate
const VERSION = 2;

const MODES = ["absent", "appearing", "alive", "vanishing"];
const SOURCES = ["sight", "bump", "your touch", "blind", "noise", "falling"];
const AFFLICTIONS = [
  "", "shield", "quick muscles", "speed", "camouflage", "blind", "confused",
  "agitated", "agitated", "agitated", "floaty gravity", "no gravity", "reversed gravity",
];
const CONTROLLERS = [
  { name: "Posture", lo: "crouch", hi: "tall", signed: false },
  { name: "Speed", lo: "back", hi: "forward", signed: true },
  { name: "Turn", lo: "right", hi: "left", signed: true },
];

// The site's palette (style.css) plus one colour per kind of hit.
const C = {
  abyss: "#050b0d", deep: "#0b1a1e", bone: "#ece6d8", mute: "#7f8d8a", lime: "#c6ff3d",
  rule: "#ffffff1a", faint: "#ffffff0f", ray: "#ffffff22",
  hit: [null, "#4fd1c5", "#ece6d8", "#ff5a3c", "#ffb340"], // —, floor, wall, hazard, drop
  touch: "#7fd8ff", // your right click
};
const HIT_NAMES = ["", "floor", "wall", "hazard", "drop"];
const MONO = "ui-monospace, 'JetBrains Mono', Menlo, monospace";
const SANS = "ui-sans-serif, Inter, 'Helvetica Neue', Arial, sans-serif";

/**
 * Register the "Show Mendel's mind" setting and build the (hidden) panel.
 * `getEngine()` returns the running engine, or nothing before the CD boots.
 */
export function mendelsMind(getEngine) {
  const setting = register({
    key: "mendelsMind",
    label: "Show Mendel's mind",
    help: "A live panel of his eyes, touch, muscles and learning brain, beside the 3-D view.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group: "Explore",
    order: 10,
    shortcut: "KeyM",
  });

  // ——— DOM: a header, the canvas, the mood line, links ———
  const el = (tag, props = {}, ...kids) => {
    const e = Object.assign(document.createElement(tag), props);
    e.append(...kids);
    return e;
  };
  const canvas = el("canvas", { className: "mind__canvas", role: "img" });
  const mood = el("p", { className: "mind__mood" });
  const close = el("button", { type: "button", className: "mind__close", title: "Close (M)", textContent: "×" });
  close.setAttribute("aria-label", "Close Mendel's mind");
  close.onclick = () => setting.set(false);
  const panel = el(
    "aside",
    { className: "mind", hidden: true },
    el(
      "header",
      { className: "mind__head" },
      el("div", {}, el("p", { className: "mind__kicker", textContent: "Live · not in the 1997 game" }), el("h2", { textContent: "Mendel's mind" })),
      close,
    ),
    canvas,
    el("div", { className: "mind__moodbox" }, el("span", { className: "mind__tag", textContent: "Interpretation" }), mood),
    el(
      "footer",
      { className: "mind__foot" },
      el("a", { href: "./making-of/#mind-panel", target: "_blank", rel: "noopener", textContent: "How to read this panel →" }),
      el("a", { href: "./making-of/#mendel", target: "_blank", rel: "noopener", textContent: "Mendel's mind, the chapter" }),
    ),
  );
  panel.setAttribute("aria-label", "Mendel's mind");
  document.body.append(panel);

  // ——— State kept between frames ———
  let raf = 0;
  let prev = null; // last snapshot drawn
  let history = []; // { t, posture, speed } running costs, 2 per second
  let lastSample = -1;
  let flashes = []; // { kind, k, until, text }
  let ariaAt = 0;
  const seen = { trials: null, generations: null, pains: null, deaths: null, clicks: 0 };

  const ctx = canvas.getContext("2d");
  let H = 0;
  // The canvas is W CSS pixels wide (CSS may scale it down on small
  // screens) and as tall as the last drawing needed.
  function size(h = H || 640) {
    H = h;
    const dpr = Math.max(1, devicePixelRatio || 1);
    canvas.width = Math.round(W * dpr);
    canvas.height = Math.round(H * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  function frame(now) {
    raf = requestAnimationFrame(frame);
    const engine = getEngine?.();
    const m = engine?.creature?.mind?.();
    if (!m || m[M.version] !== VERSION) {
      if (prev !== "empty") drawEmpty(engine ? "Mendel has not been born yet in this world." : "Load your CD: Mendel lives in the worlds.");
      prev = "empty";
      return;
    }
    events(m, now);
    const animating = flashes.some((f) => f.until > now);
    // Redraw only when the snapshot changed (the game ticks at 24 Hz, the
    // screen at 60+) or a flash is fading.
    if (!animating && prev instanceof Float32Array && same(prev, m)) return;
    prev = Float32Array.from(m);
    draw(m, now);
    mood.textContent = interpret(m, now);
    if (now > ariaAt) {
      ariaAt = now + 1000;
      canvas.setAttribute("aria-label", describe(m));
    }
  }
  const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);

  // Edges in the counters become flashes: a new genome on trial, a GA
  // generation, pain, a death, your touch.
  function events(m, now) {
    const ctrl = (k, i) => m[M.controllers + k * CTRL + i];
    const trials = [0, 1, 2].map((k) => ctrl(k, 12));
    const gens = [0, 1, 2].map((k) => ctrl(k, 13));
    if (seen.trials) {
      for (let k = 0; k < 3; k++) {
        if (gens[k] > seen.generations[k]) {
          flashes.push({ kind: "generation", k, until: now + 3500, text: `${CONTROLLERS[k].name}: new generation. The worst 15 genomes are rebuilt from the best 15.` });
          history.push({ mark: "generation", t: m[M.ticksAlive] });
        } else if (trials[k] > seen.trials[k]) {
          flashes.push({ kind: "trial", k, until: now + 1400 });
          history.push({ mark: "trial", k, t: m[M.ticksAlive] });
        }
      }
      if (m[M.pains] > seen.pains) flashes.push({ kind: "pain", until: now + 1500 });
      if (m[M.deaths] > seen.deaths) flashes.push({ kind: "death", until: now + 2500 });
      // A click is felt for only 4 ticks (a sixth of a second), and a slow
      // frame runs up to 5: we could miss it entirely. So the engine counts
      // clicks, and we keep the sensor lit long enough for the eye to see.
      if (m[M.clicks] > seen.clicks) flashes.push({ kind: "click", side: m[M.lastClickSide], until: now + 1200 });
    }
    seen.clicks = m[M.clicks];
    seen.trials = trials;
    seen.generations = gens;
    seen.pains = m[M.pains];
    seen.deaths = m[M.deaths];
    flashes = flashes.filter((f) => f.until > now - 50);
    // Cost history: two samples a second of sim time, two minutes kept.
    const t = m[M.ticksAlive];
    if (t < lastSample) history = []; // a new game, or a load
    if (t - lastSample >= HZ / 2 || lastSample < 0) {
      lastSample = t;
      history.push({ t, posture: ctrl(0, 6), speed: ctrl(1, 6) });
      const from = t - 120 * HZ;
      while (history.length && history[0].t < from) history.shift();
    }
  }

  // ——— Drawing ———
  const text = (s, x, y, { font = `10px ${MONO}`, color = C.mute, align = "left", base = "alphabetic" } = {}) => {
    ctx.font = font;
    ctx.fillStyle = color;
    ctx.textAlign = align;
    ctx.textBaseline = base;
    ctx.fillText(s, x, y);
  };
  const heading = (s, y, right) => {
    text(s.toUpperCase(), 0, y, { font: `600 9px ${SANS}`, color: C.mute });
    if (right) text(right, W, y, { font: `9px ${MONO}`, color: C.mute, align: "right" });
    ctx.fillStyle = C.rule;
    ctx.fillRect(0, y + 5, W, 1);
  };
  const dot = (x, y, r, fill, stroke) => {
    ctx.beginPath();
    ctx.arc(x, y, r, 0, Math.PI * 2);
    if (fill) (ctx.fillStyle = fill), ctx.fill();
    if (stroke) (ctx.strokeStyle = stroke), ctx.stroke();
  };
  const glow = (on) => {
    ctx.shadowColor = on || "transparent";
    ctx.shadowBlur = on ? 10 : 0;
  };
  const fade = (f, now) => Math.max(0, Math.min(1, (f.until - now) / 600));
  const secs = (ticks) => (ticks >= 600 * HZ ? `${Math.round(ticks / HZ / 60)} min` : ticks >= 90 * HZ ? `${(ticks / HZ / 60).toFixed(1)} min` : `${Math.ceil(ticks / HZ)} s`);

  function drawEmpty(msg) {
    size();
    ctx.clearRect(0, 0, W, H);
    text(msg, W / 2, 120, { font: `12px ${SANS}`, color: C.mute, align: "center" });
    mood.textContent = "—";
  }

  function draw(m, now) {
    if (!H) size();
    ctx.clearRect(0, 0, W, H);
    const gen = flashes.find((f) => f.kind === "generation" && f.until > now);
    if (gen) {
      // The whole panel breathes lime when the GA breeds.
      const a = fade(gen, now);
      ctx.fillStyle = `rgba(198,255,61,${0.1 * a})`;
      ctx.fillRect(0, 0, W, H);
    }
    const pain = flashes.find((f) => (f.kind === "pain" || f.kind === "death") && f.until > now);
    if (pain) {
      ctx.fillStyle = `rgba(255,90,60,${0.14 * fade(pain, now)})`;
      ctx.fillRect(0, 0, W, H);
    }
    let y = 10;
    y = senses(m, now, y);
    y = muscles(m, now, y + 16);
    y = learning(m, now, y + 16);
    y = sparkline(m, y + 16);
    y = state(m, now, y + 16);
    const need = Math.ceil(y) + 6;
    if (need !== H) {
      size(need); // grew or shrank (an affliction line came or went): redraw at the new height
      return draw(m, now);
    }
    if (gen) {
      // The banner across the top: the GA just bred.
      const a = fade(gen, now);
      ctx.globalAlpha = a;
      ctx.fillStyle = C.lime;
      ctx.fillRect(0, 0, W, 22);
      text(`GA generation ${m[M.generationsTotal]}`, 8, 15, { font: `700 11px ${SANS}`, color: C.abyss });
      text(gen.text.split(": ")[0], W - 8, 15, { font: `11px ${SANS}`, color: C.abyss, align: "right" });
      ctx.globalAlpha = 1;
    }
  }

  // SENSES: the top-down ray diagram, the eye meters, the touch points.
  function senses(m, now, y0) {
    heading("Senses", y0, `eyes ±${Math.round((m[M.eyeAngle] * 360) / 1024)}° · range ${m[M.eyeRange]}`);
    const top = y0 + 14, h = 196;
    const cx = W / 2, cy = top + h - 36; // his centre
    const px = 150 / 2000; // pixels per world unit (longest range 2,000)
    const head = [cx, cy - 20];
    const fwd = m[M.speed] / 256;

    // Range arcs: posture widens or narrows his gaze and his reach.
    const a = (m[M.eyeAngle] / 1024) * Math.PI * 2;
    const R = m[M.eyeRange] * px;
    ctx.setLineDash([2, 4]);
    ctx.strokeStyle = C.rule;
    for (const s of [1, -1]) {
      ctx.beginPath();
      const th = -Math.PI / 2 - s * a;
      ctx.arc(head[0], head[1], R, th - 0.22, th + 0.22);
      ctx.stroke();
    }
    ctx.setLineDash([]);

    // The rays. Left eye = heading + a (he turns left as heading grows).
    for (let e = 0; e < 2; e++) {
      const o = M.eyes + e * EYE;
      const s = e === 0 ? 1 : -1;
      const dir = [-Math.sin(a) * s, -Math.cos(a)];
      const side = [-dir[1], dir[0]]; // lateral, to spread the three rays apart
      const winner = m[o + 2];
      const source = m[o + 1];
      for (let k = 0; k < 3; k++) {
        const r = o + 3 + k * 4;
        const [dist, cls, isSeen] = [m[r], m[r + 1], m[r + 2]];
        const off = (k - 1) * 3.5;
        const ox = head[0] + side[0] * off, oy = head[1] + side[1] * off;
        const end = (d) => [ox + dir[0] * d * px, oy + dir[1] * d * px];
        // The full ray, faint; dashed for the ledge ray under the floor.
        ctx.setLineDash(k === 2 ? [3, 3] : []);
        ctx.lineWidth = 1;
        ctx.strokeStyle = source === 0 ? C.ray : C.faint;
        const [fx, fy] = end(m[M.eyeRange]);
        ctx.beginPath(), ctx.moveTo(ox, oy), ctx.lineTo(fx, fy), ctx.stroke();
        if (dist >= 0) {
          const [hx, hy] = end(dist);
          const col = isSeen ? C.hit[cls] : C.mute;
          const win = winner === k && source === 0;
          ctx.lineWidth = win ? 2.5 : 1.25;
          ctx.strokeStyle = col;
          ctx.globalAlpha = isSeen ? 1 : 0.5;
          glow(win && col);
          ctx.beginPath(), ctx.moveTo(ox, oy), ctx.lineTo(hx, hy), ctx.stroke();
          glow(false);
          if (isSeen) dot(hx, hy, win ? 3.5 : 2.5, col);
          else {
            // A blocker the eye does not count: it only hides what lies behind.
            ctx.lineWidth = 1;
            ctx.beginPath(), ctx.moveTo(hx - 3, hy - 3), ctx.lineTo(hx + 3, hy + 3), ctx.moveTo(hx + 3, hy - 3), ctx.lineTo(hx - 3, hy + 3), ctx.stroke();
          }
          ctx.globalAlpha = 1;
        }
      }
      ctx.setLineDash([]);
      ctx.lineWidth = 1;
    }

    // Mendel, from above: a shell, a glowing head, six legs that step as
    // he walks (their swing follows the distance he has walked).
    const swing = Math.sin(m[M.walked] / 60) * 4 * Math.min(1, Math.abs(fwd) * 2 + 0.2);
    ctx.save();
    ctx.translate(cx, cy);
    ctx.strokeStyle = C.mute;
    ctx.lineWidth = 1.5;
    for (const sx of [-1, 1]) {
      for (let i = 0; i < 3; i++) {
        const ly = -10 + i * 10, ph = (i % 2 ? 1 : -1) * sx * swing;
        ctx.beginPath(), ctx.moveTo(sx * 10, ly), ctx.lineTo(sx * 22, ly + ph - 4), ctx.stroke();
      }
    }
    ctx.lineWidth = 1;
    ctx.fillStyle = C.deep;
    ctx.strokeStyle = C.bone;
    ctx.beginPath();
    ctx.ellipse(0, 0, 13, 19, 0, 0, Math.PI * 2);
    ctx.fill(), ctx.stroke();
    dot(0, -15, 4, C.lime);
    ctx.restore();

    // His touch points: the hull's front corners (hull points 0 and 2).
    for (let e = 0; e < 2; e++) {
      const s = e === 0 ? -1 : 1;
      const x = cx + s * 14, y = cy - 18;
      const bump = m[M.bump + e];
      const fl = flashes.find((f) => f.kind === "click" && f.side === e + 1 && f.until > now);
      const click = m[M.clickSide] === e + 1 ? m[M.clickTicks] : 0;
      if (fl) {
        const t = 1 - (fl.until - now) / 1200;
        ctx.strokeStyle = C.touch;
        ctx.globalAlpha = 1 - t;
        dot(x, y, 6 + t * 26, null, C.touch);
        ctx.globalAlpha = 1;
      }
      const col = click || fl ? C.touch : bump ? C.lime : null;
      glow(col);
      dot(x, y, 4.5, col || C.abyss, col || C.mute);
      glow(false);
      if (click) {
        // Four ticks of your touch: one pip per tick left.
        for (let i = 0; i < click; i++) dot(x + s * (10 + i * 5), y + 9, 1.6, C.touch);
      }
    }

    // The eye meters at the sides: value 0..256 and where it came from.
    for (let e = 0; e < 2; e++) {
      const o = M.eyes + e * EYE;
      const v = m[o], src = m[o + 1];
      const x = e === 0 ? 0 : W - 10, mh = 120, my = top + 20;
      ctx.fillStyle = C.faint;
      ctx.fillRect(x, my, 10, mh);
      const col = src === 2 ? C.touch : src === 1 ? C.lime : src >= 3 ? C.hit[3] : C.bone;
      ctx.fillStyle = col;
      ctx.fillRect(x, my + mh * (1 - v / 256), 10, (mh * v) / 256);
      const align = e === 0 ? "left" : "right", tx = e === 0 ? 0 : W;
      text(e === 0 ? "LEFT EYE" : "RIGHT EYE", tx, top + 12, { font: `600 9px ${SANS}`, color: C.mute, align });
      text(String(v), tx, my + mh + 14, { font: `600 13px ${MONO}`, color: C.bone, align });
      text(SOURCES[src] || "", tx, my + mh + 26, { font: `9px ${SANS}`, color: src ? col : C.mute, align });
    }

    // Legend.
    let lx = 40;
    const ly = top + h + 4;
    for (const k of [1, 2, 3, 4]) {
      dot(lx, ly - 3, 3, C.hit[k]);
      text(HIT_NAMES[k], lx + 6, ly, { font: `9px ${SANS}` });
      lx += 12 + ctx.measureText(HIT_NAMES[k]).width + 10;
    }
    ctx.strokeStyle = C.mute;
    ctx.setLineDash([3, 3]);
    ctx.beginPath(), ctx.moveTo(lx, ly - 3), ctx.lineTo(lx + 14, ly - 3), ctx.stroke();
    ctx.setLineDash([]);
    text("ledge ray", lx + 18, ly, { font: `9px ${SANS}` });
    return ly;
  }

  // MUSCLES: chain state, target, muscle after slew, gait.
  function muscles(m, now, y0) {
    heading("Muscles", y0, `gait ${m[M.gait]} · stance ${m[M.stanceHeight]}`);
    let y = y0 + 22;
    for (let k = 0; k < 3; k++) {
      const o = M.controllers + k * CTRL;
      const c = CONTROLLERS[k];
      text(c.name, 0, y + 4, { font: `11px ${SANS}`, color: C.bone });
      // The Markov chain: 10 states; odd ones output 256, even ones 0.
      for (let s = 0; s < 10; s++) {
        const on = m[o] === s;
        const x = 58 + s * 6.5;
        dot(x, y, on ? 2.6 : 1.6, on ? C.lime : s % 2 ? "#ffffff44" : "#ffffff1c");
      }
      // The bar: muscle after slew (fill) chasing the chain's output (marker).
      const bx = 134, bw = 150, bh = 6;
      ctx.fillStyle = C.faint;
      ctx.fillRect(bx, y - bh / 2, bw, bh);
      const val = m[o + 2], target = m[o + 1];
      const X = (v) => bx + (v / 256) * bw;
      ctx.fillStyle = C.bone;
      if (c.signed) {
        ctx.fillRect(Math.min(X(128), X(val)), y - bh / 2, Math.abs(X(val) - X(128)), bh);
        ctx.fillStyle = C.mute;
        ctx.fillRect(X(128), y - 6, 1, 12);
      } else ctx.fillRect(bx, y - bh / 2, X(val) - bx, bh);
      ctx.strokeStyle = C.lime;
      ctx.lineWidth = 1.5;
      ctx.beginPath(), ctx.moveTo(X(target), y - 7), ctx.lineTo(X(target) - 3, y - 11), ctx.lineTo(X(target) + 3, y - 11), ctx.closePath(), ctx.stroke();
      ctx.lineWidth = 1;
      text(c.lo, bx, y + 14, { font: `9px ${SANS}` });
      text(c.hi, bx + bw, y + 14, { font: `9px ${SANS}`, align: "right" });
      text(String(val), W, y + 4, { font: `11px ${MONO}`, color: C.bone, align: "right" });
      y += 30;
    }
    // Gait and heading in words a player can check against the 3-D view.
    const deg = Math.round(((m[M.heading] % 1024) * 360) / 1024 + 360) % 360;
    const turn = m[M.turnRate];
    const turning = turn ? `${turn > 0 ? "left" : "right"} ${Math.abs(Math.round((turn * 360 * HZ) / 1024))}°/s` : "straight";
    const ground = m[M.groundObject] ? `on #${m[M.groundObject] - 1}/${m[M.groundPolygon]}` : `airborne ${secs(m[M.fallTicks])}`;
    text(`heading ${deg}° · ${Math.round(Math.abs(m[M.step]) * HZ)} u/s ${m[M.step] < 0 ? "back" : ""} · ${turning} · ${ground}`, 0, y, { font: `9.5px ${MONO}`, color: C.mute });
    return y;
  }

  // LEARNING: the genetic algorithm, one row per controller.
  function learning(m, now, y0) {
    const ctrl = (k, i) => m[M.controllers + k * CTRL + i];
    const breed = Math.min(ctrl(0, 11), ctrl(1, 11), ctrl(2, 11));
    heading("Learning", y0, `${m[M.generationsTotal]} generations · next ≈ ${secs(breed)}`);
    let y = y0 + 22;
    for (let k = 0; k < 3; k++) {
      const fl = flashes.find((f) => f.k === k && (f.kind === "trial" || f.kind === "generation") && f.until > now);
      if (fl) {
        ctx.fillStyle = `rgba(198,255,61,${0.22 * fade(fl, now)})`;
        ctx.fillRect(-4, y - 11, W + 8, 22);
      }
      text(CONTROLLERS[k].name, 0, y + 4, { font: `11px ${SANS}`, color: C.bone });
      // The population: 30 genomes, the one on trial lit.
      const g = ctrl(k, 4);
      for (let i = 0; i < 30; i++) {
        const x = 58 + (i % 15) * 4.6, yy = y - 4 + Math.floor(i / 15) * 6;
        ctx.fillStyle = i === g ? C.lime : i < g ? "#ffffff55" : "#ffffff1c";
        ctx.fillRect(x, yy, 3, 3);
      }
      // This trial's progress, and its running cost.
      const bx = 134, bw = 70;
      ctx.fillStyle = C.faint;
      ctx.fillRect(bx, y - 2, bw, 4);
      ctx.fillStyle = fl ? C.lime : C.bone;
      ctx.fillRect(bx, y - 2, bw * ctrl(k, 5), 4);
      text(`#${g} · ${secs(ctrl(k, 10))}`, bx, y + 13, { font: `9px ${MONO}` });
      text(fmt(ctrl(k, 6)), W, y + 2, { font: `11px ${MONO}`, color: C.bone, align: "right" });
      text(`best ${fmt(ctrl(k, 8))}`, W, y + 13, { font: `9px ${MONO}`, align: "right" });
      y += 30;
    }
    text("Cost: lower is fitter. Each genome drives for 65,536 cost-ticks, then the next.", 0, y - 4, { font: `9px ${SANS}` });
    return y - 4;
  }
  const fmt = (v) => (v >= 1e6 ? `${(v / 1e6).toFixed(1)}M` : v >= 1e4 ? `${Math.round(v / 1000)}k` : String(Math.round(v)));

  // COST: running costs over two minutes; ticks at trials, lime at breeding.
  function sparkline(m, y0) {
    heading("Cost over time", y0, "last 2 min");
    const top = y0 + 12, h = 46;
    const pts = history.filter((p) => p.posture !== undefined);
    const t1 = m[M.ticksAlive], t0 = t1 - 120 * HZ;
    const X = (t) => ((t - t0) / (t1 - t0)) * W;
    let max = 1;
    for (const p of pts) max = Math.max(max, p.posture, p.speed);
    const Y = (v) => top + h - (v / max) * h;
    for (const p of history) {
      if (!p.mark || p.t < t0) continue;
      ctx.fillStyle = p.mark === "generation" ? C.lime : C.rule;
      ctx.fillRect(Math.round(X(p.t)), top, p.mark === "generation" ? 2 : 1, h);
    }
    for (const [key, col] of [["posture", C.hit[1]], ["speed", C.bone]]) {
      ctx.strokeStyle = col;
      ctx.lineWidth = 1.25;
      ctx.beginPath();
      pts.forEach((p, i) => (i ? ctx.lineTo(X(p.t), Y(p[key])) : ctx.moveTo(X(p.t), Y(p[key]))));
      ctx.stroke();
    }
    ctx.lineWidth = 1;
    text("— posture & turn (one shared cost)", 0, top + h + 12, { font: `9px ${SANS}`, color: C.hit[1] });
    text("— speed", W, top + h + 12, { font: `9px ${SANS}`, color: C.bone, align: "right" });
    return top + h + 12;
  }

  // STATE: mode, affliction, hazards, pain.
  function state(m, now, y0) {
    heading("State", y0);
    let y = y0 + 20;
    const mode = MODES[m[M.mode]] || "?";
    text(mode[0].toUpperCase() + mode.slice(1), 0, y, { font: `600 12px ${SANS}`, color: m[M.mode] === 2 ? C.lime : C.bone });
    text(`pain ${m[M.pains]} · deaths ${m[M.deaths]} · touched ${m[M.clicks]} · steps ${m[M.touches]}${m[M.savePad] ? " · on a save pad" : ""}`, W, y, { font: `9.5px ${MONO}`, align: "right" });
    y += 18;
    const kind = m[M.affliction];
    if (kind) {
      const left = m[M.afflictionLeft], total = m[M.afflictionDuration] || 1;
      text(`Affliction: ${AFFLICTIONS[kind]}`, 0, y, { font: `11px ${SANS}`, color: C.hit[4] });
      text(secs(left), W, y, { font: `11px ${MONO}`, color: C.hit[4], align: "right" });
      ctx.fillStyle = C.faint;
      ctx.fillRect(0, y + 5, W, 3);
      ctx.fillStyle = C.hit[4];
      ctx.fillRect(0, y + 5, (W * left) / total, 3);
      y += 18;
    }
    if (m[M.hazardDelay]) {
      text(`Dangerous floor: it fires in ${(m[M.hazardDelay] / HZ).toFixed(1)} s`, 0, y, { font: `11px ${SANS}`, color: C.hit[3] });
      y += 16;
    }
    return y;
  }

  // ——— MOOD: our reading of the numbers, in words ———
  function interpret(m, now) {
    const eye = (e, i) => m[M.eyes + e * EYE + i];
    const L = eye(0, 0), R = eye(1, 0);
    const what = (e) => {
      const w = eye(e, 2);
      return w >= 0 ? HIT_NAMES[eye(e, 3 + w * 4 + 1)] : "something";
    };
    const speed = m[M.speed], turn = m[M.turnRate];
    const mode = m[M.mode];
    if (mode === 0) return "Not here yet: the world is waiting for him to appear.";
    if (mode === 1) return "Rematerialising.";
    if (mode === 3) return "Dissolving: a hazard, a teleporter or a gate takes him.";
    if (flashes.some((f) => f.kind === "generation" && f.until > now)) return "Rethinking: his brain just bred a new generation of habits.";
    const kind = m[M.affliction];
    if (kind >= 7 && kind <= 9) return "Agitated: the floor is hurting him; he needs to get off it.";
    if (m[M.hazardDelay]) return "In danger: standing on a floor that will kill him.";
    if (eye(0, 1) === 5) return "Falling: both eyes scream at full strength.";
    if (kind === 5) return "Blind: no light reaches his eyes, he walks on habit alone.";
    if (kind === 6) return "Confused: his eyes report random flashes.";
    const clicked = m[M.clickSide] || flashes.find((f) => f.kind === "click" && f.until > now)?.side;
    if (clicked) return `Startled: he feels your touch on his ${clicked === 1 ? "left" : "right"}, like a bump.`;
    if (m[M.bump] || m[M.bump + 1]) return `Bumped into something on his ${m[M.bump] && m[M.bump + 1] ? "front" : m[M.bump] ? "left" : "right"}.`;
    const extra = kind === 1 ? " Shielded, too." : kind === 3 ? " Sped up." : "";
    if (L > 160 && R > 160) return "Hemmed in: things are close on both sides." + extra;
    if (Math.abs(L - R) > 48) {
      const near = L > R ? 0 : 1;
      const away = (near === 0 && turn < 0) || (near === 1 && turn > 0);
      const thing = what(near) === "drop" ? "the edge" : what(near) === "hazard" ? "a hazard" : `the ${what(near)}`;
      const side = near === 0 ? "left" : "right";
      if (away) return `Cautious: turning away from ${thing} on his ${side}.` + extra;
      if (turn) return `Curious, or careless: turning towards ${thing} on his ${side}.` + extra;
      return `Watching ${thing} on his ${side}.` + extra;
    }
    if (speed < -40) return "Backing away." + extra;
    if (Math.abs(speed) < 24) return (turn ? "Looking around, almost on the spot." : "Hesitating: standing still.") + extra;
    if (L + R < 40) return (speed > 120 ? "Striding into open space." : "Wandering: nothing in sight.") + extra;
    return "Exploring." + extra;
  }

  function describe(m) {
    const eye = (e) => `${m[M.eyes + e * EYE]} (${SOURCES[m[M.eyes + e * EYE + 1]]})`;
    const c = (k, i) => m[M.controllers + k * CTRL + i];
    return `Mendel's mind. Left eye ${eye(0)}, right eye ${eye(1)}. Muscles: posture ${c(0, 2)}, speed ${c(1, 2)}, turn ${c(2, 2)}. ` +
      `Genomes on trial: ${c(0, 4)}, ${c(1, 4)}, ${c(2, 4)} of 30. ${m[M.generationsTotal]} generations. Mood: ${mood.textContent}`;
  }

  // ——— On and off ———
  setting.on((on) => {
    panel.hidden = !on;
    document.body.classList.toggle("has-mind", on);
    cancelAnimationFrame(raf);
    raf = 0;
    if (!on) return; // closed: no loop, no reads, no drawing
    prev = null;
    seen.trials = null; // don't flash for what happened while closed
    history = [];
    lastSample = -1;
    size();
    raf = requestAnimationFrame(frame);
  });
  addEventListener("resize", () => !panel.hidden && ((prev = null), size()));
}
