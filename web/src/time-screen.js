// The time screen shown when Mendel passes a dimensional gate.
//
// Everything on it comes from the game's own picture: chrome Mendel, the
// LEVEL and TOTAL labels, and, in its last rows, a glyph strip
// "0123456789:" that the original copies character by character to write
// the times (0x41a2d0 / 0x41a090 / 0x41aca0; docs/re/saves.md). Glyph pixels
// equal to the picture's top-left pixel are transparent. We do exactly that
// into an ImageData, then let CSS scale the result.

function formatTime(seconds) {
  const two = (n) => String(n).padStart(2, "0");
  // "%.2ld:%.2ld:%.2ld", hours mod 100 like the original's two digits.
  return `${two(Math.floor(seconds / 3600) % 100)}:${two(Math.floor(seconds / 60) % 60)}:${two(seconds % 60)}`;
}

function render(engine, level, times) {
  // Ask for the largest set (a 640×480 view): the 560×421 pictures.
  const layout = engine.game.timeScreen(level, 640, 480);
  const pic = layout && engine.picture(layout.picture);
  if (!pic) return null;
  const { width: w, glyphHeight: gh, visibleHeight: vh } = layout;
  const src = pic.data;
  const out = new ImageData(w, vh);
  out.data.set(src.subarray(0, w * vh * 4));
  const key = src.subarray(0, 4); // the transparent colour
  const stripY = pic.height - gh;

  // Copy one glyph from the strip to (x, layout.y); returns its width.
  const glyph = (ch, x) => {
    const g = layout.glyphs[ch];
    for (let y = 0; y < gh; y++) {
      for (let dx = 0; dx < g.w; dx++) {
        const s = ((stripY + y) * w + g.x + dx) * 4;
        if (src[s] === key[0] && src[s + 1] === key[1] && src[s + 2] === key[2]) continue;
        const d = ((layout.y + y) * w + x + dx) * 4;
        if (x + dx >= w || layout.y + y >= vh) continue;
        out.data.set(src.subarray(s, s + 4), d);
      }
    }
    return g.w;
  };
  const width = (text) => [...text].reduce((n, ch) => n + layout.glyphs[ch].w, 0);

  // Level time from the left, total time ending at the right.
  let x = layout.levelX;
  for (const ch of formatTime(times.levelSeconds)) x += glyph(ch, x);
  const total = formatTime(times.totalSeconds);
  x = layout.totalRight - width(total);
  for (const ch of total) x += glyph(ch, x);
  return out;
}

/** Shows the screen until Escape, Space or a click; resolves then. */
export function showTimeScreen(engine, level, times) {
  const image = render(engine, level, times);
  if (!image) return Promise.resolve();
  const root = document.getElementById("time-screen");
  const canvas = root.querySelector("canvas");
  [canvas.width, canvas.height] = [image.width, image.height];
  canvas.getContext("2d").putImageData(image, 0, 0);
  root.hidden = false;
  return new Promise((resolve) => {
    const done = (e) => {
      if (e.type === "keydown" && e.key !== "Escape" && e.code !== "Space") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      removeEventListener("keydown", done, true);
      root.removeEventListener("pointerdown", done);
      root.hidden = true;
      resolve();
    };
    addEventListener("keydown", done, true);
    root.addEventListener("pointerdown", done);
  });
}
