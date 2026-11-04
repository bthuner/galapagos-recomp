//! WebAssembly entry points.
//!
//! We deliberately avoid wasm-bindgen: the ABI is a handful of `extern "C"`
//! functions exchanging pointers into linear memory. It is small enough to
//! read in one sitting, which is the point of this project.
//!
//! Ownership rule: when JavaScript hands us a file it first calls [`alloc`],
//! fills the buffer, then passes `(ptr, len)` to an `*_open` function, which
//! takes ownership. JavaScript must not touch or free that buffer afterwards.

mod affliction;
mod camera;
mod creature;
mod flash;
mod overlay;
mod predator;
mod sim;
mod sound;
mod transition;
mod walker;
mod world;

use galapagos_formats::creature::{overlay_for_age, Brain, CreatureBody};
use galapagos_formats::smacker::{Smacker, YScale};
use galapagos_formats::worlds::{Archive, SoundSet};
use std::cell::RefCell;

/// Allocate `len` bytes the host (JavaScript) can write a file into.
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    core::mem::forget(v);
    p
}

/// Free a buffer from [`alloc`] that was never handed to an `*_open` call.
#[no_mangle]
pub extern "C" fn dealloc(ptr: *mut u8, len: usize) {
    unsafe { drop(Vec::from_raw_parts(ptr, 0, len)) }
}

/// Turn an [`alloc`]ated buffer back into an owned `Vec` without copying.
unsafe fn take(ptr: *mut u8, len: usize) -> Vec<u8> {
    Vec::from_raw_parts(ptr, len, len)
}

/// Returns the number of world sections, or -1 if the archive is malformed.
#[no_mangle]
pub extern "C" fn worlds_section_count(ptr: *const u8, len: usize) -> i32 {
    let data = unsafe { core::slice::from_raw_parts(ptr, len) };
    match Archive::parse(data) {
        Ok(a) => a.sections.len() as i32,
        Err(_) => -1,
    }
}

// ——— Movies (Smacker) ————————————————————————————————————————————————
//
// One movie plays at a time, like in the original game. WebAssembly here is
// single-threaded, so a thread-local RefCell is a safe home for that state.

struct Movie {
    smk: Smacker,
    /// The latest frame as RGBA at its stored size (640x240 for the intro).
    rgba: Vec<u8>,
    /// Audio track 0 for the whole movie, decoded up front.
    audio: Vec<i16>,
    /// Header facts, laid out for JavaScript to read as a Uint32Array.
    info: [u32; 8],
}

thread_local! {
    static MOVIE: RefCell<Option<Movie>> = const { RefCell::new(None) };
}

/// Open a Smacker file. Returns 0 on success, -1 if it isn't valid Smacker.
#[no_mangle]
pub extern "C" fn movie_open(ptr: *mut u8, len: usize) -> i32 {
    let Ok(smk) = Smacker::from_vec(unsafe { take(ptr, len) }) else {
        return -1;
    };
    let i = smk.info();
    let (rate, channels) = i.audio.first().map_or((0, 0), |t| (t.sample_rate, t.channels as u32));
    let y_scale = match i.y_scale {
        YScale::None => 0,
        YScale::Doubled => 1,
        YScale::Interlaced => 2,
    };
    let info = [i.width, i.height, i.display_height(), i.frame_count, i.us_per_frame, rate, channels, y_scale];
    MOVIE.with(|m| *m.borrow_mut() = Some(Movie { smk, rgba: Vec::new(), audio: Vec::new(), info }));
    0
}

/// Pointer to eight u32s: width, stored height, display height, frames,
/// µs per frame, audio sample rate, audio channels, and the Y mode
/// (0 = as stored, 1 = line-doubled, 2 = interlaced with black lines).
#[no_mangle]
pub extern "C" fn movie_info() -> *const u32 {
    MOVIE.with(|m| m.borrow().as_ref().map_or(core::ptr::null(), |m| m.info.as_ptr()))
}

/// Decode the entire soundtrack (all frames) and rewind. Returns the number
/// of i16 samples (interleaved if stereo), readable at [`movie_audio_ptr`].
///
/// Why up front? The browser plays an AudioBuffer with sample-accurate timing,
/// so we let the audio clock drive which video frame is shown. Picture
/// follows sound and the two cannot drift.
#[no_mangle]
pub extern "C" fn movie_decode_audio() -> i32 {
    MOVIE.with(|m| {
        let mut guard = m.borrow_mut();
        let Some(movie) = guard.as_mut() else { return -1 };
        let mut audio = Vec::new();
        while movie.smk.frames_remaining() > 0 {
            match movie.smk.decode_next_frame() {
                Ok(f) => audio.extend_from_slice(&f.audio[0]),
                Err(_) => return -1,
            }
        }
        movie.smk.rewind();
        movie.audio = audio;
        movie.audio.len() as i32
    })
}

#[no_mangle]
pub extern "C" fn movie_audio_ptr() -> *const i16 {
    MOVIE.with(|m| m.borrow().as_ref().map_or(core::ptr::null(), |m| m.audio.as_ptr()))
}

/// Decode the next frame into the RGBA buffer. Returns 1 when a frame was
/// decoded, 0 at the end of the movie, -1 on a decoding error.
#[no_mangle]
pub extern "C" fn movie_next_frame() -> i32 {
    MOVIE.with(|m| {
        let mut guard = m.borrow_mut();
        let Some(movie) = guard.as_mut() else { return -1 };
        if movie.smk.frames_remaining() == 0 {
            return 0;
        }
        match movie.smk.decode_next_frame() {
            Ok(f) => {
                movie.rgba = f.to_rgba_stored();
                1
            }
            Err(_) => -1,
        }
    })
}

/// The current frame as width × stored-height RGBA bytes. Scaling to the
/// display height is the browser's job (see web/src/movie.js).
#[no_mangle]
pub extern "C" fn movie_frame_ptr() -> *const u8 {
    MOVIE.with(|m| m.borrow().as_ref().map_or(core::ptr::null(), |m| m.rgba.as_ptr()))
}

#[no_mangle]
pub extern "C" fn movie_close() {
    MOVIE.with(|m| *m.borrow_mut() = None);
}

// ——— Worlds ——————————————————————————————————————————————————————————
//
// The 126 MB archive is adopted once; a section is parsed and flattened into
// GPU buffers on request (see world.rs). Parsing is fast because the format
// is plain offsets into the same bytes: nothing is decompressed.

