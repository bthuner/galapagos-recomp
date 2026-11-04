// Playing a Smacker movie: the audio clock is the master.
//
// We decode the whole soundtrack first and start it as one AudioBuffer.
// Then, every animation frame, we ask "which video frame belongs to
// audio time t?" and decode forward until we reach it. If the tab stutters,
// frames are skipped instead of the sound drifting out of sync.

export function toAudioBuffer(ctx, samples, rate, channels) {
  const frames = samples.length / channels;
  const buf = ctx.createBuffer(channels, Math.max(1, frames), rate);
  for (let c = 0; c < channels; c++) {
    const out = buf.getChannelData(c);
    for (let i = 0; i < frames; i++) out[i] = samples[i * channels + c] / 32768;
  }
  return buf;
}

// How a frame reaches the canvas. Smacker's "Y-interlace" flag (the intro
// uses it) stores 240 lines meant for a 480-line screen. There are two
// honest ways to show that today:
//
//   smooth     the canvas holds the 240 stored lines and CSS stretches them
//              to 4:3; bilinear filtering blends neighbouring lines, roughly
//              what a blurry 1997 CRT did. The default.
//   scanlines  the exact original output: each stored line on every other
//              row of a 480-line canvas, black rows in between.
//
// Either way the browser scales smoothly. `image-rendering: pixelated` at a
// non-integer factor would double some rows and not others (moiré).
function presenter(canvas, movie, scanlines) {
  const g = canvas.getContext("2d");
  const { width: w, height: h } = movie;
  let mode = null;
  let image = null;

  return function present(pixels) {
    const want = movie.interlaced && scanlines() ? "scanlines" : "smooth";
    if (want !== mode) {
      mode = want;
      const rows = mode === "scanlines" ? h * 2 : h;
      [canvas.width, canvas.height] = [w, rows];
      canvas.style.aspectRatio = `${w} / ${movie.displayHeight}`;
      image = new ImageData(w, rows); // starts transparent black
      if (mode === "scanlines") {
        for (let i = 3; i < image.data.length; i += 4) image.data[i] = 255; // opaque black gaps
      }
    }
    if (mode === "smooth") {
      image.data.set(pixels);
    } else {
      const line = w * 4;
      for (let y = 0; y < h; y++) {
        image.data.set(pixels.subarray(y * line, (y + 1) * line), 2 * y * line);
      }
    }
    g.putImageData(image, 0, 0);
  };
}

export function playMovie(engine, bytes, canvas, audioCtx, { scanlines = () => false, destination = audioCtx.destination } = {}) {
  const movie = engine.openMovie(bytes);
  // Audio-only files (the soundtrack) carry a 4x4 placeholder picture: skip it.
  const present = movie.width > 4 ? presenter(canvas, movie, scanlines) : null;

  let source = null;
  if (movie.sampleRate) {
    source = audioCtx.createBufferSource();
    source.buffer = toAudioBuffer(audioCtx, movie.decodeAudio(), movie.sampleRate, movie.channels);
    source.connect(destination);
  }

  return new Promise((resolve) => {
    const secondsPerFrame = movie.usPerFrame / 1e6;
    const t0 = audioCtx.currentTime;
    let shown = -1;
    let done = false;

    const finish = () => {
      if (done) return;
      done = true;
      source?.stop();
      movie.close();
      removeEventListener("keydown", finish);
      canvas.removeEventListener("click", finish);
      resolve();
    };
    addEventListener("keydown", finish);
    canvas.addEventListener("click", finish);
    source?.start(t0);

    (function tick() {
      if (done) return;
      const want = Math.floor((audioCtx.currentTime - t0) / secondsPerFrame);
      let pixels = null;
      while (shown < want) {
        pixels = movie.nextFrame();
        if (!pixels) return finish();
        shown++;
      }
      if (pixels && present) present(pixels);
      requestAnimationFrame(tick);
    })();
  });
}
