// Remember the CD, skip straight to play (#40): not in the 1997 game.
//
// In 1997 every launch went CD-ROM spin-up → Anark logo movie → world. A
// browser adds one more step in front: "Press to begin", because sound may
// only start after the player has clicked something. On the tenth visit
// that is two waits before Mendel, for nothing new.
//
// With "Skip intro on later visits" on, a visit that finds the CD already
// cached in OPFS gets one start screen with one real choice instead:
//
//      [ Continue ]   World 0 · 3 min ago     (only if an autosave exists,
//      [ New game ]                            see autosave.js)
//                          Replay intro
//
// Whichever is clicked is also the gesture that unlocks sound, so there is
// still exactly one click, and it now means something. The movie is not
// played; the world comes straight up.
//
// What stays as it was:
//   - the toggle is off by default (#37: it changes the game's pacing);
//   - the visit that reads the ISO is never a "later visit": the intro
//     plays, exactly as in 1997. So does any visit before the intro has
//     been seen once in this browser (`galapagos.introSeen`);
//   - "Replay intro" brings it back: on the start screen it is a third
//     choice (same click, movie first, then the world); in Pause ▸ Quality
//     of Life it plays the movie over the paused game, Esc or a click ends
//     it, and the menu comes back where it was.
//
// The hooks in main.js are two calls: `offer(fromCache)` when the stage
// appears, and `run(play)` where the intro used to be played.

import { register } from "../qol.js";

const SEEN = "galapagos.introSeen";

export function skipIntro() {
  const enabled = register({
    key: "skipIntro",
    label: "Skip intro on later visits",
    help: "Once your CD is remembered, the start screen offers Continue or New game and goes straight to the world. The intro still plays the first time.",
    type: "toggle",
    default: false, // pacing: off unless the player asks (#37)
    notOriginal: true,
    group: "Comfort",
    order: 20,
  });
  register({
    key: "replayIntro",
    label: "Replay intro",
    help: "The Anark logo movie, over the paused game. Esc or a click ends it.",
    type: "button",
    buttonLabel: "Replay",
    notOriginal: true,
    group: "Comfort",
    order: 21,
    action: () => replayInGame(),
  });

  const $ = (id) => document.getElementById(id);
  let skipping = false; // this visit's start screen is the streamlined one
  let replay = false; // the player chose "Replay intro" on it
  let play = null; // main.js's way to play the intro, once the engine is up
  let replaying = false;

  // The stage just appeared, before the engine has loaded. `fromCache`:
  // the CD came from OPFS, not from an ISO dropped on this visit.
  function offer(fromCache) {
    skipping = fromCache && enabled.get() && localStorage.getItem(SEEN) === "1";
    if (!skipping) return; // "Press to begin", then the movie: as before
    const start = $("start");
    start.textContent = "New game";
    const again = Object.assign(document.createElement("button"), {
      type: "button",
      className: "link start__replay",
      textContent: "Replay intro",
    });
    // A real click, so it unlocks sound like the others. main.js listens to
    // "start" only, so pass the click on (autosave.js drops Continue on it).
    again.onclick = () => {
      replay = true;
      start.click();
    };
    start.after(again);
    start.addEventListener("click", () => again.remove(), { once: true });
  }

  // Where main.js used to play the intro unconditionally.
  async function run(playIntro) {
    play = playIntro;
    if (skipping && !replay) return;
    localStorage.setItem(SEEN, "1");
    await play();
  }

  // From the pause menu: the menu stays logically open (so the game stays
  // paused) but steps aside while the movie plays where it played at boot.
  async function replayInGame() {
    if (!play || replaying) return;
    // Any keydown ends the movie in progress (movie.js): that stops the
    // music, so it does not play over the intro's own soundtrack. (Sent
    // before `replaying` is set, or the listener below would swallow it.)
    dispatchEvent(new KeyboardEvent("keydown"));
    replaying = true;
    const menu = $("menu");
    const box = $("screen").parentElement;
    const world = $("world").parentElement;
    menu.hidden = world.hidden = true;
    box.hidden = false;
    try {
      await play();
    } finally {
      box.hidden = true;
      menu.hidden = world.hidden = false;
      replaying = false;
    }
  }

  // During a replay, keys end it and do nothing else: otherwise Esc would
  // also close the (hidden) menu and unpause the game behind the movie.
  // Registered now, at startup, so it runs before menu.js's Esc listener.
  addEventListener("keydown", (e) => {
    if (!replaying) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    if (!e.repeat) $("screen").click(); // movie.js also ends on a click
  }, true);

  return { offer, run };
}
