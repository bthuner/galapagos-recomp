//! The world simulation: 2,089 little state machines, ticking at 24 Hz.
//!
//! A port of `ObjectRegistry_TickAll` @ 0x4284f0 and friends (see
//! docs/re/objects.md). Each tick has two passes:
//!
//! 1. **update + evaluate**: every enabled object advances its running
//!    animations (motion path, appearance flip-book), then its state machine
//!    looks at the world *as it was at the start of the tick* and records
//!    what it wants to do;
//! 2. **apply**: all recorded actions are executed together.
//!
//! Nothing sees another object's change until the next tick, so the result
//! doesn't depend on the order of the object list: the same idea as double
//! buffering.
//!
//! Object ids are 1-based list indices, the camera included.
//!
//! After both passes, Mendel takes his turn (`creature.rs`): objects first,
//! then the creature, as in `Game_Tick` @ 0x41bc50. He reads the world as
//! the objects left it and posts his touch events for the *next* tick.

use crate::creature::Creature;
use crate::flash::Flash;
use crate::overlay::{self, Overlay};
use crate::predator::{Class, Predator};
use crate::world::WorldMesh;
use galapagos_formats::logic::{Actions, LogicView, PortalAction};
use galapagos_formats::worlds::{Object, Section, WorldObject};
use std::cell::Cell;

pub mod save;
mod hover;

pub const TICKS_PER_SECOND: u32 = 24;

/// Something the browser has to do on the engine's behalf.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct HostEvent {
    pub kind: u32,
    pub a: i32,
    pub b: i32,
    pub c: i32,
}

pub const EV_MUSIC: u32 = 1; // a = Smacker file 1..10, 0 = stop
pub const EV_SECTION: u32 = 2; // a = section, b = level (time screen)
pub const EV_SOUND: u32 = 3; // a = world-bank sound (0-based), b = object id, c = emitter index
pub const EV_CREATURE_SOUND: u32 = 5; // a = creature-bank sound (0-based)
pub const EV_CAMERA_SNAP: u32 = 4; // a teleporter of kind 2 asks the camera to jump
pub const EV_SAVE_PAD: u32 = 6; // a = clicked object id: the original opens its Save dialog (docs/re/saves.md)
pub const EV_GAME_COMPLETE: u32 = 7; // a = 8 (level), b = 9 (time-screen picture): "GALAPAGOS COMPLETED"
/// An affliction's sound, heard at Mendel (affliction.rs): a = world-bank
/// sound (0-based), b = pitch (16.16), c = volume (low 16 bits, 256 = full)
/// | pitch variation >> 8 (high 16 bits; 0x80 = ±0.5).
pub const EV_AFFLICTION_SOUND: u32 = 8;
/// A predator's cue (predator.rs): a = predator-bank sound (0-based: 0 =
/// the growl near Mendel, 1 = the bite), b = object id, c = pitch (16.16).
pub const EV_PREDATOR_SOUND: u32 = 9;
/// A transition effect's sound (transition.rs), heard at its owner: a =
/// world-bank sound (0-based), b = object id (0 = Mendel), c = pitch >> 4
/// (bits 0..12, 16.16 pitch) | variation >> 8 (bits 13..20) | volume >> 2
/// (bits 21..27) | 1 << 28 for distances 10000/20000 instead of 3000/30000.
pub const EV_EFFECT_SOUND: u32 = 10;

/// What a portal or save pad asks of Mendel; the creature picks these up
/// on its tick (`Game_RequestTeleport` @ 0x41c690 queues them the same way).
#[derive(Clone, Copy, Debug)]
pub enum Request {
    /// `Game_SpawnCreature` @ 0x41c590: appear at `at` facing `heading`,
    /// standing on `on` (see [`PortalPlace`]).
    /// `effect` is the materialise effect (kind, variant; `ints[5..6]`,
    /// kind 0 = none), played backwards.
    Spawn { at: [f32; 3], heading: i32, on: Option<PortalPlace>, effect: [u16; 2] },
    /// Portal kinds 1, 2 and 7. `effect` is the dematerialise effect of
    /// the portal's reaction (`Portal_InitReactions` @ 0x41b140: kind 1
    /// `0xf1000` → effect 1 v7, kind 2 `0x161000` → 1 v6, kind 7
    /// `0x151000` → 1 v5).
    Teleport { at: [f32; 3], heading: i32, snap: bool, on: Option<PortalPlace>, effect: [u16; 2] },
    /// Portal kind 6: Mendel dissolves and respawn reason 3 runs (the
    /// world snapshot comes back, he reappears at the respawn point).
    Respawn,
    /// Portal kinds 3 and 5: Mendel dissolves (effect 1 v5, the
    /// white-out), then respawn reason 4/5 ends the level: the event
    /// (`EV_SECTION` / `EV_GAME_COMPLETE`) goes to the host.
    LevelEnd(HostEvent),
}

/// Where a portal puts Mendel: a polygon of the portal (`ints[0]`) and
/// a point `(u, v, h)` in that polygon's frame, measured from its **first
/// vertex** along the polygon's own axes (`Math_FrameFromNormal` @
/// 0x4312b0; `h` along its normal), exactly the mover's surface-local
/// coordinates (`Mover_Place` @ 0x411bf0 takes `obj, poly, u, v, h,
/// heading`; `Game_SpawnCreature` @ 0x41c590 / `Game_RequestTeleport` @
/// 0x41c690 store the same six values). Checked on every pad of the CD:
/// `v0 + offset` lands on the polygon's centre (section 0's Strt #57:
/// `(383, 922) + (-387, -922)`; arrival pad #83: exactly). The heading
/// turns in the polygon's plane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PortalPlace {
    pub obj: usize,
    pub poly: usize,
    pub local: [f32; 3],
}

struct Runtime {
    obj: Option<WorldObject>,
    draw: Option<usize>,
    state: u16,
    ticks: u16,
    events: u32,
    prev_events: u32,
    pending: Option<Actions>,
    rest: [i32; 3],
    pos: [i32; 3],
    rot: [i32; 3],
    /// Expanded frames of the running motion path, and where we are in it.
    /// (frames, next frame, looping, mode): see `update_animations`.
    motion: Option<(Vec<([i32; 3], [i32; 3])>, usize, bool, u16)>,
    /// Which motion path `motion` expanded (needed to rebuild it from a
    /// snapshot or a saved game).
    motion_path: u16,
    /// The vertex morph (`ShapeAnim` at `+0xbc`): selected state (`+0xc`),
    /// frame (`+0x10`), and whether it plays (`WorldObject +0x13c`, set by
    /// `0x423320` when the state has more than one frame).
    shape: u16,
    shape_frame: u16,
    morphing: bool,
    /// Current appearance state and per-polygon flip-book frame.
    appearance: usize,
    poly_frame: Vec<usize>,
    /// Per polygon, the collision flags the exe's flag array holds
    /// (`appearance+0x14`, read by `0x409680`): see [`Sim::frame_flags`].
    flags: Vec<u32>,
    /// `WorldObject +0x13e`: the flip-book plays (some polygon of the
    /// selected state has more than one frame). Only a selection changes
    /// it (`0x423350`); the end of a one-shot does not.
    animating: bool,
    /// `WorldObject +0x14c`: the looping appearance state a one-shot goes
    /// back to when it ends (`0x423350` stores the state it replaces, if
    /// that one loops; -1 at construction, `0x422896`).
    return_appearance: Option<usize>,
    /// Registry skip count: when this folder is disabled, the update loop
    /// jumps over its `skip` descendants.
    skip: u32,
}

