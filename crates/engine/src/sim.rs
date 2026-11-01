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
use crate::world::WorldMesh;
use galapagos_formats::logic::{Actions, LogicView, PortalAction};
use galapagos_formats::worlds::{Object, Section, WorldObject};
use std::cell::Cell;

pub const TICKS_PER_SECOND: u32 = 24;

/// Something the browser has to do on the engine's behalf.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct HostEvent {
    pub kind: u32,
    pub a: i32,
    pub b: i32,
}

pub const EV_MUSIC: u32 = 1; // a = Smacker file 1..10, 0 = stop
pub const EV_SECTION: u32 = 2; // a = section, b = level (time screen)
pub const EV_SOUND: u32 = 3; // a = section sound index, b = object id
pub const EV_CAMERA_SNAP: u32 = 4; // a teleporter of kind 2 asks the camera to jump

/// What a portal or save pad asks of Mendel; the creature picks these up
/// on its tick (`Game_RequestTeleport` @ 0x41c690 queues them the same way).
#[derive(Clone, Copy, Debug)]
pub enum Request {
    /// `Game_SpawnCreature` @ 0x41c590: appear at `at` facing `heading`.
    Spawn { at: [f32; 3], heading: i32 },
    /// Portal kinds 1, 2 and 7.
    Teleport { at: [f32; 3], heading: i32, snap: bool },
    /// `Save` trigger (`0x41ce90`): the respawn point after a death.
    SavePoint { at: [f32; 3] },
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
    motion: Option<(Vec<([i32; 3], [i32; 3])>, usize, bool, bool)>,
    /// Current appearance state and per-polygon flip-book frame.
    appearance: usize,
    poly_frame: Vec<usize>,
    animating: bool,
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
}