struct Worlds {
    data: Vec<u8>,
    mesh: Option<world::WorldMesh>,
    sim: Option<sim::Sim>,
    /// The original game camera, ticked after the simulation (camera.rs).
    camera: Option<camera::Camera>,
    /// Host events handed out by the last `sim_tick`.
    events: Vec<sim::HostEvent>,
    /// Sound banks, indexed by `sound::BANK_*`: the current section's,
    /// Mendel's and the predators' (the last two live in the global block).
    banks: [sound::Bank; 3],
    /// Preferences (see `game_new`, `sound_set_quality`).
    age: u8,
    quality: u8,
    /// The section on screen, and a brain waiting for the next build
    /// (a new game or an imported `.mnd`).
    section: usize,
    pending_brain: Option<Brain>,
}

thread_local! {
    static WORLDS: RefCell<Option<Worlds>> = const { RefCell::new(None) };
}

/// Adopt the `worlds` file. Returns the section count, or -1 if malformed.
#[no_mangle]
pub extern "C" fn worlds_open(ptr: *mut u8, len: usize) -> i32 {
    let data = unsafe { take(ptr, len) };
    let (n, creature, predator) = match Archive::parse(&data) {
        Ok(a) => {
            let bank = |off: u32| {
                SoundSet::parse(&data, off as usize)
                    .map_or_else(|_| sound::Bank::empty(), |s| sound::Bank::decode(&data, &s, sound::BEST_QUALITY))
            };
            (a.sections.len() as i32, bank(a.header.creature_sounds), bank(a.header.predator_sounds))
        }
        Err(_) => return -1,
    };
    WORLDS.with(|w| {
        let banks = [sound::Bank::empty(), creature, predator];
        *w.borrow_mut() = Some(Worlds {
            data,
            mesh: None,
            sim: None,
            camera: None,
            events: Vec::new(),
            banks,
            age: DEFAULT_AGE,
            quality: sound::BEST_QUALITY,
            section: 0,
            pending_brain: None,
        })
    });
    n
}

/// Build render buffers for section `i`. Returns the number of vertices
/// (10 floats each, see world.rs), or -1 on a parse error.
#[no_mangle]
pub extern "C" fn world_build(i: usize) -> i32 {
    WORLDS.with(|w| w.borrow_mut().as_mut().map_or(-1, |worlds| build_section(worlds, i)))
}

fn build_section(worlds: &mut Worlds, i: usize) -> i32 {
    let Ok(archive) = Archive::parse(&worlds.data) else { return -1 };
    let Ok(section) = archive.world(i) else { return -1 };
    let mut mesh = world::WorldMesh::build(&section);
    let n = (mesh.vertices.len() / world::FLOATS_PER_VERTEX) as i32;
    worlds.banks[sound::BANK_WORLD as usize] = sound::Bank::decode(&worlds.data, &section.sounds, worlds.quality);
    worlds.section = i;
    let mut sim = sim::Sim::build(&section, &mesh);
    sim.section = i;
    let old = worlds.sim.take();
    // The `Save` variables and the game clock outlive the section; the
    // level clock restarts (`Game_LevelEnd` @ 0x41c8c0: start = total).
    if let Some(o) = &old {
        sim.vars = o.vars;
        sim.total_ticks = o.total_ticks;
        sim.level_start_ticks = o.total_ticks;
    }
    // The load pass of `World_Read` @ 0x4222d0: every initial state is
    // entered now, with the variables already in place (they live in the
    // archive object, which outlives the section: `0x41cc20`).
    sim.enter_initial_states(&mut mesh);
    // Mendel carries his brain from section to section; the first time,
    // it comes from the CD for the age preference (see `brain_for_age`).
    let carried = old.and_then(|s| s.creature).map(|c| c.brain);
    let brain = worlds.pending_brain.take().or(carried).or_else(|| brain_for_age(&archive, worlds.age));
    let body = CreatureBody::parse(&worlds.data, archive.header.creature as usize).ok();
    if let (Some(body), Some(brain)) = (body, brain) {
        sim.creature = Some(creature::Creature::new(&section, &mesh, body, brain));
    }
    // A level starts with a world snapshot: dying before any save brings
    // the level back to this instant (`0x41caf3` → `Game_TakeSnapshot`).
    sim.take_snapshot();
    // `Camera_Init` at level load: the section's default key.
    let mut cam = camera::Camera::new(&section);
    cam.start_level(&sim);
    worlds.camera = Some(cam);
    worlds.sim = Some(sim);
    worlds.mesh = Some(mesh);
    n
}

/// The Preferences "New Mendel Age" slider (0..=255) decides which brain a
/// new game starts with (`0x4171ad`): Newborn, 1, 2, 4 or 7 hours. We start
/// in the middle, "2 hours" (level 3).
pub const DEFAULT_AGE: u8 = 128;

/// `Game_LoadCreatureBrainForSection` @ 0x41ced0: level 1 is a newborn
/// (random genome), levels 2..5 load a pre-trained brain from the CD.
fn brain_for_age(archive: &Archive, age: u8) -> Option<Brain> {
    let overlay = |i: usize| Brain::parse(archive.data, archive.header.creature_overlays[i] as usize).ok();
    match overlay_for_age(age) {
        Some(i) => overlay(i),
        // inferred seed: any LCG start makes a different newborn.
        None => overlay(1).map(|t| Brain::newborn(&t, 0x4d656e64)),
    }
}

fn with_mesh<T>(f: impl FnOnce(&world::WorldMesh) -> T, default: T) -> T {
    WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.mesh.as_ref()).map_or(default, f))
}

#[no_mangle]
pub extern "C" fn world_vertices_ptr() -> *const f32 {
    with_mesh(|m| m.vertices.as_ptr(), core::ptr::null())
}

/// Number of 128×128 RGBA texture layers at [`world_textures_ptr`].
#[no_mangle]
pub extern "C" fn world_texture_count() -> u32 {
    with_mesh(|m| m.texture_count, 0)
}

#[no_mangle]
pub extern "C" fn world_textures_ptr() -> *const u8 {
    with_mesh(|m| m.textures.as_ptr(), core::ptr::null())
}

/// Six f32: min x, y, z, max x, y, z.
#[no_mangle]
pub extern "C" fn world_bounds_ptr() -> *const f32 {
    with_mesh(|m| m.bounds.as_ptr(), core::ptr::null())
}