pub struct Sim {
    objs: Vec<Runtime>,
    picked: Option<u16>,
    next_pick: Option<u16>,
    rng: Cell<u32>,
    pub tick: u64,
    /// Debug/teaching switch: ignore folder gating so every machine runs,
    /// even far from Mendel. Not part of the original game.
    pub wake_all: bool,
    pub events: Vec<HostEvent>,
    /// Vertex range touched since the host last uploaded (first, end).
    pub dirty: Option<(u32, u32)>,
    /// Mendel, once the host gave us his body and brain.
    pub creature: Option<Creature>,
    /// Portal requests waiting for the creature's tick.
    pub requests: Vec<Request>,
    /// The Space bar is held: every `STel` object counts as clicked, the
    /// original's "skip the introduction" key (`STel` Update @ 0x41af10).
    pub skip_held: bool,

    // ——— Saves and times (sim/save.rs, docs/re/saves.md) ———
    /// The ten persistent level variables of the `Save` class (the archive
    /// object's `i32[10]` at `+0x13c`). They survive section changes, so a
    /// world remembers what was done in it.
    pub vars: [i32; save::VARS],
    /// The world as it was at the last snapshot (level start, save, open):
    /// what respawn reason 3 brings back.
    pub snapshot: Option<save::WorldState>,
    /// `Game+0x17ee84`: ticks since the game began.
    pub total_ticks: u32,
    /// `Game+0x17ee88`: `total_ticks` when the current level began.
    pub level_start_ticks: u32,
    /// Index of the section this simulation runs.
    pub section: usize,

    // ——— Predators and afflictions (predator.rs, affliction.rs) ———
    /// The class state of every `Pred`/`PBal`/`PFrk`/`PMan`/`PPol`.
    pub predators: Vec<Predator>,
    /// The section's `PPol` (`Game+0x17efc0`, set by the object factory at
    /// 0x41c29e): where `PBal` appears.
    pub pole: Option<usize>,
    /// The screen's palette flash (flash.rs).
    pub flash: Flash,
    /// The 2-D picture over the view (`Over` class, overlay.rs): world 0's
    /// "GALAPAGOS" title.
    pub overlay: Option<Overlay>,
    /// Objects whose shape frame changed (see [`Sim::take_reshaped`]).
    reshaped: Vec<usize>,
    /// Every object's pose at the start of the last tick, for the
    /// renderer's in-between frames ([`Sim::transforms_at`]).
    prev_pose: Vec<([i32; 3], [i32; 3])>,
}

impl Sim {
    /// Loads a section the way `World_Read` @ 0x4222d0 does: build every
    /// object, then enter every initial state ([`Sim::enter_initial_states`])
    /// before the first tick. `Save` variables are all zero; use
    /// [`Sim::build`] + [`Sim::enter_initial_states`] to carry them over.
    #[cfg(test)]
    pub fn new(section: &Section, mesh: &mut WorldMesh) -> Self {
        let mut sim = Self::build(section, mesh);
        sim.enter_initial_states(mesh);
        sim
    }

