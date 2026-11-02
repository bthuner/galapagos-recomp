// The JavaScript side of the WebAssembly boundary.
//
// The Rust engine exports plain C functions. To hand it a file we:
//   1. ask it to allocate `len` bytes inside its linear memory,
//   2. copy the file there,
//   3. pass the (pointer, length) pair to an exported function.
// No bindings generator, no magic: you can read all of it.
//
// One trap: when WebAssembly memory grows, every existing view on
// `memory.buffer` is detached. So we create views *after* each call that
// might allocate, never cache them.

export async function loadEngine(url = "./pkg/galapagos_engine.wasm") {
  const { instance } = await WebAssembly.instantiateStreaming(fetch(url), {});
  const wasm = instance.exports;
  const u8 = (ptr, len) => new Uint8Array(wasm.memory.buffer, ptr, len);

  function upload(bytes) {
    const ptr = wasm.alloc(bytes.length);
    u8(ptr, bytes.length).set(bytes);
    return [ptr, bytes.length];
  }

  return {
    wasm,
    worldsSectionCount(bytes) {
      const [ptr, len] = upload(bytes);
      const n = wasm.worlds_section_count(ptr, len);
      wasm.dealloc(ptr, len);
      return n;
    },

    // The worlds archive. The engine adopts the bytes; sections are built
    // into render buffers on demand.
    openWorlds(bytes) {
      const n = wasm.worlds_open(...upload(bytes));
      if (n < 0) throw new Error("Not a Galapagos worlds archive");
      return {
        sectionCount: n,
        // Build section i into render buffers (and start its simulation).
        build(i) {
          if (wasm.world_build(i) < 0) throw new Error(`Section ${i} failed to parse`);
          return this.current();
        },
        // The buffers of the section already built (e.g. after a load).
        current() {
          const verts = wasm.world_current_vertex_count();
          if (verts < 0) throw new Error("No section built");
          const textureCount = wasm.world_texture_count();
          const mem = wasm.memory.buffer;
          return {
            vertices: new Float32Array(mem, wasm.world_vertices_ptr(), verts * 7).slice(),
            textureCount,
            textures: new Uint8Array(mem, wasm.world_textures_ptr(), textureCount * 128 * 128 * 4).slice(),
            bounds: [...new Float32Array(mem, wasm.world_bounds_ptr(), 6)],
            objects: wasm.world_object_count(),
            // Per-object draw ranges: [first0, count0, first1, count1, ...].
            draws: new Uint32Array(mem, wasm.world_draws_ptr(), wasm.world_draw_count() * 2).slice(),
            // Transforms change as the simulation runs, so this is a getter
            // returning a fresh view each frame (memory may have grown).
            transforms: () =>
              new Float32Array(wasm.memory.buffer, wasm.world_transforms_ptr(), wasm.world_draw_count() * 16),
            liveVertices: () => new Float32Array(wasm.memory.buffer, wasm.world_vertices_ptr(), verts * 7),
            farClip: wasm.world_far_clip(),
          };
        },
      };
    },

    // The world simulation (see crates/engine/src/sim.rs).
    sim: {
      ticksPerSecond: () => wasm.sim_ticks_per_second(),
      // Runs n ticks; returns host events as {kind, a, b, c}.
      tick(n) {
        const count = wasm.sim_tick(n);
        const raw = new Int32Array(wasm.memory.buffer, wasm.sim_events_ptr(), count * 4);
        const out = [];
        for (let i = 0; i < count; i++) {
          out.push({ kind: raw[i * 4], a: raw[i * 4 + 1], b: raw[i * 4 + 2], c: raw[i * 4 + 3] });
        }
        return out;
      },
      // [first, end) vertex range patched since the last call, or null.
      takeDirty() {
        if (wasm.sim_take_dirty() < 0) return null;
        return [...new Uint32Array(wasm.memory.buffer, wasm.sim_dirty_ptr(), 2)];
      },
      wakeAll: (on) => wasm.sim_set_wake_all(on ? 1 : 0),
      // The original's Space key: while held, every 'STel' object counts as
      // clicked (section 0 uses one to skip the introduction).
      holdSkip: (on) => wasm.sim_hold_skip(on ? 1 : 0),
      click: (o, d) => wasm.sim_click(o[0], o[1], o[2], d[0], d[1], d[2]),
      // { position: [x, y, z], state } of object `id` (1-based).
      object(id) {
        const [x, y, z, state] = new Int32Array(wasm.memory.buffer, wasm.sim_object_info(id), 4);
        return { position: [x, y, z], state };
      },
    },

    // Mendel (see crates/engine/src/creature.rs). His posed mesh is rebuilt
    // in world space every tick, in the world's 7-float vertex layout.
    creature: {
      // A fresh view each call: memory may have grown, and the vertex
      // count changes (0 while he is absent or blinking).
      vertices: () => new Float32Array(wasm.memory.buffer, wasm.creature_vertices_ptr(), wasm.creature_vertex_count() * 7),
      state() {
        const s = new Float32Array(wasm.memory.buffer, wasm.creature_state(), 16);
        return {
          mode: ["absent", "appearing", "alive", "vanishing"][s[0]],
          position: [s[1], s[2], s[3]],
          heading: s[4], // 1024ths of a turn; forward = (cos, 0, sin)
          ground: s[5] ? { object: s[5], polygon: s[6] } : null,
          eyes: { left: s[7], right: s[8] },
          actuators: { posture: s[9], speed: s[10], turn: s[11] },
          walked: s[12], touches: s[13], deaths: s[14], generations: s[15],
        };
      },
      // Chase-camera collision: clear fraction of the segment from → eye.
      clearance: (f, e) => wasm.creature_camera_clearance(f[0], f[1], f[2], e[0], e[1], e[2]),
      // Right click: a ray against his mesh. Returns "left", "right" or null.
      touch: (o, d) => [null, "left", "right"][wasm.creature_touch_ray(o[0], o[1], o[2], d[0], d[1], d[2])],
    },

    // ——— Menus ———
    // File ▸ New with the Preferences age (0..255); then build section 0.
    newGame: (age) => wasm.game_new(age),
    section: () => wasm.world_section(),
    // File ▸ Export / Import Mendel NERM: the 28,242-byte .mnd format.
    exportBrain() {
      const n = wasm.brain_export();
      return new Uint8Array(wasm.memory.buffer, wasm.brain_export_ptr(), n).slice();
    },
    importBrain(bytes) {
      const ptr = wasm.alloc(bytes.length);
      new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
      return wasm.brain_import(ptr, bytes.length) === 0;
    },
    // UI picture i of the archive as ImageData (time screens, title strips).
    picture(i) {
      const [w, h, ptr] = new Uint32Array(wasm.memory.buffer, wasm.picture_info(i), 3);
      if (!w) return null;
      return new ImageData(new Uint8ClampedArray(wasm.memory.buffer, ptr, w * h * 4).slice(), w, h);
    },
    pictureCount: () => wasm.picture_count(),
    // Preferences ▸ Fidelity: 1 = 8-bit 11 kHz … 4 = CD quality.
    setSoundQuality: (q) => wasm.sound_set_quality(q),
    // Saved games (provided by the save-pad work; absent in older builds).
    saveGame() {
      if (!wasm.game_save) return null;
      const n = wasm.game_save();
      return new Uint8Array(wasm.memory.buffer, wasm.game_save_ptr(), n).slice();
    },
    loadGame(bytes) {
      if (!wasm.game_load) return false;
      const ptr = wasm.alloc(bytes.length);
      new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
      return wasm.game_load(ptr, bytes.length) === 0;
    },
    canSave: () => !!wasm.game_save,

    // Sound `i` of a bank (0 = current world, 1 = Mendel, 2 = predators):
    // { rate, baseNote, samples: Int16Array } copied out of WASM memory, or
    // null for an empty slot.
    sound(bank, i) {
      const [rate, len, baseNote, ptr] = new Uint32Array(wasm.memory.buffer, wasm.sound_info(bank, i), 4);
      if (!len) return null;
      return { rate, baseNote, samples: new Int16Array(wasm.memory.buffer, ptr, len).slice() };
    },

    // How emitter n of object id plays (meanings inferred, see objects.md).
    emitter(id, n) {
      const [near, far, volume, loop, pitch, variation] = new Int32Array(wasm.memory.buffer, wasm.world_emitter(id, n), 6);
      return { near, far, volume, loop: loop !== 0, pitch: pitch || 65536, variation };
    },

    // Smacker movies. The engine takes ownership of the uploaded bytes.
    openMovie(bytes) {
      if (wasm.movie_open(...upload(bytes)) !== 0) throw new Error("Not a Smacker file");
      const [width, height, displayHeight, frames, usPerFrame, sampleRate, channels, yMode] =
        new Uint32Array(wasm.memory.buffer, wasm.movie_info(), 8);
      return {
        width, height, displayHeight, frames, usPerFrame, sampleRate, channels,
        interlaced: yMode === 2,
        decodeAudio() {
          const n = wasm.movie_decode_audio();
          if (n < 0) throw new Error("Corrupt movie audio");
          return new Int16Array(wasm.memory.buffer, wasm.movie_audio_ptr(), n).slice();
        },
        nextFrame() {
          const r = wasm.movie_next_frame();
          if (r < 0) throw new Error("Corrupt movie frame");
          return r === 1 ? u8(wasm.movie_frame_ptr(), width * height * 4) : null;
        },
        close: () => wasm.movie_close(),
      };
    },
  };
}