impl Sim {
    pub fn new(section: &Section, mesh: &WorldMesh) -> Self {
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
                        // The initial state is queued, so its entry actions run
                        // on the first tick.
                        pending = Some(Actions { goto_state: Some(o.initial_state() as i16), ..Default::default() });
                    }
                }
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
                    appearance: 0,
                    poly_frame: Vec::new(),
                    animating: false,
                    skip: 0,
                }
            })
            .collect();
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
        }
    }

    /// The CRT `rand()` the original links (MSVC LCG, 15-bit output).
    fn rand(&self) -> u32 {
        let s = self.rng.get().wrapping_mul(214013).wrapping_add(2531011);
        self.rng.set(s);
        (s >> 16) & 0x7fff
    }

    /// Registers a click on object `id`; it counts as "clicked" during the
    /// next tick only, as in the original.
    pub fn click(&mut self, id: u16) {
        self.next_pick = Some(id);
    }

    /// Visits enabled objects in registry order, honouring folder skips.
    fn enabled_ids(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.objs.len());
        let mut i = 0;
        while i < self.objs.len() {
            out.push(i);
            i += if self.wake_all { 1 } else { self.objs[i].skip as usize + 1 };
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

        // Pass 2: apply.
        for &i in &ids {
            let Some(pending) = self.objs[i].pending.take() else { continue };
            let Some(sm) = self.objs[i].obj.as_ref().and_then(|w| w.logic.states.as_ref()) else { continue };
            let a = sm.resolve(pending);
            self.apply(i, a, mesh);
        }

        // Mendel moves through the world the objects just left.
        if let Some(mut c) = self.creature.take() {
            c.tick(self, mesh);
            self.creature = Some(c);
        }
        self.tick += 1;
    }

    // ——— What the creature may see and touch ———

    /// Current position and rotation (1024ths) of object list index `i`.
    pub(crate) fn pose(&self, i: usize) -> ([i32; 3], [i32; 3]) {
        self.objs.get(i).map_or(([0; 3], [0; 3]), |o| (o.pos, o.rot))
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

    /// Flags of polygon `p` of object `i` in its current appearance frame
    /// (0 if it has none): the collision classes of docs/re/objects.md §3.
    pub(crate) fn frame_flags(&self, i: usize, p: usize) -> u32 {
        let Some(o) = self.objs.get(i) else { return 0 };
        let Some(state) = o.obj.as_ref().and_then(|w| w.appearance.as_ref()).and_then(|s| s.get(o.appearance)) else {
            return 0;
        };
        let frame = *o.poly_frame.get(p).unwrap_or(&0);
        state.polygons.get(p).and_then(|f| f.get(frame).or(f.first())).map_or(0, |f| f.flags)
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
            self.objs[i].state = s.max(0) as u16;
            self.objs[i].ticks = 0;
        }
        if let Some(n) = a.appearance {
            self.select_appearance(i, (n - 1).max(0) as usize, mesh);
        }
        if let Some(n) = a.sound {
            let sound = self.objs[i].obj.as_ref().and_then(|o| o.logic.sounds.get((n - 1).max(0) as usize));
            if let Some(&s) = sound.and_then(|e| e.sounds.iter().find(|&&s| s >= 0)) {
                self.events.push(HostEvent { kind: EV_SOUND, a: s as i32, b: i as i32 + 1 });
            }
        }
        if let Some(n) = a.motion {
            self.start_motion(i, (n - 1).max(0) as usize);
        }
        if let Some(n) = a.trigger {
            self.trigger(i, (n - 1).max(0) as usize);
        }
        if a.reset_timer {
            self.objs[i].ticks = 0;
        }
    }

    /// The class behaviour (vtable slot 0x18), for the classes we know.
    fn trigger(&mut self, i: usize, entry: usize) {
        let Some(obj) = self.objs[i].obj.as_ref() else { return };
        let Some(p) = obj.logic.params.get(entry).copied() else { return };
        match &obj.kind {
            b"Fldr" => {
                // shorts[0]: 1 = enable, 0 = disable; shorts[1] = descendants.
                self.objs[i].skip = if p.shorts[0] != 0 { 0 } else { p.shorts[1].max(0) as u32 };
            }
            b"Voic" => self.events.push(HostEvent { kind: EV_MUSIC, a: p.shorts[0] as i32, b: 0 }),
            b"Tele" | b"Strt" | b"STel" => {
                let o = self.objs[i].pos;
                let at = |off: [i32; 3]| [(o[0] + off[0]) as f32, (o[1] + off[1]) as f32, (o[2] + off[2]) as f32];
                match p.portal() {
                    PortalAction::ChangeSection { section, level } => {
                        self.events.push(HostEvent { kind: EV_SECTION, a: section as i32, b: level as i32 })
                    }
                    // inferred: `mode` is ignored; the offset is always
                    // relative to the portal, which fits every pad on the CD.
                    PortalAction::Spawn { offset, heading, .. } => self.requests.push(Request::Spawn { at: at(offset), heading }),
                    PortalAction::Teleport { offset, heading, snap_camera, .. } => {
                        self.requests.push(Request::Teleport { at: at(offset), heading, snap: snap_camera })
                    }
                    // Kinds 5/6 only make Mendel react (an animation).
                    PortalAction::ReactOnly | PortalAction::Other(_) => {}
                }
            }
            b"Save" => {
                if p.shorts[0] != 0 {
                    let o = self.objs[i].pos;
                    self.requests.push(Request::SavePoint { at: o.map(|c| c as f32) });
                }
            }
            _ => {}
        }
    }

    fn start_motion(&mut self, i: usize, n: usize) {
        let o = &mut self.objs[i];
        let Some(path) = o.obj.as_ref().and_then(|w| w.logic.motions.get(n)) else { return };
        o.motion = Some((path.frames(), 0, path.looping != 0, path.relative != 0));
    }

    fn select_appearance(&mut self, i: usize, n: usize, mesh: &mut WorldMesh) {
        let o = &mut self.objs[i];
        let (Some(obj), Some(draw)) = (o.obj.as_ref(), o.draw) else { return };
        let Some(state) = obj.appearance.as_ref().and_then(|s| s.get(n)) else { return };
        o.appearance = n;
        o.poly_frame = vec![0; state.polygons.len()];
        o.animating = state.polygons.iter().any(|f| f.len() > 1);
        for (p, frames) in state.polygons.iter().enumerate() {
            if let Some(r) = mesh.apply_frame(draw, p, frames.first()) {
                mark(&mut self.dirty, r.first, r.first + r.count);
            }
        }
    }

    fn update_animations(&mut self, i: usize, mesh: &mut WorldMesh) {
        let o = &mut self.objs[i];

        // Motion path: one expanded frame per tick.
        if let Some((frames, at, looping, relative)) = &mut o.motion {
            if let Some(&(p, r)) = frames.get(*at) {
                o.pos = if *relative { [o.rest[0] + p[0], o.rest[1] + p[1], o.rest[2] + p[2]] } else { p };
                o.rot = r;
                if let Some(d) = o.draw {
                    write_transform(&mut mesh.transforms[d * 16..d * 16 + 16], o.pos, o.rot);
                }
            }
            *at += 1;
            if *at >= frames.len() {
                if *looping { *at = 0 } else { o.motion = None }
            }
        }

        // Appearance flip-book: every polygon steps through its frames.
        if o.animating {
            let (Some(obj), Some(draw)) = (o.obj.as_ref(), o.draw) else { return };
            let Some(state) = obj.appearance.as_ref().and_then(|s| s.get(o.appearance)) else { return };
            let looping = state.looping != 0;
            let mut still = false;
            for (p, frames) in state.polygons.iter().enumerate() {
                if frames.len() < 2 {
                    continue;
                }
                let f = &mut o.poly_frame[p];
                if *f + 1 < frames.len() {
                    *f += 1;
                    still = true;
                } else if looping {
                    *f = 0;
                    still = true;
                } else {
                    continue;
                }
                if let Some(r) = mesh.apply_frame(draw, p, frames.get(*f)) {
                    mark(&mut self.dirty, r.first, r.first + r.count);
                }
            }
            o.animating = still;
        }
    }

    /// Ray pick against every visible polygon whose current appearance frame
    /// is pickable (flags & 0xfffffffe), like `World_Pick` @ 0x4089e0.
    /// Returns the 1-based id of the nearest hit.
    pub fn pick(&self, origin: [f32; 3], dir: [f32; 3]) -> Option<u16> {
        let visible = self.visible();
        let mut best: Option<(f32, u16)> = None;
        for (i, o) in self.objs.iter().enumerate() {
            let (Some(obj), Some(_)) = (o.obj.as_ref(), o.draw) else { continue };
            if !visible[i] {
                continue;
            }
            let (Some(m), Some(states)) = (obj.mesh.as_ref(), obj.appearance.as_ref()) else { continue };
            let Some(state) = states.get(o.appearance) else { continue };
            let t = transform(o.pos, o.rot);
            for (p, poly) in m.polygons.iter().enumerate() {
                let frame = state.polygons.get(p).and_then(|f| f.get(*o.poly_frame.get(p).unwrap_or(&0)));
                let Some(f) = frame else { continue };
                if f.flags & 0xffff_fffe == 0 || f.enabled == 0 {
                    continue;
                }
                let pts: Vec<[f32; 3]> = poly
                    .iter()
                    .filter_map(|&k| m.vertices.get(k as usize))
                    .map(|v| apply(&t, [v[0] as f32, v[1] as f32, v[2] as f32]))
                    .collect();
                for k in 1..pts.len().saturating_sub(1) {
                    if let Some(d) = ray_triangle(origin, dir, pts[0], pts[k], pts[k + 1]) {
                        if best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, i as u16 + 1));
                        }
                    }
                }
            }
        }
        best.map(|(_, id)| id)
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
// Rotations are in 1024ths of a turn. The axis order is not yet confirmed
// from the exe (0x402370); we use Z·Y·X, the usual choice of the period.