    /// The objects as the reader leaves them, before the load pass: every
    /// one in state 0 (the registry's state array is zeroed by
    /// `ObjectRegistry_Init` @ 0x428430) with its initial state queued
    /// (`StateMachine_SetInitialState` @ 0x42ce50).
    pub fn build(section: &Section, mesh: &WorldMesh) -> Self {
        let objs = section
            .objects
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let obj = match o {
                    Object::World(w) => Some(w.clone()),
                    Object::Camera(_) => None,
                };
                let rest = obj.as_ref().map_or([0; 3], |o| o.position);
                let mut pending = None;
                if let Some(o) = &obj {
                    if o.logic.states.is_some() {
                        // Queued, not entered: `enter_initial_states` applies it.
                        pending = Some(Actions { goto_state: Some(o.initial_state() as i16), ..Default::default() });
                    }
                }
                let flags = initial_flags(obj.as_ref());
                Runtime {
                    draw: mesh.draw_of_object[i],
                    obj,
                    state: 0,
                    ticks: 0,
                    events: 0,
                    prev_events: 0,
                    pending,
                    rest,
                    pos: rest,
                    rot: [0; 3],
                    motion: None,
                    motion_path: 0,
                    shape: 0,
                    shape_frame: 0,
                    morphing: false,
                    appearance: 0,
                    poly_frame: Vec::new(),
                    flags,
                    animating: false,
                    return_appearance: None,
                    skip: 0,
                }
            })
            .collect();
        let predators: Vec<Predator> = section
            .objects
            .iter()
            .enumerate()
            .filter_map(|(i, o)| Class::of(&o.kind()).map(|c| Predator::new(i, c)))
            .collect();
        // The factory overwrites the slot for every PPol: the last one wins.
        let pole = predators.iter().rev().find(|p| p.class == Class::Pole).map(|p| p.obj);
        Sim {
            objs,
            picked: None,
            next_pick: None,
            rng: Cell::new(1),
            tick: 0,
            wake_all: false,
            events: Vec::new(),
            dirty: None,
            creature: None,
            requests: Vec::new(),
            skip_held: false,
            vars: [0; save::VARS],
            snapshot: None,
            total_ticks: 0,
            level_start_ticks: 0,
            section: 0,
            predators,
            pole,
            flash: Flash::default(),
            overlay: None,
            reshaped: Vec::new(),
            prev_pose: Vec::new(),
        }
    }

    /// The CRT `rand()` the original links (MSVC LCG, 15-bit output).
    pub(crate) fn rand(&self) -> u32 {
        let s = self.rng.get().wrapping_mul(214013).wrapping_add(2531011);
        self.rng.set(s);
        (s >> 16) & 0x7fff
    }

    /// Registers a click on object `id`; it counts as "clicked" during the
    /// next tick only, as in the original.
    pub fn click(&mut self, id: u16) {
        self.next_pick = Some(id);
    }

    /// The level-load pass of `World_Read` @ 0x4222d0 (0x422372..0x422396):
    /// once every object is read, `StateMachine_ApplyPending` @ 0x42ce70
    /// runs on registry ids `1..=count` **in list order and without folder
    /// skipping**, so every initial state is entered — `SetState` writes it
    /// into the registry, and its entry actions (appearance, sound, motion,
    /// class trigger, timer reset) run — before `Game_Tick` ever evaluates
    /// a condition. A folder disabled by its initial state still has its
    /// whole subtree entered. Only then (`0x41cc20` → `0x41caf3`) is the
    /// level-start snapshot taken.
    pub fn enter_initial_states(&mut self, mesh: &mut WorldMesh) {
        for i in 0..self.objs.len() {
            self.apply_pending(i, mesh);
        }
    }

    /// `StateMachine_ApplyPending` @ 0x42ce70 for list index `i`.
    fn apply_pending(&mut self, i: usize, mesh: &mut WorldMesh) {
        let Some(pending) = self.objs[i].pending.take() else { return };
        let Some(sm) = self.objs[i].obj.as_ref().and_then(|w| w.logic.states.as_ref()) else { return };
        let a = sm.resolve(pending);
        self.apply(i, a, mesh);
    }

    /// The registry walk of `ObjectRegistry_TickAll` @ 0x4284f0:
    /// `next(i) = i + skip[i] + 1`, read *live*.
    fn next_enabled(&self, i: usize) -> usize {
        i + if self.wake_all { 1 } else { self.objs[i].skip as usize + 1 }
    }

    /// Visits enabled objects in registry order, honouring folder skips.
    fn enabled_ids(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.objs.len());
        let mut i = 0;
        while i < self.objs.len() {
            out.push(i);
            i = self.next_enabled(i);
        }
        out
    }

    /// Whether each object is updated this tick (not inside a disabled
    /// folder). Disabled folders still *render*: gameplay footage shows
    /// whole worlds, so folders only switch off updates (inferred: a CPU
    /// saving for areas far from Mendel).
    pub fn visible(&self) -> Vec<bool> {
        let mut v = vec![false; self.objs.len()];
        for i in self.enabled_ids() {
            v[i] = true;
        }
        v
    }

    pub fn step(&mut self, mesh: &mut WorldMesh) {
        self.prev_pose.clear();
        self.prev_pose.extend(self.objs.iter().map(|o| (o.pos, o.rot)));
        self.picked = self.next_pick.take();
        let ids = self.enabled_ids();

        // Pass 1: update running animations, then evaluate.
        for &i in &ids {
            self.update_animations(i, mesh);
            let decided = {
                let o = &self.objs[i];
                let Some(sm) = o.obj.as_ref().and_then(|w| w.logic.states.as_ref()) else { continue };
                let view = View { sim: self, i };
                sm.evaluate(o.state, &view)
            };
            let o = &mut self.objs[i];
            if let Some(a) = decided {
                o.pending.get_or_insert_with(Actions::default).overlay(&a);
            }
            o.ticks = o.ticks.wrapping_add(1);
            o.prev_events = o.events;
            o.events = 0;
        }

        // Pass 2: apply. The exe walks the registry again and reads each
        // skip count *after* applying that object (0x4284f0, second loop),
        // so a folder switching itself off here already hides its subtree
        // from this pass (their actions stay queued until it wakes up), and
        // one switching on lets its subtree apply what it had queued.
        let mut i = 0;
        while i < self.objs.len() {
            self.apply_pending(i, mesh);
            i = self.next_enabled(i);
        }

        // Predators: their class update (slot 0x28) after the base update,
        // for the enabled ones. inferred: we run them after the apply pass
        // rather than inside it; they read Mendel's last position.
        let enabled = self.visible();
        let prey = self.creature.as_ref().and_then(|c| c.prey());
        let mut predators = std::mem::take(&mut self.predators);
        for p in predators.iter_mut().filter(|p| enabled.get(p.obj).copied().unwrap_or(false)) {
            p.tick(self, mesh, prey);
        }
        self.predators = predators;
        // `Palette_Step` @ 0x437690, once per tick (0x4222ad).
        self.flash.step();

        // Mendel moves through the world the objects just left.
        if let Some(mut c) = self.creature.take() {
            c.tick(self, mesh);
            self.creature = Some(c);
        }
        self.tick += 1;
        // `Game_Tick` @ 0x41bc50 ends with `++level_ticks; ++total_ticks`.
        self.total_ticks = self.total_ticks.wrapping_add(1);
    }

    // ——— In-between frames for the renderer ———

    /// Forget the previous poses: the next frames show the current ones
    /// as they are (a level load, a snapshot coming back).
    pub fn cut_motion(&mut self) {
        self.prev_pose.clear();
    }

    /// The object matrices (WebGL column-major, as in `mesh.transforms`)
    /// a fraction `alpha` of the way from the previous tick's poses to the
    /// current ones.
    ///
    /// not in the original: the 1997 game drew once per tick, whatever the
    /// last tick left (`Game_Tick` @ 0x41bc50 passes a full tick, `0x100`,
    /// to the updates). A browser draws at the display's rate, 60 Hz or
    /// more, so without this a platform holds still for two or three
    /// frames, then jumps. The camera (`world-view.js`) and Mendel
    /// ([`crate::creature::Creature::vertices_at`]) blend with the same
    /// `alpha`, so he stays glued to the floor he rides.
    ///
    /// Angles blend the short way round (1024ths of a turn, so a fan whose
    /// path wraps from 2048 back to 0 keeps turning forwards). inferred:
    /// a move of more than 8192 units or a quarter turn in one tick is a
    /// cut (a path restarting elsewhere, a predator parked at 100000),
    /// shown as is.
    pub fn transforms_at(&self, alpha: f32, mesh: &WorldMesh, out: &mut Vec<f32>) {
        out.clear();
        out.extend_from_slice(&mesh.transforms);
        if !(0.0..1.0).contains(&alpha) || self.prev_pose.len() != self.objs.len() {
            return;
        }
        for (o, &(p0, r0)) in self.objs.iter().zip(&self.prev_pose) {
            let Some(d) = o.draw else { continue };
            if (p0, r0) == (o.pos, o.rot) {
                continue;
            }
            let dp: [i32; 3] = core::array::from_fn(|k| o.pos[k].wrapping_sub(p0[k]));
            let dr: [i32; 3] = core::array::from_fn(|k| ((o.rot[k].wrapping_sub(r0[k]) + 512) & 1023) - 512);
            if dp.iter().any(|v| v.abs() > 8192) || dr.iter().any(|v| v.abs() > 256) {
                continue;
            }
            let pos: [f32; 3] = core::array::from_fn(|k| p0[k] as f32 + dp[k] as f32 * alpha);
            let rot: [f32; 3] = core::array::from_fn(|k| r0[k] as f32 + dr[k] as f32 * alpha);
            write_matrix(&mut out[d * 16..d * 16 + 16], &transform_f(pos, rot));
        }
    }

    // ——— What the creature may see and touch ———

    /// Current position and rotation (1024ths) of object list index `i`.
    pub(crate) fn pose(&self, i: usize) -> ([i32; 3], [i32; 3]) {
        self.objs.get(i).map_or(([0; 3], [0; 3]), |o| (o.pos, o.rot))
    }

    /// Where object `i` started (`+0xd0`): the home of predators and poles.
    pub(crate) fn rest_of(&self, i: usize) -> [i32; 3] {
        self.objs.get(i).map_or([0; 3], |o| o.rest)
    }

    /// Texture of polygon `p` of object `i` in its current frame.
    pub(crate) fn frame_texture(&self, i: usize, p: usize) -> Option<i16> {
        let o = self.objs.get(i)?;
        let state = o.obj.as_ref()?.appearance.as_ref()?.get(o.appearance)?;
        let frames = state.polygons.get(p)?;
        let f = frames.get(*o.poly_frame.get(p).unwrap_or(&0)).or(frames.first())?;
        (f.texture >= 0).then_some(f.texture)
    }

    /// Moves object `i` (used to glue `MEnc` to Mendel).
    pub(crate) fn set_pose(&mut self, i: usize, pos: [i32; 3], rot: [i32; 3], mesh: &mut WorldMesh) {
        let Some(o) = self.objs.get_mut(i) else { return };
        o.pos = pos;
        o.rot = rot;
        if let Some(d) = o.draw {
            write_transform(&mut mesh.transforms[d * 16..d * 16 + 16], pos, rot);
        }
    }

    /// Flags of polygon `p` of object `i` as every collision test of the
    /// exe reads them (`0x409680` → `0x42e0f0`: the appearance's
    /// per-polygon flag array, `+0x14`). Applying a frame (`0x42df50` on a
    /// state change, `0x42e050` on a flip-book step) copies its flags into
    /// that array **only when its `enabled` byte is positive** (signed
    /// char); a disabled frame leaves the polygon what it was. On the CD
    /// every disabled frame carries flags 0: the arrival pad #83 shows
    /// `[e1 0x2, e0 0x0, e0 0x0]` and stays a floor; section 1's #163 has
    /// states whose frames are all disabled, and keeps its floor there too.
    pub(crate) fn frame_flags(&self, i: usize, p: usize) -> u32 {
        self.objs.get(i).and_then(|o| o.flags.get(p).copied()).unwrap_or(0)
    }

    /// The texture offset step of polygon `p` this tick, in texels
    /// `(du, dv)`: what `0x41dc30` reads from the polygon's scroll record
    /// (`+0x1d0`, 16 bytes: current and previous offset), each wrapped like
    /// the exe: `(cur & 0x7f) − (prev & 0x7f)` folded into `−64..64`.
    ///
    /// The record (observed): built zeroed (`0x41d9e0`), so a polygon whose
    /// appearance was never applied does not move him; every frame applied
    /// (`0x423730` on a state change, `0x423810` per flip-book step) goes
    /// through slot 0x14 `0x41daf0`, which **resets** it (prev = cur = the
    /// new frame's offset) when the texture number changes, then slot 0x18
    /// `0x41dbc0`: prev = cur, cur = the frame's `(u, v)`. Flip-books step
    /// once per tick (`Game_Tick` → `0x4284f0` → object slot 0x28 →
    /// `0x423420` → `0x423810`; the record takes the frame's own offset,
    /// not the sub-tick interpolated one), so the step is the difference
    /// of consecutive frames. A flip-book that does not step (an object in
    /// a switched-off folder) keeps its last step. inferred: a one-shot
    /// with no state to go back to, on its rest tick at the wrap (frame 0,
    /// see `update_animations`), is read as no step here.
    /// inferred: on the first tick of a state, "previous" is taken as the
    /// list's last frame (looping) instead of the old state's frame, which
    /// would need the record saved with the game.
    pub(crate) fn uv_step(&self, i: usize, p: usize) -> (i32, i32) {
        let Some(o) = self.objs.get(i) else { return (0, 0) };
        let Some(state) = o.obj.as_ref().and_then(|w| w.appearance.as_ref()).and_then(|s| s.get(o.appearance)) else {
            return (0, 0);
        };
        // Never applied: the record is still the zeros 0x41d9e0 wrote.
        let Some(&at) = o.poly_frame.get(p) else { return (0, 0) };
        let Some(frames) = state.polygons.get(p).filter(|f| f.len() > 1) else { return (0, 0) };
        let looping = state.looping != 0;
        let at = at.min(frames.len() - 1);
        let prev = match at {
            0 if looping => frames.len() - 1,
            0 => return (0, 0),
            k => k - 1,
        };
        // 0x41daf0: a new texture number resets the record.
        if frames[at].texture >= 0 && Some(frames[at].texture) != last_texture(frames, prev, looping) {
            return (0, 0);
        }
        let fold = |a: i16, b: i16| {
            let d = (a as i32 & 0x7f) - (b as i32 & 0x7f);
            if d > 0x40 { d - 0x80 } else if d < -0x40 { d + 0x80 } else { d }
        };
        (fold(frames[at].u, frames[prev].u), fold(frames[at].v, frames[prev].v))
    }

    /// `WorldObject_PostEvent` @ 0x4232f0: OR `bits` into the object's
    /// events and forward them up the folder chain, so a folder "feels"
    /// Mendel touching any of its descendants.
    pub(crate) fn post_event(&mut self, i: usize, bits: u32) {
        let mut at = Some(i);
        let mut guard = 0;
        while let Some(k) = at {
            let Some(o) = self.objs.get_mut(k) else { break };
            o.events |= bits;
            let parent = o.obj.as_ref().map_or(-1, |w| w.folder);
            at = (parent >= 1).then(|| parent as usize - 1);
            guard += 1;
            if guard > self.objs.len() {
                break; // a malformed cycle; none on the CD
            }
        }
    }

    pub(crate) fn mark_dirty(&mut self, first: u32, end: u32) {
        mark(&mut self.dirty, first, end);
    }

    fn apply(&mut self, i: usize, a: Actions, mesh: &mut WorldMesh) {
        if let Some(s) = a.goto_state {
            // `CStateMachine_SetState` @ 0x42cfd0 (slot 0x04, shared by every
            // class) stores the state, its record and flag and the registry
            // entry; it does *not* touch the tick counter (`+0x2c`): only
            // the reset flag below does.
            self.objs[i].state = s.max(0) as u16;
        }
        if let Some(n) = a.shape {
            self.select_shape(i, (n - 1).max(0) as usize, mesh);
        }
        if let Some(n) = a.appearance {
            self.select_appearance(i, (n - 1).max(0) as usize, mesh);
        }
        if let Some(n) = a.sound {
            // An emitter lists up to three alternative sounds; the sound
            // system picks one at random. Their indices are **1-based** into
            // the section's sound set (checked on the CD: 1-based leaves 1 of
            // 1,467 references on an empty slot, 0-based leaves 107).
            let e = (n - 1).max(0) as usize;
            let emitter = self.objs[i].obj.as_ref().and_then(|o| o.logic.sounds.get(e));
            if let Some(em) = emitter {
                let choices: Vec<i16> = em.sounds.iter().copied().filter(|&s| s > 0).collect();
                if !choices.is_empty() {
                    let s = choices[self.rand() as usize % choices.len()];
                    self.events.push(HostEvent { kind: EV_SOUND, a: s as i32 - 1, b: i as i32 + 1, c: e as i32 });
                }
            }
        }
        if let Some(n) = a.motion {
            self.start_motion(i, (n - 1).max(0) as usize);
        }
        if let Some(n) = a.trigger {
            self.trigger(i, (n - 1).max(0) as usize, mesh);
        }
        if a.reset_timer {
            self.objs[i].ticks = 0;
        }
    }

    /// The world point (on the polygon plane plus `h` along its normal) a
    /// portal offset designates, and the polygon place itself. Falls back
    /// to `position + offset` (no polygon) if the polygon is missing.
    fn portal_place(&self, i: usize, poly: usize, off: [i32; 3]) -> ([f32; 3], Option<PortalPlace>) {
        let o = self.objs[i].pos;
        let fallback = ([(o[0] + off[0]) as f32, (o[1] + off[1]) as f32, (o[2] + off[2]) as f32], None);
        let Some(mesh) = self.objs[i].obj.as_ref().and_then(|w| w.mesh.as_ref()) else { return fallback };
        let Some(pts) = mesh.polygons.get(poly).filter(|p| p.len() >= 3) else { return fallback };
        let verts = self.vertices_of(i).unwrap_or(&mesh.vertices);
        let v = |k: usize| verts.get(pts[k] as usize).map(|v| v.map(|c| c as f32));
        let (Some(a), Some(b), Some(c)) = (v(0), v(1), v(2)) else { return fallback };
        let sub = |p: [f32; 3], q: [f32; 3]| [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
        let (e1, e2) = (sub(b, a), sub(c, a));
        let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if l == 0.0 {
            return fallback;
        }
        let f = frame_from_normal(n.map(|x| x / l));
        let local = off.map(|x| x as f32);
        let d = [
            f[0] * local[0] + f[1] * local[1] + f[2] * local[2],
            f[3] * local[0] + f[4] * local[1] + f[5] * local[2],
            f[6] * local[0] + f[7] * local[1] + f[8] * local[2],
        ];
        let m = transform(o, self.objs[i].rot);
        let at = apply(&m, [a[0] + d[0], a[1] + d[1], a[2] + d[2]]);
        (at, Some(PortalPlace { obj: i, poly, local }))
    }

    /// The class behaviour (vtable slot 0x18), for the classes we know.
    /// A gate or the last portal: Mendel dissolves first (creature.rs
    /// sends `ev` when his effect ends); without him, at once.
    fn level_end(&mut self, ev: HostEvent) {
        if self.creature.is_some() {
            self.requests.push(Request::LevelEnd(ev));
        } else {
            self.events.push(ev);
        }
    }

    fn trigger(&mut self, i: usize, entry: usize, mesh: &mut WorldMesh) {
        let Some(obj) = self.objs[i].obj.as_ref() else { return };
        let Some(p) = obj.logic.params.get(entry).copied() else { return };
        if let Some(k) = self.predators.iter().position(|q| q.obj == i) {
            let mut q = self.predators[k].clone();
            q.trigger(p.shorts, p.ints, self, mesh);
            self.predators[k] = q;
            return;
        }
        match &obj.kind {
            b"Fldr" => {
                // shorts[0]: 1 = enable, 0 = disable; shorts[1] = descendants.
                self.objs[i].skip = if p.shorts[0] != 0 { 0 } else { p.shorts[1].max(0) as u32 };
            }
            b"Voic" => self.events.push(HostEvent { kind: EV_MUSIC, a: p.shorts[0] as i32, b: 0, c: 0 }),
            // A picture over the view (overlay.rs): show (and maybe flash
            // white, script 0 at priority 250) or take it down.
            b"Over" => {
                let (o, flash) = overlay::trigger(p.shorts);
                self.overlay = o;
                if flash {
                    self.flash.play(0, 250);
                }
            }
            b"Tele" | b"Strt" | b"STel" => {
                let place = |off: [i32; 3], mode: i32| self.portal_place(i, mode.max(0) as usize, off);
                match p.portal() {
                    // Kind 3's reaction 0x251000 (and kind 5's 0x2d1000)
                    // dissolve Mendel with effect 1 v5 (the white-out)
                    // before respawn reason 4 (5) ends the level.
                    PortalAction::ChangeSection { section, level } => {
                        self.level_end(HostEvent { kind: EV_SECTION, a: section as i32, b: level as i32, c: 0 })
                    }
                    // `mode` (ints[0]) is the polygon he lands on; the
                    // offset is measured on it (see `PortalPlace`).
                    PortalAction::Spawn { offset, heading, mode, extra } => {
                        let (at, on) = place(offset, mode);
                        let effect = [extra[0] as u16, extra[1] as u16];
                        self.requests.push(Request::Spawn { at, heading, on, effect })
                    }
                    PortalAction::Teleport { offset, heading, snap_camera, mode } => {
                        let (at, on) = place(offset, mode);
                        let effect = match p.shorts[0] {
                            2 => [1, 6],
                            7 => [1, 5],
                            _ => [1, 7],
                        };
                        self.requests.push(Request::Teleport { at, heading, snap: snap_camera, on, effect })
                    }
                    // Kinds 5 and 6 only differ by the reaction they hand
                    // Mendel (`Portal_InitReactions` @ 0x41b140): kind 5's
                    // 0x2d1000 ends in respawn reason 5, the end of the
                    // game; kind 6's 0x1b2000 in reason 3, a respawn.
                    PortalAction::ReactOnly if p.shorts[0] == 5 => {
                        self.level_end(HostEvent { kind: EV_GAME_COMPLETE, a: 8, b: 9, c: 0 })
                    }
                    PortalAction::ReactOnly if p.shorts[0] == 6 => self.requests.push(Request::Respawn),
                    PortalAction::ReactOnly | PortalAction::Other(_) => {}
                }
            }
            // Not the save pads! `Save` trigger @ 0x41afe0 is a persistent
            // variable: `shorts[0] = 1` stores this object's state in slot
            // `shorts[1]` (`0x41ce90` -> archive slot 0x14 @ 0x41d530);
            // `0` reads the slot back and queues it as the next state
            // (`0x41ceb0` -> slot 0x10 @ 0x41d560, then 0x42ce50).
            b"Save" => {
                let slot = p.shorts[1];
                if !(0..save::VARS as i16).contains(&slot) {
                    return;
                }
                if p.shorts[0] != 0 {
                    self.vars[slot as usize] = self.objs[i].state as i32;
                } else {
                    let s = self.vars[slot as usize] as i16;
                    self.objs[i].pending = Some(Actions { goto_state: Some(s), ..Default::default() });
                }
            }
            _ => {}
        }
    }

    /// not in the original: send object `i`'s state machine to `state` on
    /// the next tick, entry actions included.
    pub fn debug_goto(&mut self, i: usize, state: u16) {
        if let Some(o) = self.objs.get_mut(i) {
            o.pending = Some(Actions { goto_state: Some(state as i16), ..Default::default() });
        }
    }

    /// not in the original: run object `i`'s class trigger by hand.
    pub fn debug_trigger(&mut self, i: usize, entry: usize, mesh: &mut WorldMesh) {
        if i < self.objs.len() {
            self.trigger(i, entry, mesh);
        }
    }

    fn start_motion(&mut self, i: usize, n: usize) {
        let o = &mut self.objs[i];
        let Some(path) = o.obj.as_ref().and_then(|w| w.logic.motions.get(n)) else { return };
        o.motion = Some((path.frames(), 0, path.looping != 0, path.relative));
        o.motion_path = n as u16;
    }

    /// Shape action (slot 0x08 `0x423320`): `ShapeAnim_SelectState` @
    /// 0x42adc0 (state `n`, frame 0; plays if it has more than one frame,
    /// `+0x13c`), then slot 0x70 `0x423530` puts frame 0 on the mesh.
    fn select_shape(&mut self, i: usize, n: usize, mesh: &mut WorldMesh) {
        let o = &mut self.objs[i];
        let Some(frames) = o.obj.as_ref().and_then(|w| w.logic.shape.as_ref()).and_then(|s| s.states.get(n)).map(|s| s.frames.len()) else {
            return;
        };
        o.shape = n as u16;
        o.shape_frame = 0;
        o.morphing = frames > 1;
        self.put_shape(i, mesh);
    }

    /// Writes object `i`'s current shape frame into the render buffers
    /// and tells Mendel his collision polygons of it changed.
    fn put_shape(&mut self, i: usize, mesh: &mut WorldMesh) {
        let o = &self.objs[i];
        let (Some(obj), Some(draw)) = (o.obj.as_ref(), o.draw) else { return };
        let (Some(m), Some(f)) = (obj.mesh.as_ref(), self.shape_frame_of(i)) else { return };
        if let Some(r) = mesh.set_shape(draw, &m.polygons, &f.vertices, &f.polygon_mapping) {
            mark(&mut self.dirty, r.first, r.first + r.count);
        }
        if !self.reshaped.contains(&i) {
            self.reshaped.push(i);
        }
    }

    /// The shape frame object `i` is in (`0x423130`: what the mover and
    /// the world casts read as its polygons), if it has a `ShapeAnim`.
    pub(crate) fn shape_frame_of(&self, i: usize) -> Option<&galapagos_formats::logic::ShapeFrame> {
        let o = self.objs.get(i)?;
        o.obj.as_ref()?.logic.shape.as_ref()?.states.get(o.shape as usize)?.frames.get(o.shape_frame as usize)
    }

    /// Object `i`'s current local vertices: its shape frame, or the mesh.
    pub(crate) fn vertices_of(&self, i: usize) -> Option<&[[i16; 3]]> {
        self.shape_frame_of(i).map(|f| &f.vertices[..]).or_else(|| self.objs.get(i)?.obj.as_ref()?.mesh.as_ref().map(|m| &m.vertices[..]))
    }

    /// Object `i`'s mesh (polygon index lists), if it has one.
    pub(crate) fn mesh_of(&self, i: usize) -> Option<&galapagos_formats::worlds::Mesh> {
        self.objs.get(i)?.obj.as_ref()?.mesh.as_ref()
    }

    /// Objects whose shape frame changed since the last call (Mendel
    /// rebuilds his collision polygons of them).
    pub(crate) fn take_reshaped(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.reshaped)
    }

    fn select_appearance(&mut self, i: usize, n: usize, mesh: &mut WorldMesh) {
        let o = &mut self.objs[i];
        let (Some(obj), Some(draw)) = (o.obj.as_ref(), o.draw) else { return };
        let Some(state) = obj.appearance.as_ref().and_then(|s| s.get(n)) else { return };
        // `0x423350`: if the state being replaced loops (`0x42e0d0`, 0
        // while none was ever selected: the appearance's `+0xc` starts at
        // -1, `0x42de50`), remember it (`+0x14c`): a one-shot selected now
        // goes back to it when it ends (see `update_animations`).
        let replaced = obj.appearance.as_ref().and_then(|s| s.get(o.appearance));
        if !o.poly_frame.is_empty() && replaced.is_some_and(|r| r.looping != 0) {
            o.return_appearance = Some(o.appearance);
        }
        o.appearance = n;
        o.poly_frame = vec![0; state.polygons.len()];
        o.animating = state.polygons.iter().any(|f| f.len() > 1);
        o.flags.resize(state.polygons.len(), 0);
        for (p, frames) in state.polygons.iter().enumerate() {
            if let Some(f) = frames.first().filter(|f| (f.enabled as i8) > 0) {
                o.flags[p] = f.flags;
            }
            if let Some(r) = mesh.apply_frame(draw, p, frames, 0) {
                mark(&mut self.dirty, r.first, r.first + r.count);
            }
        }
    }

    fn update_animations(&mut self, i: usize, mesh: &mut WorldMesh) {
        // Vertex morph (`0x423420` step 3 → slot 0x74 `0x4235d0`): the
        // next frame, wrapping to 0 (`0x42ae50`: a playing state loops
        // until another shape action). `Game_Tick` passes a full tick
        // (0x100), so the frame is taken as is, no blend.
        if self.objs[i].morphing {
            let n = self.objs[i].obj.as_ref().and_then(|w| w.logic.shape.as_ref()).and_then(|s| s.states.get(self.objs[i].shape as usize)).map_or(1, |s| s.frames.len());
            let o = &mut self.objs[i];
            o.shape_frame = if o.shape_frame as usize + 1 >= n { 0 } else { o.shape_frame + 1 };
            self.put_shape(i, mesh);
        }
        let o = &mut self.objs[i];

        // Motion path: one expanded frame per tick.
        // `WorldObject_StepMotion` @ 0x423a30 hands each frame to one of
        // three pose setters by the path's mode (`path+2`): 0 → 0x422e80
        // *adds* the frame to the current pose (a nudge: section 1's pads
        // bounce ±100 this way), 1 → 0x422cb0 rest position + frame, the
        // frame's rotation as is, 2 → 0x422af0 the frame as is.
        if let Some((frames, at, looping, mode)) = &mut o.motion {
            if let Some(&(p, r)) = frames.get(*at) {
                let add = |a: [i32; 3], b: [i32; 3]| [a[0] + b[0], a[1] + b[1], a[2] + b[2]];
                (o.pos, o.rot) = match *mode {
                    0 => (add(o.pos, p), add(o.rot, r)),
                    1 => (add(o.rest, p), r),
                    _ => (p, r),
                };
                if let Some(d) = o.draw {
                    write_transform(&mut mesh.transforms[d * 16..d * 16 + 16], o.pos, o.rot);
                }
            }
            *at += 1;
            if *at >= frames.len() {
                if *looping { *at = 0 } else { o.motion = None }
            }
        }

        // Appearance flip-book (`0x423420` step 4 → slot 0x7c `0x423810`):
        // polygon by polygon, `0x42e050` advances the frame and wraps it
        // to 0; in a state that does not loop, the wrap *is the end*: it
        // returns NULL before touching the polygon, `0x423810` stops the
        // polygon loop there (later polygons are not stepped this tick)
        // and, if a looping state was replaced by this one-shot (`+0x14c
        // >= 0`), selects it again (`0x423976` → slot 0x0c `0x423350`),
        // which puts back its frame 0: textures and collision flags. So a
        // one-shot of n frames shows for n ticks, then the object is what
        // it was. Without a state to go back to, `+0x13e` stays set and
        // the one-shot keeps cycling through frames 1..n-1, a tick of
        // rest at each wrap (s1 #236). Section 0's #110 turns its floor
        // into a bouncing wall (`0x2` → `0x510`) for 6 ticks this way:
        // Mendel is thrown up once and lands on a floor again.
        if o.animating {
            let (Some(obj), Some(draw)) = (o.obj.as_ref(), o.draw) else { return };
            let Some(state) = obj.appearance.as_ref().and_then(|s| s.get(o.appearance)) else { return };
            let looping = state.looping != 0;
            let mut ended = false;
            for (p, frames) in state.polygons.iter().enumerate() {
                let Some(f) = o.poly_frame.get_mut(p) else { break };
                let was = *f;
                *f += 1;
                if *f >= frames.len() {
                    *f = 0;
                }
                if !looping && *f == 0 {
                    ended = true;
                    break;
                }
                if *f == was {
                    // A looping single frame: re-applying it changes nothing.
                    continue;
                }
                if let (Some(fl), Some(fr)) = (o.flags.get_mut(p), frames.get(*f).filter(|fr| (fr.enabled as i8) > 0)) {
                    *fl = fr.flags;
                }
                if let Some(r) = mesh.apply_frame(draw, p, frames, *f) {
                    mark(&mut self.dirty, r.first, r.first + r.count);
                }
            }
            if ended {
                if let Some(back) = o.return_appearance {
                    self.select_appearance(i, back, mesh);
                }
            }
        }
    }

    /// Ray pick against every visible polygon whose flags are pickable
    /// (flags & 0xfffffffe), like `World_Pick` @ 0x4089e0.
    /// Returns the 1-based id of the nearest hit.
    #[cfg(test)]
    pub fn pick(&self, origin: [f32; 3], dir: [f32; 3]) -> Option<u16> {
        self.pick_hit(origin, dir).map(|(id, _)| id)
    }

    /// The nearest hit (as `pick`) with the hit polygon's frame flags.
    pub fn pick_hit(&self, origin: [f32; 3], dir: [f32; 3]) -> Option<(u16, u32)> {
        let visible = self.visible();
        let mut best: Option<(f32, u16, u32)> = None;
        for (i, o) in self.objs.iter().enumerate() {
            let (Some(obj), Some(_)) = (o.obj.as_ref(), o.draw) else { continue };
            if !visible[i] {
                continue;
            }
            let Some(m) = obj.mesh.as_ref() else { continue };
            let verts = self.vertices_of(i).unwrap_or(&m.vertices);
            let t = transform(o.pos, o.rot);
            for (p, poly) in m.polygons.iter().enumerate() {
                // The polygon's flag-array entry (`0x408730` → `0x409680`),
                // not its current flip-book frame: a disabled frame keeps
                // the flags the last enabled one left.
                let flags = o.flags.get(p).copied().unwrap_or(0);
                if flags & 0xffff_fffe == 0 {
                    continue;
                }
                let pts: Vec<[f32; 3]> = poly
                    .iter()
                    .filter_map(|&k| verts.get(k as usize))
                    .map(|v| apply(&t, [v[0] as f32, v[1] as f32, v[2] as f32]))
                    .collect();
                // The exe tests the hit point against the polygon's own edges
                // (`0x4076d0` → `0x407830`, a crossing count), so a ring's
                // hole is a hole: the renderer's triangles cover exactly that
                // area (`world::triangulate`), a fan from vertex 0 does not.
                for [a, b, c] in crate::world::triangulate(&pts) {
                    if let Some(d) = ray_triangle(origin, dir, pts[a], pts[b], pts[c]) {
                        if best.is_none_or(|(bd, _, _)| d < bd) {
                            best = Some((d, i as u16 + 1, flags));
                        }
                    }
                }
            }
        }
        best.map(|(_, id, flags)| (id, flags))
    }

    /// Sound emitter `n` of object `id` (1-based).
    pub fn emitter(&self, id: u16, n: usize) -> Option<galapagos_formats::logic::SoundEmitter> {
        self.objs.get(id.wrapping_sub(1) as usize)?.obj.as_ref()?.logic.sounds.get(n).copied()
    }

    /// Current position of object `id` (1-based), in world units.
    pub fn position_of(&self, id: u16) -> [i32; 3] {
        self.objs.get(id.wrapping_sub(1) as usize).map_or([0; 3], |o| o.pos)
    }

    pub fn state_of(&self, id: u16) -> u16 {
        self.objs.get(id.wrapping_sub(1) as usize).map_or(0, |o| o.state)
    }
}

