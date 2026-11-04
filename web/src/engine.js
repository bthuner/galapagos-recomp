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
  // `no-cache` means "ask the server before reusing a cached copy" (a 304
  // costs a few bytes), not "never cache". Without it a browser may keep
  // running an engine from before the last deploy: servers that send no
  // Cache-Control (python's http.server) leave freshness to the browser's
  // own guess, and an old engine replays bugs that were already fixed.
  const { instance } = await WebAssembly.instantiateStreaming(fetch(url, { cache: "no-cache" }), {});
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
            vertices: new Float32Array(mem, wasm.world_vertices_ptr(), verts * 10).slice(),
            textureCount,
            textures: new Uint8Array(mem, wasm.world_textures_ptr(), textureCount * 128 * 128 * 4).slice(),
            bounds: [...new Float32Array(mem, wasm.world_bounds_ptr(), 6)],
            objects: wasm.world_object_count(),
            // Per-object draw ranges: [first0, count0, first1, count1, ...].
            draws: new Uint32Array(mem, wasm.world_draws_ptr(), wasm.world_draw_count() * 2).slice(),
            // Transforms change as the simulation runs, so this is a getter
            // returning a fresh view each frame (memory may have grown).
            // With `alpha` (0..1), the in-between frame from the previous
            // tick to the current one (not in the original, which drew
            // once per tick: Sim::transforms_at in crates/engine/src/sim.rs).
            transforms: (alpha) => {
              const p = alpha === undefined ? 0 : wasm.world_transforms_at(alpha);
              return new Float32Array(wasm.memory.buffer, p || wasm.world_transforms_ptr(), wasm.world_draw_count() * 16);
            },
            liveVertices: () => new Float32Array(wasm.memory.buffer, wasm.world_vertices_ptr(), verts * 10),
            // Objects hidden by a -5 appearance frame (fresh view each frame).
            visible: () => new Uint8Array(wasm.memory.buffer, wasm.world_visible_ptr(), wasm.world_draw_count()),
            // Objects whose glass is back-face culled too (fresh view each frame).
            cull: () => new Uint8Array(wasm.memory.buffer, wasm.world_cull_ptr(), wasm.world_draw_count()),
            farClip: wasm.world_far_clip(),
            // Lighting (crates/engine/src/world.rs, `light_level`): the
            // section's materials (4 floats each, indexed by a vertex's
            // `light`), the light's direction, and the fog / clear colour
            // (0 black, 1 white).
            materials: new Float32Array(mem, wasm.world_materials_ptr(), wasm.world_material_count() * 4).slice(),
            lightDir: [...new Float32Array(mem, wasm.world_light_ptr(), 3)],
            fogColour: wasm.world_fog_colour(),
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
      // The screen's palette flash (crates/engine/src/flash.rs): six
      // numbers, lo/hi for red, green, blue; each channel c (0..255) is
      // drawn as lo + c * (hi - lo) / 256. [0,255,0,255,0,255] = no flash.
      tint: () => [...new Int32Array(wasm.memory.buffer, wasm.sim_tint_ptr(), 6)],
      // The picture over the 3-D view (`Over` class: world 0's title) for
      // a view of w×h original pixels, or null.
      overlay(w, h) {
        const [picture, width, height, x, y, centred] = new Int32Array(wasm.memory.buffer, wasm.sim_overlay(w, h), 6);
        return picture < 0 ? null : { picture, width, height, x, y, centred: centred !== 0 };
      },
      // The predators of the section (crates/engine/src/predator.rs).
      predators() {
        const out = [];
        for (let k = 0; ; k++) {
          const p = wasm.sim_predator_info(k);
          if (!p) return out;
          const [id, cls, mode, contact] = new Int32Array(wasm.memory.buffer, p, 4);
          out.push({
            id,
            kind: ["Pred", "PBal", "PFrk", "PMan", "PPol"][cls],
            mode: ["idle", "vanishing", "appearing", "active"][mode],
            contact: !!contact,
          });
        }
      },
      // Teaching/debug, not in the original: run object `id`'s class
      // trigger with params entry `entry` (section 2's ball: trigger(54, 0)).
      trigger: (id, entry) => wasm.sim_debug_trigger(id, entry),
      // Teaching/debug, not in the original: send object `id`'s state
      // machine to `state` (entry actions run): goto(54, 3) in section 2.
      goto: (id, state) => wasm.sim_debug_goto(id, state),
    },

    // Mendel (see crates/engine/src/creature.rs). His posed mesh is rebuilt
    // in world space every tick, in the world's 10-float vertex layout.
    creature: {
      // A fresh view each call: memory may have grown, and the vertex
      // count changes (0 while he is absent or blinking).
      // With `alpha` (0..1): blended from the previous tick's mesh, like
      // the objects' transforms (Creature::vertices_at).
      vertices: (alpha) => {
        const p = alpha === undefined ? wasm.creature_vertices_ptr() : wasm.creature_vertices_at(alpha);
        return new Float32Array(wasm.memory.buffer, p, wasm.creature_vertex_count() * 10);
      },
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
      // Right click: a ray against his mesh. Returns "left", "right" or null.
      touch: (o, d) => [null, "left", "right"][wasm.creature_touch_ray(o[0], o[1], o[2], d[0], d[1], d[2])],
      // The side that ray would touch, without touching (keyboard play, #49).
      side: (o, d) => [null, "left", "right"][wasm.creature_side_ray(o[0], o[1], o[2], d[0], d[1], d[2])],
      // His affliction (crates/engine/src/affliction.rs): { kind, ticksLeft },
      // kind 0 = none.
      affliction() {
        const v = wasm.creature_affliction();
        return { kind: v & 0xff, ticksLeft: v >>> 8 };
      },
      // Teaching/debug, not in the original: afflict him with n (1..12).
      afflict: (n) => wasm.creature_afflict(n),
      // Teaching/debug, not in the original: hand him surface flags, as a
      // bite does; 0x01582380 kills him with transition effect 2 v0.
      react: (flags) => wasm.creature_react(flags >>> 0),
    },

    // The original game camera (crates/engine/src/camera.rs), ticked with
    // the simulation. Returns the previous and the current tick's view, so
    // the renderer can interpolate between them; null before a world loads.
    camera: {
      state() {
        const p = wasm.camera_state();
        if (!p) return null;
        const f = new Float32Array(wasm.memory.buffer, p, 32);
        const view = (o) => ({
          eye: [f[o + 1], f[o + 2], f[o + 3]],
          forward: [f[o + 4], f[o + 5], f[o + 6]], // unit vectors of the
          up: [f[o + 7], f[o + 8], f[o + 9]], //      original's view matrix
          look: [f[o + 10], f[o + 11], f[o + 12]], // filtered look-at point
          // The camera key in use: object id (0 = the section's default
          // key), polygon, and its style's mode (0..11, see objects.md §6).
          key: { object: f[o + 13], polygon: f[o + 14], mode: f[o + 15] },
        });
        return f[16] ? { prev: view(0), cur: view(16) } : null;
      },
    },

    // Saved games, the respawn snapshot and the time screen
    // (crates/engine/src/sim/save.rs, docs/re/saves.md).
    game: {
      // Serialises the whole game ("GLPS" v1) and, like the original after
      // a successful save, makes Mendel's current spot the respawn point.
      save() {
        const n = wasm.game_save();
        return new Uint8Array(wasm.memory.buffer, wasm.game_save_ptr(), n).slice();
      },
      // The same bytes without taking a snapshot: the respawn point stays
      // where the original left it (the autosave, web/src/qol/autosave.js).
      autosave() {
        const n = wasm.game_autosave();
        return new Uint8Array(wasm.memory.buffer, wasm.game_save_ptr(), n).slice();
      },
      // Rebuilds the saved section and restores everything (brain included).
      // Returns the section index; then re-read worlds.current().
      load(bytes) {
        const r = wasm.game_load(...upload(bytes));
        if (r !== 0) throw new Error(["", "Not a Galapagos save", "Unknown section", "Save does not fit its section"][-r]);
        return wasm.game_section();
      },
      // Read when EV_SECTION arrives, before building the next section.
      times() {
        const [levelTicks, totalTicks, levelSeconds, totalSeconds] = new Uint32Array(wasm.memory.buffer, wasm.game_times(), 4);
        return { levelTicks, totalTicks, levelSeconds, totalSeconds };
      },
      onSavePad: () => wasm.game_on_save_pad() === 1,
      resetProgress: () => wasm.game_reset_progress(),
      // Which picture and where to draw the times, for level 0..7 (8 = the
      // end of the game) in a view of w x h pixels; null if too small.
      timeScreen(level, w, h) {
        const v = new Int32Array(wasm.memory.buffer, wasm.time_screen_layout(level, w, h), 30).slice();
        if (v[0] < 0) return null;
        const glyphs = {};
        "0123456789:".split("").forEach((ch, k) => (glyphs[ch] = { x: v[8 + 2 * k], w: v[9 + 2 * k] }));
        return {
          picture: v[0], width: v[1], height: v[2], visibleHeight: v[3], glyphHeight: v[4],
          levelX: v[5], y: v[6], totalRight: v[7], glyphs,
        };
      },
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