/// How many objects with geometry the current section contains.
#[no_mangle]
pub extern "C" fn world_object_count() -> u32 {
    with_mesh(|m| m.objects_drawn, 0)
}

/// Far-clip distance of the current section's environment, in world units.
#[no_mangle]
pub extern "C" fn world_far_clip() -> f32 {
    with_mesh(|m| m.far_clip, 0.0)
}

/// The fog and clear colour of the current section: 0 black, 1 white
/// (`world::fog_white`).
#[no_mangle]
pub extern "C" fn world_fog_colour() -> f32 {
    with_mesh(|m| m.fog, 0.0)
}

/// Number of section materials (colour ramps) at [`world_materials_ptr`].
#[no_mangle]
pub extern "C" fn world_material_count() -> u32 {
    with_mesh(|m| (m.materials.len() / world::FLOATS_PER_MATERIAL) as u32, 0)
}

/// The section's materials for the shader, 4 f32 each (`m0 + m2·ambient`,
/// `m1`, `m3·intensity`, `m5`; see `world::light_level`). A vertex's
/// `light` float indexes them.
#[no_mangle]
pub extern "C" fn world_materials_ptr() -> *const f32 {
    with_mesh(|m| m.materials.as_ptr(), core::ptr::null())
}

/// The section's directional light: 3 f32, the unit vector light travels.
#[no_mangle]
pub extern "C" fn world_light_ptr() -> *const f32 {
    with_mesh(|m| m.light.dir.as_ptr(), core::ptr::null())
}

/// Number of drawn objects; each has a [`world::DrawRange`] (two u32:
/// first vertex, vertex count) and a 4×4 column-major transform.
#[no_mangle]
pub extern "C" fn world_draw_count() -> u32 {
    with_mesh(|m| m.draws.len() as u32, 0)
}

#[no_mangle]
pub extern "C" fn world_draws_ptr() -> *const u32 {
    with_mesh(|m| m.draws.as_ptr() as *const u32, core::ptr::null())
}

#[no_mangle]
pub extern "C" fn world_transforms_ptr() -> *const f32 {
    with_mesh(|m| m.transforms.as_ptr(), core::ptr::null())
}

thread_local! {
    /// The in-between frame of the object matrices and of Mendel's mesh
    /// (`world_transforms_at`, `creature_vertices_at`).
    static BLENDED: core::cell::RefCell<(Vec<f32>, Vec<f32>)> = const { core::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// The object matrices `alpha` (0..1) of the way from the previous tick
/// to the current one, same layout as [`world_transforms_ptr`]. not in the
/// original: see `Sim::transforms_at`.
#[no_mangle]
pub extern "C" fn world_transforms_at(alpha: f32) -> *const f32 {
    WORLDS.with(|w| {
        let guard = w.borrow();
        let Some((sim, mesh)) = guard.as_ref().and_then(|w| Some((w.sim.as_ref()?, w.mesh.as_ref()?))) else { return core::ptr::null() };
        BLENDED.with(|b| {
            let out = &mut b.borrow_mut().0;
            sim.transforms_at(alpha, mesh, out);
            out.as_ptr()
        })
    })
}

// ——— Simulation ——————————————————————————————————————————————————————
//
// JavaScript owns the clock: it calls `sim_tick` as many times as 24 Hz
// demands, then reads back what changed (transforms are re-read every frame;
// vertex changes come as one dirty range; side effects come as events).

/// Run `n` ticks of the world simulation. Returns the number of host events
/// produced (read them at [`sim_events_ptr`], 4 × i32 each: kind, a, b, c).
///
/// | kind | event | a, b, c |
/// |---|---|---|
/// | 1 | music | Smacker file 1..10 (0 = stop) |
/// | 2 | dimensional gate | section, level |
/// | 3 | world sound | world-bank sound, object id, emitter |
/// | 4 | camera cut | — |
/// | 5 | Mendel's cue | creature-bank sound |
/// | 6 | save pad clicked | object id |
/// | 7 | game completed | level 8, picture 9 |
/// | 8 | affliction sound, at Mendel | world-bank sound, pitch 16.16, volume \| variation>>8 << 16 |
/// | 9 | predator sound | predator-bank sound (0 growl, 1 bite), object id, pitch 16.16 |
/// | 10 | transition effect sound | world-bank sound, object id (0 = Mendel), packed cue (`sim::EV_EFFECT_SOUND`) |
#[no_mangle]
pub extern "C" fn sim_tick(n: u32) -> u32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return 0 };
        let (Some(sim), Some(mesh)) = (worlds.sim.as_mut(), worlds.mesh.as_mut()) else { return 0 };
        for _ in 0..n {
            let before = sim.events.len();
            sim.step(mesh);
            // Game_Tick order: objects → creature → camera.
            if let Some(cam) = worlds.camera.as_mut() {
                let snap = sim.events[before..].iter().any(|e| e.kind == sim::EV_CAMERA_SNAP);
                cam.tick(sim, snap);
            }
        }
        worlds.events = std::mem::take(&mut sim.events);
        worlds.events.len() as u32
    })
}

#[no_mangle]
pub extern "C" fn sim_events_ptr() -> *const sim::HostEvent {
    WORLDS.with(|w| w.borrow().as_ref().map_or(core::ptr::null(), |w| w.events.as_ptr()))
}

/// The original camera (camera.rs): 2 × 16 f32, the previous tick's view
/// then the current one, so the browser can interpolate between them.
/// Each: valid, eye xyz, forward xyz, up xyz, look-at xyz, key object id
/// (1-based, 0 = the section's default key), key polygon, style mode.
#[no_mangle]
pub extern "C" fn camera_state() -> *const f32 {
    WORLDS.with(|w| {
        w.borrow().as_ref().and_then(|w| w.camera.as_ref()).map_or(core::ptr::null(), |c| c.samples.as_ptr() as *const f32)
    })
}

/// Takes the vertex range patched since the last call. Returns -1 if
/// nothing changed; otherwise 0, with `[first, end)` in the two u32 at
/// [`sim_dirty_ptr`].
#[no_mangle]
pub extern "C" fn sim_take_dirty() -> i32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return -1 };
        let Some(sim) = worlds.sim.as_mut() else { return -1 };
        match sim.dirty.take() {
            Some((a, b)) => {
                DIRTY.with(|d| d.set([a, b]));
                0
            }
            None => -1,
        }
    })
}