fn mark(dirty: &mut Option<(u32, u32)>, a: u32, b: u32) {
    *dirty = Some(match *dirty {
        Some((x, y)) => (x.min(a), y.max(b)),
        None => (a, b),
    });
}

/// What one object's conditions can see (the `LogicView` of logic.rs).
struct View<'a> {
    sim: &'a Sim,
    i: usize,
}

impl LogicView for View<'_> {
    fn clicked(&self) -> bool {
        let skip = self.sim.skip_held && self.sim.objs[self.i].obj.as_ref().is_some_and(|w| &w.kind == b"STel");
        skip || self.sim.picked == Some(self.i as u16 + 1)
    }
    fn ticks(&self) -> u16 {
        self.sim.objs[self.i].ticks
    }
    fn events(&self) -> u32 {
        self.sim.objs[self.i].events
    }
    fn previous_events(&self) -> u32 {
        self.sim.objs[self.i].prev_events
    }
    fn random(&self) -> u32 {
        self.sim.rand()
    }
    fn position(&self) -> [i32; 3] {
        self.sim.objs[self.i].pos
    }
    fn rotation(&self) -> [i16; 3] {
        self.sim.objs[self.i].rot.map(|r| r as i16)
    }
    fn state_of(&self, id: u16) -> u16 {
        self.sim.state_of(id)
    }
}

