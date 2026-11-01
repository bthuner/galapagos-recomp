// Sound effects: a small WebAudio mixer for the engine's sound events.
//
// The engine decides *what* plays (it picks one of an emitter's
// alternatives with the game's own random generator) and *who* plays it. This
// file decides *how* it sounds: each world sound comes from an emitter with a
// near and a far distance, a volume, a pitch and a random pitch variation, so
// we give it a PannerNode at the object's position. The listener is the
// camera. The original mixed in DirectSound with "active-panning 3-D stereo";
// WebAudio's panner is the modern equivalent.
//
// Samples are decoded lazily, once: the first event that needs sound #n
// copies its PCM out of WebAssembly memory into an AudioBuffer.

export function createSfx(engine, ctx) {
  const master = ctx.createGain();
  master.gain.value = 0.8;
  master.connect(ctx.destination);

  const buffers = new Map(); // "bank:index" → AudioBuffer (or null if empty)
  const loops = new Map(); // object id → playing looped source

  function buffer(bank, i) {
    const key = `${bank}:${i}`;
    if (!buffers.has(key)) {
      const pcm = engine.sound(bank, i);
      let buf = null;
      if (pcm) {
        buf = ctx.createBuffer(1, pcm.samples.length, pcm.rate);
        const out = buf.getChannelData(0);
        for (let k = 0; k < out.length; k++) out[k] = pcm.samples[k] / 32768;
      }
      buffers.set(key, buf);
    }
    return buffers.get(key);
  }

  // A panner with the emitter's distances. "linear" falls from full volume
  // at `near` to silence at `far`, the simplest model that matches the
  // two numbers the game stores (their exact curve is unknown).
  function panner(position, near, far) {
    const p = ctx.createPanner();
    p.panningModel = "equalpower";
    p.distanceModel = "linear";
    p.refDistance = Math.max(1, near);
    p.maxDistance = Math.max(p.refDistance + 1, far);
    p.rolloffFactor = 1;
    p.positionX.value = position[0];
    p.positionY.value = position[1];
    p.positionZ.value = position[2];
    return p;
  }

  function play(bank, i, { position, near = 5000, far = 20000, volume = 256, pitch = 65536, variation = 0, loop = false } = {}) {
    const buf = buffer(bank, i);
    if (!buf) return null;
    const src = ctx.createBufferSource();
    src.buffer = buf;
    src.loop = loop;
    // Pitch and variation are 16.16 fixed point: 65536 = ×1.0.
    const jitter = 1 + (Math.random() * 2 - 1) * (variation / 65536);
    src.playbackRate.value = (pitch / 65536) * jitter;
    const gain = ctx.createGain();
    gain.gain.value = volume / 256;
    src.connect(gain);
    // A far distance of 300000 means "heard everywhere": skip the panner.
    if (position && far < 300000) gain.connect(panner(position, near, far)).connect(master);
    else gain.connect(master);
    src.start();
    played++;
    return src;
  }

  let played = 0;

  return {
    // How many sounds have started (for the curious, from DevTools).
    get played() {
      return played;
    },
    // A world emitter fired (engine event kind 3).
    emitter(sound, objectId, emitterIndex) {
      const e = engine.emitter(objectId, emitterIndex);
      const position = engine.sim.object(objectId).position;
      // inferred: an object plays one looped sound at a time; a new sound
      // from the same object replaces its loop.
      if (loops.has(objectId)) {
        loops.get(objectId).stop();
        loops.delete(objectId);
      }
      const src = play(0, sound, { position, ...e });
      if (src && e.loop) loops.set(objectId, src);
    },
    // One of Mendel's cues (engine event kind 5), heard from where he is,
    // with the cue distances from Creature_ReadSoundCues (15000 / 16000).
    creature(sound) {
      const { position } = engine.creature.state();
      play(1, sound, { position, near: 15000, far: 16000 });
    },
    // The camera is the listener.
    listen(eye, forward, up) {
      const l = ctx.listener;
      if (!l.positionX) return; // very old browsers
      l.positionX.value = eye[0];
      l.positionY.value = eye[1];
      l.positionZ.value = eye[2];
      l.forwardX.value = forward[0];
      l.forwardY.value = forward[1];
      l.forwardZ.value = forward[2];
      l.upX.value = up[0];
      l.upY.value = up[1];
      l.upZ.value = up[2];
    },
    // A new section: its bank replaces the old one, loops stop.
    reset() {
      for (const src of loops.values()) src.stop();
      loops.clear();
      for (const key of [...buffers.keys()]) if (key.startsWith("0:")) buffers.delete(key);
    },
  };
}