thread_local! {
    static DIRTY: std::cell::Cell<[u32; 2]> = const { std::cell::Cell::new([0, 0]) };
}

#[no_mangle]
pub extern "C" fn sim_dirty_ptr() -> *const u32 {
    DIRTY.with(|d| d.as_ptr() as *const u32)
}

/// Pick along a world-space ray; a hit counts as a click on the next tick.
/// Returns the object id hit, or 0. A click on a save-pad polygon (frame
/// flag 0x20) while Mendel stands on one also queues `EV_SAVE_PAD`, as the
/// original's click handler opens its Save dialog (0x41baa3..0x41bb07).
#[no_mangle]
pub extern "C" fn sim_click(ox: f32, oy: f32, oz: f32, dx: f32, dy: f32, dz: f32) -> u32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return 0 };
        let Some(sim) = worlds.sim.as_mut() else { return 0 };
        match sim.pick_hit([ox, oy, oz], [dx, dy, dz]) {
            Some((id, flags)) => {
                if flags & 0x20 != 0 && sim.creature.as_ref().is_some_and(|c| c.on_save_pad) {
                    sim.events.push(sim::HostEvent { kind: sim::EV_SAVE_PAD, a: id as i32, b: 0, c: 0 });
                }
                sim.click(id);
                id as u32
            }
            None => 0,
        }
    })
}

// ——— Hover: would a click matter? (Quality of Life #41, sim/hover.rs) ———
// Read-only questions: nothing here clicks, draws a random number or
// changes what the next tick sees.

/// The object a left click along this ray would reach, if that click would
/// currently do something (same pick as `sim_click`); -1 otherwise.
#[no_mangle]
pub extern "C" fn sim_hover(ox: f32, oy: f32, oz: f32, dx: f32, dy: f32, dz: f32) -> i32 {
    peek_sim(|sim| sim.hover([ox, oy, oz], [dx, dy, dz]).map_or(-1, i32::from), -1)
}

/// The renderer's draw index of object `id` (1-based), or -1 if it has no mesh.
#[no_mangle]
pub extern "C" fn sim_object_draw(id: u32) -> i32 {
    peek_sim(|sim| sim.draw_of(id as u16).map_or(-1, |d| d as i32), -1)
}

thread_local! {
    static CLICKABLE: std::cell::RefCell<Vec<u32>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Every awake object a click would currently matter for, ignoring line
/// of sight (for keyboard-only play, #49). Returns the count; the 1-based
/// ids (u32 each) are at `sim_clickable_ptr()` until the next call.
#[no_mangle]
pub extern "C" fn sim_clickable() -> u32 {
    let ids = peek_sim(|sim| sim.clickable_ids(), Vec::new());
    CLICKABLE.with(|c| {
        let mut c = c.borrow_mut();
        c.clear();
        c.extend(ids.into_iter().map(u32::from));
        c.len() as u32
    })
}

#[no_mangle]
pub extern "C" fn sim_clickable_ptr() -> *const u32 {
    CLICKABLE.with(|c| c.borrow().as_ptr())
}

fn peek_sim<T>(f: impl FnOnce(&sim::Sim) -> T, default: T) -> T {
    WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.sim.as_ref()).map_or(default, f))
}

/// The simulation rate. 24 Hz is inferred: the time screen divides the
/// tick counters by 24 (docs/re/objects.md §7).
#[no_mangle]
pub extern "C" fn sim_ticks_per_second() -> u32 {
    sim::TICKS_PER_SECOND
}

thread_local! {
    static SCRATCH: std::cell::Cell<[i32; 4]> = const { std::cell::Cell::new([0; 4]) };
}

/// Position (3 × i32) and current state (1 × i32) of object `id`, for
/// debugging and the making-of's "poke it from the console" examples.
#[no_mangle]
pub extern "C" fn sim_object_info(id: u32) -> *const i32 {
    WORLDS.with(|w| {
        if let Some(sim) = w.borrow().as_ref().and_then(|w| w.sim.as_ref()) {
            let p = sim.position_of(id as u16);
            SCRATCH.with(|s| s.set([p[0], p[1], p[2], sim.state_of(id as u16) as i32]));
        }
    });
    SCRATCH.with(|s| s.as_ptr() as *const i32)
}

/// Debug/teaching switch: run every state machine, ignoring folder gating.
#[no_mangle]
pub extern "C" fn sim_set_wake_all(on: u32) {
    WORLDS.with(|w| {
        if let Some(sim) = w.borrow_mut().as_mut().and_then(|w| w.sim.as_mut()) {
            sim.wake_all = on != 0;
        }
    });
}

/// Hold (1) or release (0) the "skip" key: Space in the original, which
/// makes every `STel` object count as clicked (section 0: skip the intro).
#[no_mangle]
pub extern "C" fn sim_hold_skip(on: u32) {
    with_sim_mut(|sim| sim.skip_held = on != 0);
}

fn with_sim_mut(f: impl FnOnce(&mut sim::Sim)) {
    WORLDS.with(|w| {
        if let Some(sim) = w.borrow_mut().as_mut().and_then(|w| w.sim.as_mut()) {
            f(sim);
        }
    });
}

// ——— Mendel ——————————————————————————————————————————————————————————
//
// His posed mesh is rebuilt every tick in world space (same 10-float vertex
// layout as the world, texture layers 16..19), so the renderer draws it
// with an identity model matrix.

fn with_creature<T>(f: impl FnOnce(&creature::Creature) -> T, default: T) -> T {
    WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.sim.as_ref()).and_then(|s| s.creature.as_ref()).map_or(default, f))
}

#[no_mangle]
pub extern "C" fn creature_vertices_ptr() -> *const f32 {
    with_creature(|c| c.vertices.as_ptr(), core::ptr::null())
}

/// Mendel's mesh `alpha` (0..1) of the way from the previous tick to the
/// current one ([`creature_vertex_count`] vertices). not in the original:
/// see `Creature::vertices_at`.
#[no_mangle]
pub extern "C" fn creature_vertices_at(alpha: f32) -> *const f32 {
    with_creature(
        |c| {
            BLENDED.with(|b| {
                let out = &mut b.borrow_mut().1;
                c.vertices_at(alpha, out);
                out.as_ptr()
            })
        },
        core::ptr::null(),
    )
}