// ——— transforms ————————————————————————————————————————————————————————
// Rotations are in 1024ths of a turn, kept in the order of the motion-path
// keys `(rx, ry, rz)`. The exe stores them crosswise: key `rx` goes to the
// object's `+0xf2`, `ry` to `+0xf0`, `rz` to `+0xf4` (the pose setters,
// e.g. 0x422e80 at 0x422f36..0x422f68), and the object matrix is
// `Math_RotationMatrix` @ 0x430ce0 of (`+0xf2`, `+0xf0`, `+0xf4`)
// (`Object_UpdateMatrices` @ 0x4096a0), applied as `v · M` (0x409880).
// So `rx` is a yaw about z, `ry` a pitch about x and `rz` a roll about y.

pub(crate) type M = [f32; 12]; // 3×3 rotation (row-major) + translation

/// The object transform: `world = R · local + pos`, with `R = Mᵀ` and
/// `M = Math_RotationMatrix(yaw = rot[0], pitch = rot[1], roll = rot[2])`
/// (same formula as `camera::Trig::rot`, in floating point).
pub(crate) fn transform(pos: [i32; 3], rot: [i32; 3]) -> M {
    transform_f(pos.map(|v| v as f32), rot.map(|v| v as f32))
}

/// [`transform`] for an in-between pose ([`Sim::transforms_at`]).
fn transform_f(pos: [f32; 3], rot: [f32; 3]) -> M {
    let sc = |r: f32| {
        let a = -r * core::f32::consts::TAU / 1024.0;
        a.sin_cos()
    };
    let (sa, ca) = sc(rot[0]);
    let (sb, cb) = sc(rot[1]);
    let (scc, cc) = sc(rot[2]);
    let m = [
        cc * ca - sb * sa * scc,
        -(sb * ca * scc + cc * sa),
        -scc * cb,
        cb * sa,
        cb * ca,
        -sb,
        cc * sa * sb + scc * ca,
        cc * ca * sb - scc * sa,
        cc * cb,
    ];
    [m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8], pos[0], pos[1], pos[2]]
}

