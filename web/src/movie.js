// Playing a Smacker movie: the audio clock is the master.
//
// We decode the whole soundtrack first and start it as one AudioBuffer.
// Then, every animation frame, we ask "which video frame belongs to
// audio time t?" and decode forward until we reach it. If the tab stutters,
// frames are skipped instead of the sound drifting out of sync.

function toAudioBuffer(ctx, samples, rate, channels) {
  const frames = samples.length / channels;
  const buf = ctx.createBuffer(channels, Math.max(1, frames), rate);
  for (let c = 0; c < channels; c++) {
    const out = buf.getChannelData(c);
    for (let i = 0; i < frames; i++) out[i] = samples[i * channels + c] / 32768;
  }
  return buf;
}

export function playMovie(engine, bytes, canvas, audioCtx) {
  const movie = engine.openMovie(bytes);
  const g = canvas.getContext("2d");
  const image = movie.width > 4 ? new ImageData(movie.width, movie.height) : null;
  if (image) [canvas.width, canvas.height] = [movie.width, movie.height];

  let source = null;
  if (movie.sampleRate) {
    source = audioCtx.createBufferSource();
    source.buffer = toAudioBuffer(audioCtx, movie.decodeAudio(), movie.sampleRate, movie.channels);
    source.connect(audioCtx.destination);
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
      if (pixels && image) {
        image.data.set(pixels);
        g.putImageData(image, 0, 0);
      }
      requestAnimationFrame(tick);
    })();
  });
}