/// Number of vertices at [`creature_vertices_ptr`] (0 while he is absent).
#[no_mangle]
pub extern "C" fn creature_vertex_count() -> u32 {
    with_creature(|c| (c.vertices.len() / world::FLOATS_PER_VERTEX) as u32, 0)
}

thread_local! {
    static CREATURE_STATE: std::cell::Cell<[f32; 16]> = const { std::cell::Cell::new([0.0; 16]) };
}

/// Sixteen f32 describing Mendel, for the camera and the HUD:
///
/// ```text
/// 0 mode (0 absent, 1 appearing, 2 alive, 3 vanishing)   1..3 x, y, z
/// 4 heading (1024ths)   5 ground object id (0 = airborne)   6 ground polygon
/// 7 left eye/touch   8 right   9 posture   10 speed   11 turn  (actuators, 0..256)
/// 12 distance walked   13 touch events posted   14 deaths   15 generations evolved
/// ```
#[no_mangle]
pub extern "C" fn creature_state() -> *const f32 {
    let s = with_creature(
        |c| {
            let mode = match c.mode {
                creature::Mode::Absent => 0.0,
                creature::Mode::Appearing(_) => 1.0,
                creature::Mode::Alive => 2.0,
                creature::Mode::Vanishing(..) => 3.0,
            };
            let (gobj, gpoly) = c.ground().map_or((0.0, 0.0), |(o, p)| (o as f32 + 1.0, p as f32));
            let [a0, a1, a2] = c.outputs().map(|v| v as f32);
            [
                mode, c.pos[0], c.pos[1], c.pos[2], c.heading() as f32, gobj, gpoly, c.left as f32, c.right as f32, a0, a1, a2,
                c.stats.walked, c.stats.touches_posted as f32, c.stats.deaths as f32, c.stats.evolutions as f32,
            ]
        },
        [0.0; 16],
    );
    CREATURE_STATE.with(|c| c.set(s));
    CREATURE_STATE.with(|c| c.as_ptr() as *const f32)
}

/// Right click: ray against Mendel. Returns 1 if his left side was touched,
/// 2 for the right, 0 for a miss. A touch is felt for 4 ticks.
#[no_mangle]
pub extern "C" fn creature_touch_ray(ox: f32, oy: f32, oz: f32, dx: f32, dy: f32, dz: f32) -> u32 {
    let mut out = 0;
    with_sim_mut(|sim| {
        if let Some(c) = sim.creature.as_mut() {
            if let Some(left) = c.pick([ox, oy, oz], [dx, dy, dz]) {
                c.touch(left);
                out = if left { 1 } else { 2 };
            }
        }
    });
    out
}

/// Read-only twin of `creature_touch_ray`, for keyboard-only play (Quality
/// of Life #49, not in the original): which side of Mendel the ray would
/// touch (1 left, 2 right, 0 miss), without touching him. The keyboard's
/// "touch" key uses it to aim at the side it wants, then right-clicks there.
#[no_mangle]
pub extern "C" fn creature_side_ray(ox: f32, oy: f32, oz: f32, dx: f32, dy: f32, dz: f32) -> u32 {
    with_creature(|c| c.pick([ox, oy, oz], [dx, dy, dz]).map_or(0, |left| if left { 1 } else { 2 }), 0)
}

/// Mendel's affliction (`creature+0x8fc`, affliction.rs): the number in
/// the low byte (0 = none), the ticks it has left above it.
#[no_mangle]
pub extern "C" fn creature_affliction() -> u32 {
    with_creature(|c| c.afflictions.kind as u32 | (c.afflictions.left().max(0) as u32) << 8, 0)
}

/// Teaching/debug switch, not in the original: afflict Mendel with `n`
/// (1..12) as if he touched a polygon carrying it.
#[no_mangle]
pub extern "C" fn creature_afflict(n: u32) {
    with_sim_mut(|sim| {
        if let Some(mut c) = sim.creature.take() {
            c.afflictions.apply(n.min(255) as u8, sim);
            sim.creature = Some(c);
        }
    });
}

/// Teaching/debug switch, not in the original: hand Mendel surface flags
/// as a predator's bite does (`Game_CreatureReact` @ 0x41cb80), felt with
/// his next touch. `0x01582380` is the commonest lethal floor (transition
/// effect kind 2, variant 0); bits 12..18 pick any other effect.
#[no_mangle]
pub extern "C" fn creature_react(flags: u32) {
    with_sim_mut(|sim| {
        if let Some(c) = sim.creature.as_mut() {
            c.react(flags);
        }
    });
}

thread_local! {
    static TINT: std::cell::Cell<[i32; 6]> = const { std::cell::Cell::new(flash::IDENTITY) };
}

/// The screen's palette flash (flash.rs) this tick: six i32, `lo, hi` for
/// red, green, blue. Each channel `c` (0..255) is drawn as
/// `lo + c·(hi − lo)/256`; `0, 255` three times means "no flash".
#[no_mangle]
pub extern "C" fn sim_tint_ptr() -> *const i32 {
    let t = WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.sim.as_ref()).map_or(flash::IDENTITY, |s| s.flash.current()));
    TINT.with(|c| c.set(t));
    TINT.with(|c| c.as_ptr() as *const i32)
}

thread_local! {
    static OVERLAY: std::cell::Cell<[i32; 6]> = const { std::cell::Cell::new([-1, 0, 0, 0, 0, 0]) };
}

/// The picture over the 3-D view (`Over` class, overlay.rs) for a view of
/// `w`×`h` original pixels: six i32 — picture index for `Archive::picture`
/// (-1: none, or the view is too small), width, visible height, x, y,
/// centred (1: add `(screen − picture) / 2` to x and y, `0x436270`). The
/// picture's top-left pixel is the transparent colour.
#[no_mangle]
pub extern "C" fn sim_overlay(w: u32, h: u32) -> *const i32 {
    let o = WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.sim.as_ref()).and_then(|s| s.overlay));
    let pick = o.and_then(|o| match o.picture {
        0 => overlay::title_strip(w, h),
        n => time_screen(n as i32 - 1, w, h).map(|l| (l.0, l.1, l.2 - l.3)),
    });
    let out = match (o, pick) {
        (Some(o), Some((pic, pw, ph))) => [pic, pw, ph, o.x as i32, o.y as i32, o.centred as i32],
        _ => [-1, 0, 0, 0, 0, 0],
    };
    OVERLAY.with(|c| c.set(out));
    OVERLAY.with(|c| c.as_ptr() as *const i32)
}