/// The flag array of a freshly loaded object: its first appearance
/// state's first frames (disabled ones leave 0).
fn initial_flags(o: Option<&WorldObject>) -> Vec<u32> {
    let Some(state) = o.and_then(|w| w.appearance.as_ref()).and_then(|s| s.first()) else { return Vec::new() };
    state.polygons.iter().map(|f| f.first().filter(|f| (f.enabled as i8) > 0).map_or(0, |f| f.flags)).collect()
}

/// The texture number polygon frames up to `at` left set (the last frame
/// with a texture ≥ 0; cyclically if the state loops): `-2`/`-3`/`-4`/`-5`
/// frames do not touch it (0x423730, 0x423810).
fn last_texture(frames: &[galapagos_formats::worlds::PolyFrame], at: usize, looping: bool) -> Option<i16> {
    let wrap = (at + 1..frames.len()).rev().filter(move |_| looping);
    (0..=at.min(frames.len().saturating_sub(1))).rev().chain(wrap).map(|k| frames[k].texture).find(|&t| t >= 0)
}

/// The flags the last enabled frame up to `at` left (cyclically, if the
/// state loops); `None` if no frame of the list is enabled.
pub(crate) fn last_enabled_flags(frames: &[galapagos_formats::worlds::PolyFrame], at: usize, looping: bool) -> Option<u32> {
    if frames.is_empty() {
        return None;
    }
    let at = at.min(frames.len() - 1);
    let wrap = (at + 1..frames.len()).rev().filter(|_| looping);
    (0..=at).rev().chain(wrap).find(|&k| (frames[k].enabled as i8) > 0).map(|k| frames[k].flags)
}

