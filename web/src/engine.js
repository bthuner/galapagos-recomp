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
        build(i) {
          const verts = wasm.world_build(i);
          if (verts < 0) throw new Error(`Section ${i} failed to parse`);
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
      // Runs n ticks; returns host events as {kind, a, b}.
      tick(n) {
        const count = wasm.sim_tick(n);
        const raw = new Int32Array(wasm.memory.buffer, wasm.sim_events_ptr(), count * 3);
        const out = [];
        for (let i = 0; i < count; i++) out.push({ kind: raw[i * 3], a: raw[i * 3 + 1], b: raw[i * 3 + 2] });
        return out;
      },
      // [first, end) vertex range patched since the last call, or null.
      takeDirty() {
        if (wasm.sim_take_dirty() < 0) return null;
        return [...new Uint32Array(wasm.memory.buffer, wasm.sim_dirty_ptr(), 2)];
      },
      wakeAll: (on) => wasm.sim_set_wake_all(on ? 1 : 0),
      click: (o, d) => wasm.sim_click(o[0], o[1], o[2], d[0], d[1], d[2]),
      // { position: [x, y, z], state } of object `id` (1-based).
      object(id) {
        const [x, y, z, state] = new Int32Array(wasm.memory.buffer, wasm.sim_object_info(id), 4);
        return { position: [x, y, z], state };
      },
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