/// Teaching/debug switch, not in the original: run the class trigger of
/// object `id` (1-based) with its params entry `entry` (0-based), as a
/// state machine would (e.g. make section 2's ball #54 appear: entry 0).
#[no_mangle]
pub extern "C" fn sim_debug_trigger(id: u32, entry: u32) {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return };
        let (Some(sim), Some(mesh)) = (worlds.sim.as_mut(), worlds.mesh.as_mut()) else { return };
        sim.debug_trigger(id.wrapping_sub(1) as usize, entry as usize, mesh);
    });
}

/// Teaching/debug switch, not in the original: send object `id`'s state
/// machine to `state` on the next tick, running that state's entry actions
/// (section 2's ball #54, state 3: it appears, then hunts in state 1).
#[no_mangle]
pub extern "C" fn sim_debug_goto(id: u32, state: u32) {
    with_sim_mut(|sim| sim.debug_goto(id.wrapping_sub(1) as usize, state as u16));
}

/// Predators (predator.rs): for predator `k` (0-based, in object order),
/// four i32 — object id (1-based), class (0 Pred, 1 PBal, 2 PFrk, 3 PMan,
/// 4 PPol), mode (0 idle, 1 vanishing, 2 appearing, 3 active), contact.
/// Returns null past the last one.
#[no_mangle]
pub extern "C" fn sim_predator_info(k: u32) -> *const i32 {
    let mut ok = false;
    WORLDS.with(|w| {
        if let Some(p) = w.borrow().as_ref().and_then(|w| w.sim.as_ref()).and_then(|s| s.predators.get(k as usize)) {
            let class = match p.class {
                predator::Class::Pred => 0,
                predator::Class::Ball => 1,
                predator::Class::Fork => 2,
                predator::Class::Man => 3,
                predator::Class::Pole => 4,
            };
            SCRATCH.with(|s| s.set([p.obj as i32 + 1, class, p.mode as i32, p.contact as i32]));
            ok = true;
        }
    });
    if ok { SCRATCH.with(|s| s.as_ptr() as *const i32) } else { core::ptr::null() }
}

// ——— Sounds ——————————————————————————————————————————————————————————
//
// JavaScript fetches each sample once (lazily, when an event first asks for
// it) and turns it into a WebAudio AudioBuffer. Events say *which* sound and
// *who* plays it; the emitter supplies how (distance, volume, pitch, loop).

thread_local! {
    static SOUND_INFO: std::cell::Cell<[u32; 4]> = const { std::cell::Cell::new([0; 4]) };
    static EMITTER: std::cell::Cell<[i32; 6]> = const { std::cell::Cell::new([0; 6]) };
}

/// Four u32 for sound `i` of `bank`: sample rate, sample count (0 = empty
/// slot), MIDI base note, pointer to the i16 mono samples.
#[no_mangle]
pub extern "C" fn sound_info(bank: u32, i: u32) -> *const u32 {
    WORLDS.with(|w| {
        let guard = w.borrow();
        let pcm = guard.as_ref().and_then(|w| w.banks.get(bank as usize)?.sounds.get(i as usize)?.as_ref());
        let info = pcm.map_or([0; 4], |p| [p.rate, p.samples.len() as u32, p.base_note as u32, p.samples.as_ptr() as u32]);
        SOUND_INFO.with(|s| s.set(info));
    });
    SOUND_INFO.with(|s| s.as_ptr() as *const u32)
}

/// Six i32 describing emitter `n` (0-based) of object `id`: near and far
/// distance, volume (256 = full), looping (0/1), pitch and pitch variation
/// (both 16.16 fixed point). All but the indices are inferred meanings
/// (docs/re/objects.md, sound emitters).
#[no_mangle]
pub extern "C" fn world_emitter(id: u32, n: u32) -> *const i32 {
    WORLDS.with(|w| {
        let guard = w.borrow();
        let e = guard.as_ref().and_then(|w| w.sim.as_ref()?.emitter(id as u16, n as usize));
        let v = e.map_or([0; 6], |e| [e.near, e.far, e.volume as i32, e.looping as i32, e.pitch as i32, e.variation as i32]);
        EMITTER.with(|s| s.set(v));
    });
    EMITTER.with(|s| s.as_ptr() as *const i32)
}

// ——— Saves, snapshots and the time screen ————————————————————————————————
//
// docs/re/saves.md is the spec. The flow the original implements:
//
// * click a save-pad polygon while Mendel stands on a save pad → the host
//   gets `EV_SAVE_PAD` and shows its Save dialog; on "Save" it calls
//   [`game_save`] (which also re-anchors the respawn snapshot) and stores
//   the bytes;
// * Mendel dies → the engine restores the snapshot by itself;
// * Open → [`game_load`] with those bytes;
// * a dimensional gate (`EV_SECTION`) → the host reads [`game_times`] for
//   the time screen *before* calling `world_build` for the next section.

thread_local! {
    static SAVE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static TIMES: std::cell::Cell<[u32; 4]> = const { std::cell::Cell::new([0; 4]) };
    static TIME_SCREEN: std::cell::Cell<[i32; 30]> = const { std::cell::Cell::new([0; 30]) };
}

/// Serialise the whole game (format `GLPS`, see `Sim::save_game`).
/// Like the original after a successful save, this first takes a new world
/// snapshot: Mendel's current spot becomes his respawn point. Returns the
/// byte length (0 if no section is loaded); read the bytes at
/// [`game_save_ptr`] before the next engine call.
#[no_mangle]
pub extern "C" fn game_save() -> u32 {
    save_into_buffer(true)
}

/// The same bytes as [`game_save`], **without** taking a snapshot: the
/// respawn point stays the last save pad (or level start), exactly as the
/// original left it. This is what the web autosave uses (not in the 1997
/// game): it must only survive closing the tab, never change what dying
/// does. The file still carries the current snapshot, so opening it brings
/// the old respawn point back too.
#[no_mangle]
pub extern "C" fn game_autosave() -> u32 {
    save_into_buffer(false)
}

