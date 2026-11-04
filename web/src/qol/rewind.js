// Rewind after death (#45): a Quality of Life feature, off by default.
//
// In 1997, when Mendel dies, the world goes back to the last save pad (or
// the level start) and he reappears there, keeping everything his brain
// learnt, including the pain of that death. That stays the default. With
// this setting on, a small prompt also offers "Retry from 10 s ago / 20 s /
// 30 s": the world and Mendel's *body* go back a few seconds, his *mind*
// does not. It is the original's own idea of a death, with a nearer
// snapshot.
//
// How it works:
// * While he is alive and well, we ask the engine for a rewind point every
//   5 s of game time (`engine.rewind.take(slot)`: the world as
//   `Sim::capture` sees it plus his body, a few microseconds and ~15 kB;
//   never his brain). Eight slots, round robin: 40 s of history.
// * Game time is the engine's tick counter, so the pause menu, the time
//   screen and any game speed are accounted for without asking.
// * When he starts dying (`status()` turns "dying": a lethal floor, a bite,
//   a kill portal; not a teleport) the prompt offers, for 10, 20 and 30 s,
//   the newest point at least that old. It stays up while he dissolves and
//   rematerialises, and a few seconds after: ignore it and the original
//   respawn simply happens.
// * Choosing restores that point (`engine.rewind.restore`): the engine
//   puts the world back, moves his body, keeps his brain, its learning
//   accumulators and the death count, cuts the camera and lets him
//   materialise. Nothing is written to any saved game.

import { register } from "../qol.js";

const EVERY = 5; // seconds of game time between rewind points
const SLOTS = 8; // engine slots used, round robin (≤ 16)
const OFFERS = [10, 20, 30]; // seconds ago, as offered
const GRACE = 4; // seconds the prompt stays once he is alive again

export function rewindAfterDeath(engine, parent) {
  const setting = register({
    key: "rewind",
    label: "Rewind after death",
    help: "When Mendel dies, offer to retry from 10, 20 or 30 seconds ago. His brain keeps what it learnt, as with any death.",
    type: "toggle",
    default: false,
    notOriginal: true,
    group: "Pacing",
  });

  const hz = engine.sim.ticksPerSecond();
  const now = () => engine.game.times().totalTicks;

  // ——— The prompt ———
  const el = (tag, props = {}, ...kids) => {
    const e = Object.assign(document.createElement(tag), props);
    e.append(...kids);
    return e;
  };
  const choices = el("div", { className: "rewind__choices" });
  const fuse = el("i", { className: "rewind__fuse" });
  const box = el(
    "div",
    { className: "rewind", hidden: true, role: "group", ariaLabel: "Rewind after death" },
    el("p", { className: "rewind__title" }, "Mendel died. Retry from"),
    choices,
    el("p", { className: "rewind__note" }, "or wait: he respawns as in 1997"),
    fuse,
  );
  parent.append(box);

  // ——— State ———
  let ring = []; // { slot, at } oldest first; `at` in ticks of the current timeline
  let next = 0; // next slot to write
  let section = -1, last = 0, was = "absent";
  let death = null; // { at, aliveAt } while the prompt is up
  let raf = 0;

  const forget = () => {
    ring = [];
    engine.rewind.forget();
  };
  const close = () => {
    death = null;
    box.hidden = true;
  };

  function open(t) {
    const offers = OFFERS.map((s) => [s, [...ring].reverse().find((p) => t - p.at >= s * hz)]).filter(([, p]) => p);
    if (!offers.length) return forget(); // died within 10 s of arriving: nothing to offer, the original respawn
    death = { at: t, aliveAt: null };
    choices.replaceChildren(
      ...offers.map(([s, p]) => {
        const b = el("button", { type: "button", className: "rewind__choice", textContent: `${s} s ago` });
        b.title = `Back ${Math.round((t - p.at) / hz)} s of game time`;
        b.onclick = () => choose(p);
        return b;
      }),
    );
    fuse.style.transform = "scaleX(1)";
    box.hidden = false;
  }

  function choose(p) {
    close();
    if (!engine.rewind.restore(p.slot)) return forget(); // another build of the section: stale points
    // The timeline continues from the chosen point: newer points are gone,
    // older ones keep their distance to it.
    const t = now(), shift = t - p.at;
    ring = ring.filter((q) => q.at <= p.at).map((q) => ({ ...q, at: q.at + shift }));
  }

  function tick() {
    raf = requestAnimationFrame(tick);
    const t = now(), s = engine.section();
    if (s !== section || t < last) (section = s), forget(), close(); // a new section, game or load
    last = t;
    const status = engine.rewind.status(), edge = status !== was;
    was = status;
    if (death) {
      if (status === "absent" || status === "leaving") return close();
      if (status === "alive") {
        death.aliveAt ??= t;
        const left = 1 - (t - death.aliveAt) / (GRACE * hz);
        fuse.style.transform = `scaleX(${Math.max(0, left)})`;
        // Ignored: the original respawn stands, and the old points belong
        // to a world he has left (the save pad's snapshot is back).
        if (left <= 0) close(), forget();
      }
      return; // no new points while the prompt is up
    }
    if (status === "dying" && edge) return open(t);
    if (status !== "alive" || (ring.length && t - ring[ring.length - 1].at < EVERY * hz)) return;
    if (!engine.rewind.take(next)) return;
    ring = ring.filter((p) => p.slot !== next).concat({ slot: next, at: t });
    next = (next + 1) % SLOTS;
  }

  setting.on((on) => {
    cancelAnimationFrame(raf);
    close();
    forget();
    if (on) raf = requestAnimationFrame(tick);
  });
}