pub(crate) type M = [f32; 12]; // 3×3 rotation (row-major) + translation

pub(crate) fn transform(pos: [i32; 3], rot: [i32; 3]) -> M {
    let a = rot.map(|r| r as f32 * core::f32::consts::TAU / 1024.0);
    let (sx, cx) = a[0].sin_cos();
    let (sy, cy) = a[1].sin_cos();
    let (sz, cz) = a[2].sin_cos();
    // R = Rz * Ry * Rx
    [
        cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx,
        sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx,
        -sy, cy * sx, cy * cx,
        pos[0] as f32, pos[1] as f32, pos[2] as f32,
    ]
}

pub(crate) fn apply(m: &M, v: [f32; 3]) -> [f32; 3] {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2] + m[9],
        m[3] * v[0] + m[4] * v[1] + m[5] * v[2] + m[10],
        m[6] * v[0] + m[7] * v[1] + m[8] * v[2] + m[11],
    ]
}

/// The inverse of [`apply`]: world point → object-local (the rotation is
/// orthonormal, so its inverse is its transpose).
pub(crate) fn unapply(m: &M, v: [f32; 3]) -> [f32; 3] {
    let d = [v[0] - m[9], v[1] - m[10], v[2] - m[11]];
    [
        m[0] * d[0] + m[3] * d[1] + m[6] * d[2],
        m[1] * d[0] + m[4] * d[1] + m[7] * d[2],
        m[2] * d[0] + m[5] * d[1] + m[8] * d[2],
    ]
}

/// Writes a column-major 4×4 for WebGL.
fn write_transform(out: &mut [f32], pos: [i32; 3], rot: [i32; 3]) {
    let m = transform(pos, rot);
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
            let mut sim = Sim::new(&section, &mesh);
            for _ in 0..30 * TICKS_PER_SECOND {
                sim.step(&mut mesh);
            }
            let moved = before.iter().zip(&mesh.transforms).filter(|(a, b)| a != b).count() / 16;
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
        let mut sim = Sim::new(&section, &mesh);
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