fn save_into_buffer(snapshot: bool) -> u32 {
    let bytes = WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let sim = guard.as_mut().and_then(|w| w.sim.as_mut())?;
        if snapshot {
            sim.take_snapshot();
        }
        Some(sim.save_game())
    });
    let bytes = bytes.unwrap_or_default();
    let n = bytes.len() as u32;
    SAVE.with(|s| *s.borrow_mut() = bytes);
    n
}

#[no_mangle]
pub extern "C" fn game_save_ptr() -> *const u8 {
    SAVE.with(|s| s.borrow().as_ptr())
}

/// Open a saved game. `ptr` comes from [`alloc`] and is adopted (freed by
/// the engine), like the `*_open` functions. Rebuilds the saved section,
/// then restores the world, the snapshot, the clocks, Mendel and his brain.
/// Returns 0 on success, -1 if the bytes are not a save of this format,
/// -2 if its section does not exist, -3 if it does not fit that section
/// (the freshly built section is then left running). After success the
/// host re-reads the world buffers as after `world_build`
/// ([`world_vertex_count`] gives the vertex count).
#[no_mangle]
pub extern "C" fn game_load(ptr: *mut u8, len: usize) -> i32 {
    let data = unsafe { take(ptr, len) };
    let Ok(section) = sim::save::saved_section(&data) else { return -1 };
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return -2 };
        if build_section(worlds, section as usize) < 0 {
            return -2;
        }
        let (Some(sim), Some(mesh)) = (worlds.sim.as_mut(), worlds.mesh.as_mut()) else { return -2 };
        let r = match sim.load_game(&data, mesh) {
            Ok(()) => 0,
            Err(_) => -3,
        };
        // The restored Mendel stands where he was saved: start the camera
        // afresh from there, as `Camera_Init` does after a respawn.
        if let Ok(archive) = Archive::parse(&worlds.data) {
            if let (Ok(section), Some(sim)) = (archive.world(section as usize), worlds.sim.as_ref()) {
                let mut cam = camera::Camera::new(&section);
                cam.start_loaded(sim);
                worlds.camera = Some(cam);
            }
        }
        r
    })
}

/// Section index of the running simulation (-1 if none), e.g. after
/// [`game_load`].
#[no_mangle]
pub extern "C" fn game_section() -> i32 {
    WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.sim.as_ref()).map_or(-1, |s| s.section as i32))
}

/// Number of 10-float vertices of the current section's buffer (the value
/// `world_build` returned).
#[no_mangle]
pub extern "C" fn world_vertex_count() -> u32 {
    with_mesh(|m| (m.vertices.len() / world::FLOATS_PER_VERTEX) as u32, 0)
}

/// Four u32 for the time screen: level ticks, total ticks, level seconds
/// and total seconds (the seconds exactly as the original computes them:
/// `total/24 - level_start/24` and `total/24`). The level clock restarts in
/// `world_build`, so read this when `EV_SECTION` arrives, before building.
#[no_mangle]
pub extern "C" fn game_times() -> *const u32 {
    let t = WORLDS.with(|w| w.borrow().as_ref().and_then(|w| w.sim.as_ref()).map_or([0; 4], |s| s.times()));
    TIMES.with(|c| c.set(t));
    TIMES.with(|c| c.as_ptr() as *const u32)
}

/// 1 while Mendel stands on a save pad (`creature+0xaf0`): the condition
/// for the Save dialog, also checked by the original's Ctrl+S key poll (0x416e50).
#[no_mangle]
pub extern "C" fn game_on_save_pad() -> u32 {
    with_creature(|c| c.on_save_pad as u32, 0)
}

/// For "New game": forget the `Save` variables and zero both clocks of the
/// running simulation, so the next `world_build` starts from scratch
/// (`0x4172fc`: `SetTotalTicks(0)`, `SetLevelStartTicks(0)`).
#[no_mangle]
pub extern "C" fn game_reset_progress() {
    with_sim_mut(|sim| {
        sim.vars = [0; sim::save::VARS];
        sim.total_ticks = 0;
        sim.level_start_ticks = 0;
    });
}

/// Layout of the time screen (`TimeScreen_Select` @ 0x41a2d0) for `level`
/// (0..7 from the gate, 8 = game completed) in a view of `w`×`h` pixels.
/// Thirty i32:
///
/// ```text
/// 0 picture index for Archive::picture (-1: view smaller than 144x100)
/// 1 picture width  2 picture height  3 visible height (glyph strip cut off)
/// 4 glyph height (the glyph strip is the picture's bottom rows)
/// 5 level time x (left edge)  6 both times' y (top)
/// 7 total time x (right edge: the text ends here)
/// 8..29 eleven (x, width) pairs: glyphs '0'..'9' then ':' in the strip
/// ```
///
/// Times are drawn `hh:mm:ss` (`"%.2ld:%.2ld:%.2ld"`, hours mod 100) by
/// copying glyph pixels into the picture, skipping pixels equal to the
/// picture's top-left pixel (the transparent colour).
#[no_mangle]
pub extern "C" fn time_screen_layout(level: i32, w: u32, h: u32) -> *const i32 {
    let mut out = [0i32; 30];
    out[0] = -1;
    if let Some(l) = time_screen(level, w, h) {
        out[..8].copy_from_slice(&[l.0, l.1, l.2, l.2 - l.3, l.3, l.4, l.5, l.6]);
        for (k, g) in l.7.iter().enumerate() {
            out[8 + 2 * k] = g.0;
            out[9 + 2 * k] = g.1;
        }
    }
    TIME_SCREEN.with(|c| c.set(out));
    TIME_SCREEN.with(|c| c.as_ptr() as *const i32)
}

/// (picture, width, height, glyph height, level x, y, total right x,
/// glyph (x, width) table). The 8-bit picture set of the original: one
/// picture per level (index `level + 1`), +10 for the medium size, +20 for
/// the small one; the thresholds and coordinates are 0x41a2d0's.
#[allow(clippy::type_complexity)]
fn time_screen(level: i32, w: u32, h: u32) -> Option<(i32, i32, i32, i32, i32, i32, i32, [(i32, i32); 11])> {
    let n = level.clamp(0, 8) + 1;
    let digits = |x0: i32, step: i32, colon: (i32, i32)| {
        let mut t = [(0, step); 11];
        for (k, g) in t.iter_mut().take(10).enumerate() {
            g.0 = x0 + step * k as i32;
        }
        t[10] = colon;
        t
    };
    if w >= 560 && h >= 400 {
        Some((n, 560, 421, 24, 6, 358, 558, digits(0, 35, (350, 18))))
    } else if w >= 288 && h >= 200 {
        Some((n + 10, 288, 211, 12, 6, 180, 282, digits(1, 18, (181, 9))))
    } else if w >= 144 && h >= 100 {
        Some((n + 20, 144, 108, 7, 1, 92, 144, digits(0, 9, (91, 5))))
    } else {
        None
    }
}