/// `Math_FrameFromNormal` @ 0x4312b0 in floating point: the rotation
/// (row-major) taking +z to the unit normal `n`, about `z × n`. For a
/// floor facing +z it is the identity: surface axes = world axes.
pub(crate) fn frame_from_normal(n: [f32; 3]) -> [f32; 9] {
    let [x, y, z] = n;
    let hl = (x * x + y * y).sqrt();
    let (c, s) = (z, hl);
    let (ux, uy) = if hl == 0.0 { (0.0, 1.0) } else { (x / hl, y / hl) };
    [
        c * ux * ux + uy * uy, (c - 1.0) * ux * uy, s * ux,
        (c - 1.0) * ux * uy, c * uy * uy + ux * ux, s * uy,
        -s * ux, -s * uy, c,
    ]
}

pub(crate) fn apply(m: &M, v: [f32; 3]) -> [f32; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2] + m[9],
        m[3] * v[0] + m[4] * v[1] + m[5] * v[2] + m[10],
        m[6] * v[0] + m[7] * v[1] + m[8] * v[2] + m[11],
    ]
}

/// Writes a column-major 4×4 for WebGL.
fn write_transform(out: &mut [f32], pos: [i32; 3], rot: [i32; 3]) {
    write_matrix(out, &transform(pos, rot));
}