// ——— Menus: new game, brains, pictures, preferences ———————————————————

/// File ▸ New: forget the current game and prepare a fresh brain for
/// `age` (0..=255, the Preferences slider). The next `world_build(0)`
/// starts the new game. Returns 0, or -1 if no archive is open.
#[no_mangle]
pub extern "C" fn game_new(age: u32) -> i32 {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return -1 };
        let Ok(archive) = Archive::parse(&worlds.data) else { return -1 };
        worlds.age = age.min(255) as u8;
        worlds.pending_brain = brain_for_age(&archive, worlds.age);
        worlds.sim = None; // nothing carries over
        0
    })
}

/// The section currently built (after a load, the saved one).
#[no_mangle]
pub extern "C" fn world_section() -> u32 {
    WORLDS.with(|w| w.borrow().as_ref().map_or(0, |w| w.section as u32))
}

/// Vertex count of the current section's buffers, without rebuilding.
#[no_mangle]
pub extern "C" fn world_current_vertex_count() -> i32 {
    with_mesh(|m| (m.vertices.len() / world::FLOATS_PER_VERTEX) as i32, -1)
}

thread_local! {
    static BRAIN_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// File ▸ Export Mendel NERM: serialise his current brain as a `.mnd`
/// (28,242 bytes). Returns the length; bytes at [`brain_export_ptr`].
#[no_mangle]
pub extern "C" fn brain_export() -> u32 {
    let bytes = WORLDS.with(|w| {
        let guard = w.borrow();
        guard.as_ref().and_then(|w| w.sim.as_ref()?.creature.as_ref()).map(|c| c.brain.to_bytes())
    });
    BRAIN_OUT.with(|b| {
        *b.borrow_mut() = bytes.unwrap_or_default();
        b.borrow().len() as u32
    })
}

#[no_mangle]
pub extern "C" fn brain_export_ptr() -> *const u8 {
    BRAIN_OUT.with(|b| b.borrow().as_ptr())
}

/// File ▸ Import Mendel NERM: replace his brain with a `.mnd` (takes the
/// buffer). Body, position and the world are untouched, as in the original.
/// Returns 0, or -1 if the bytes aren't a brain.
#[no_mangle]
pub extern "C" fn brain_import(ptr: *mut u8, len: usize) -> i32 {
    let data = unsafe { take(ptr, len) };
    let Ok(brain) = Brain::parse(&data, 0) else { return -1 };
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return -1 };
        match worlds.sim.as_mut().and_then(|s| s.creature.as_mut()) {
            Some(c) => c.brain = brain,
            None => worlds.pending_brain = Some(brain),
        }
        0
    })
}

thread_local! {
    static PICTURE: RefCell<(Vec<u8>, [u32; 3])> = const { RefCell::new((Vec::new(), [0; 3])) };
}

/// UI picture `i` of the archive (time screens, title strips) as opaque
/// RGBA. Returns a pointer to three u32: width, height, pointer to pixels.
#[no_mangle]
pub extern "C" fn picture_info(i: u32) -> *const u32 {
    WORLDS.with(|w| {
        let guard = w.borrow();
        let pic = guard.as_ref().and_then(|w| Archive::parse(&w.data).ok()?.picture(i as usize).ok().map(|p| {
            let rgba: Vec<u8> = p.pixels.iter().flat_map(|&c| {
                let [r, g, b] = p.palette[c as usize];
                [r, g, b, 255]
            }).collect();
            (rgba, p.width as u32, p.height as u32)
        }));
        PICTURE.with(|slot| {
            let mut slot = slot.borrow_mut();
            match pic {
                Some((rgba, wd, ht)) => {
                    slot.0 = rgba;
                    slot.1 = [wd, ht, slot.0.as_ptr() as u32];
                }
                None => slot.1 = [0; 3],
            }
        });
    });
    PICTURE.with(|slot| slot.borrow().1.as_ptr())
}

/// How many UI pictures the archive holds (39 on the retail CD).
#[no_mangle]
pub extern "C" fn picture_count() -> u32 {
    WORLDS.with(|w| w.borrow().as_ref().and_then(|w| Archive::parse(&w.data).ok()).map_or(0, |a| a.header.picture_offsets.len() as u32))
}

/// Preferences ▸ Sound ▸ Fidelity: which stored copy to play (1 = 8-bit
/// 11 kHz … 4 = CD quality). Re-decodes all three banks.
#[no_mangle]
pub extern "C" fn sound_set_quality(q: u32) {
    WORLDS.with(|w| {
        let mut guard = w.borrow_mut();
        let Some(worlds) = guard.as_mut() else { return };
        let Ok(archive) = Archive::parse(&worlds.data) else { return };
        worlds.quality = q.clamp(1, 4) as u8;
        let set = |off: u32| SoundSet::parse(&worlds.data, off as usize).ok();
        let decode = |s: Option<SoundSet>| s.map_or_else(sound::Bank::empty, |s| sound::Bank::decode(&worlds.data, &s, worlds.quality));
        let world = archive.world(worlds.section).ok().map(|s| s.sounds);
        worlds.banks = [decode(world), decode(set(archive.header.creature_sounds)), decode(set(archive.header.predator_sounds))];
    });
}

/// One byte per draw: 0 when the object is hidden by a `-5` appearance
/// frame (the exe's object visibility flag, 0x4022e0), else 1.
#[no_mangle]
pub extern "C" fn world_visible_ptr() -> *const u8 {
    with_mesh(|m| m.visible.as_ptr(), core::ptr::null())
}

/// One byte per draw: 1 when the object's glass loses its back faces too
/// (the mesh backface flag `+0x2a`, see `world::culls_glass`), else 0.
/// Textured polygons always lose theirs.
#[no_mangle]
pub extern "C" fn world_cull_ptr() -> *const u8 {
    with_mesh(|m| m.cull.as_ptr(), core::ptr::null())
}