fn write_matrix(out: &mut [f32], m: &M) {
    out.copy_from_slice(&[
        m[0], m[3], m[6], 0.0, m[1], m[4], m[7], 0.0, m[2], m[5], m[8], 0.0, m[9], m[10], m[11], 1.0,
    ]);
}

/// Möller–Trumbore, two-sided. Returns the distance along `dir`.
fn ray_triangle(o: [f32; 3], d: [f32; 3], a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<f32> {
    let sub = |p: [f32; 3], q: [f32; 3]| [p[0] - q[0], p[1] - q[1], p[2] - q[2]];
    let cross = |p: [f32; 3], q: [f32; 3]| [p[1] * q[2] - p[2] * q[1], p[2] * q[0] - p[0] * q[2], p[0] * q[1] - p[1] * q[0]];
    let dot = |p: [f32; 3], q: [f32; 3]| p[0] * q[0] + p[1] * q[1] + p[2] * q[2];
    let (e1, e2) = (sub(b, a), sub(c, a));
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-9 {
        return None;
    }
    let inv = 1.0 / det;
    let t = sub(o, a);
    let u = dot(t, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(t, e1);
    let v = dot(d, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = dot(e2, q) * inv;
    (dist > 0.0).then_some(dist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use galapagos_formats::worlds::Archive;

    #[test]
    fn ray_hits_triangle() {
        let d = ray_triangle([0.2, 0.2, -5.0], [0.0, 0.0, 1.0], [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        assert_eq!(d, Some(5.0));
    }

    /// Runs every section for 30 simulated seconds: nothing may panic, and
    /// something must move or change (the worlds are alive).
    #[test]
    fn every_section_runs_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        for s in 0..archive.sections.len() {
            let section = archive.world(s).unwrap();
            let mut mesh = WorldMesh::build(&section);
            let before = mesh.transforms.clone();
            let mut sim = Sim::new(&section, &mut mesh);
            for _ in 0..30 * TICKS_PER_SECOND {
                sim.step(&mut mesh);
            }
            // Objects (draws) whose transform changed at all.
            let moved = before.chunks(16).zip(mesh.transforms.chunks(16)).filter(|(a, b)| a != b).count();
            let changed: usize = sim.objs.iter().filter(|o| o.state != o.obj.as_ref().map_or(0, |w| w.initial_state())).count();
            println!("section {s}: {moved} objects moved, {changed} changed state, {} host events", sim.events.len());
            assert!(moved + changed > 0, "section {s} is static");
        }
    }
}

#[cfg(test)]
mod pick_tests {
    use super::*;
    use galapagos_formats::worlds::Archive;

    /// Aims a ray at the centre of the first pickable polygon of section 0.
    #[test]
    fn picks_a_pickable_polygon_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        let section = archive.world(0).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mut mesh);
        // After ~2 s most folders have switched off (they wait for Mendel),
        // so pick right after the first tick, as the game would at load.
        sim.step(&mut mesh);
        let mut pickable = 0;
        let mut target = None;
        let enabled = sim.visible();
        for (i, o) in sim.objs.iter().enumerate() {
            if !enabled[i] {
                continue;
            }
            let (Some(w), Some(m)) = (o.obj.as_ref(), o.obj.as_ref().and_then(|w| w.mesh.as_ref())) else { continue };
            let Some(st) = w.appearance.as_ref().and_then(|s| s.get(o.appearance)) else { continue };
            for (p, poly) in m.polygons.iter().enumerate() {
                let Some(f) = st.polygons.get(p).and_then(|f| f.first()) else { continue };
                if f.flags & 0xffff_fffe != 0 && f.enabled != 0 && poly.len() >= 3 {
                    pickable += 1;
                    if target.is_none() {
                        let c = poly.iter().map(|&k| m.vertices[k as usize]).fold([0f32; 3], |a, v| {
                            [a[0] + v[0] as f32, a[1] + v[1] as f32, a[2] + v[2] as f32]
                        });
                        let n = poly.len() as f32;
                        let c = [c[0] / n + o.pos[0] as f32, c[1] / n + o.pos[1] as f32, c[2] / n + o.pos[2] as f32];
                        target = Some((i + 1, c));
                    }
                }
            }
        }
        println!("pickable polygons in section 0: {pickable}");
        let (id, c) = target.expect("no pickable polygon");
        // Shoot along each axis towards the centre; one of them must hit.
        let hits: Vec<_> = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            .iter()
            .flat_map(|&d: &[f32; 3]| {
                [1.0f32, -1.0].map(|s| {
                    let dir = d.map(|x| x * s);
                    let o = [c[0] - dir[0] * 50000.0, c[1] - dir[1] * 50000.0, c[2] - dir[2] * 50000.0];
                    sim.pick(o, dir)
                })
            })
            .collect();
        println!("target object #{id}, hits: {hits:?}");
        // Which enabled objects have a transition that tests "clicked"?
        let clickable: Vec<_> = sim
            .objs
            .iter()
            .enumerate()
            .filter(|(i, o)| {
                enabled[*i]
                    && o.obj.as_ref().and_then(|w| w.logic.states.as_ref()).is_some_and(|sm| {
                        sm.states.iter().any(|st| {
                            st.transitions.iter().any(|t| t.groups.iter().flatten().any(|c| format!("{c:?}").starts_with("Clicked")))
                        })
                    })
            })
            .map(|(i, o)| (i + 1, String::from_utf8_lossy(&o.obj.as_ref().unwrap().kind).to_string()))
            .collect();
        println!("enabled objects with a click transition: {clickable:?}");
        assert!(hits.iter().any(|h| h.is_some()));
    }
}


#[cfg(test)]
mod transform_tests {
    use super::*;

    /// The exe's convention (0x430ce0 applied as `v · M`): key `rx` turns
    /// about z, `ry` about x, `rz` about y (by −rz).
    #[test]
    fn object_rotation_follows_the_exe() {
        let r = |rot: [i32; 3], v: [f32; 3]| {
            let m = transform([0; 3], rot);
            [m[0] * v[0] + m[1] * v[1] + m[2] * v[2], m[3] * v[0] + m[4] * v[1] + m[5] * v[2], m[6] * v[0] + m[7] * v[1] + m[8] * v[2]]
        };
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5);
        assert!(near(r([256, 0, 0], [1.0, 0.0, 0.0]), [0.0, 1.0, 0.0]), "rx: yaw");
        assert!(near(r([0, 256, 0], [0.0, 1.0, 0.0]), [0.0, 0.0, 1.0]), "ry: pitch about x");
        assert!(near(r([0, 0, 256], [0.0, 0.0, 1.0]), [-1.0, 0.0, 0.0]), "rz: roll about y");
    }
}
