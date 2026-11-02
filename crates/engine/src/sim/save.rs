//! Snapshots, saved games and the level clock (docs/re/saves.md).
//!
//! The original keeps **one** in-memory copy of the world, the *snapshot*
//! (`Game+0x17eff0`, a memory stream). `Game_TakeSnapshot` @ 0x41d170
//! writes into it the camera, the archive object (current section and the
//! ten `Save` variables) and every object's dynamic state, and remembers
//! where Mendel stands as his respawn point. It runs at four moments only:
//!
//! | when                         | caller                      |
//! |------------------------------|-----------------------------|
//! | a new game starts            | 0x4170cd / 0x417310         |
//! | a level starts (after a gate)| `Game_LevelEnd` @ 0x41c8c0  |
//! | the Save dialog succeeded    | view slot 0x100 @ 0x41bbf0  |
//! | a saved game was opened      | view slot 0xfc @ 0x41bbb0   |
//!
//! When Mendel dies (respawn reason 3, `Game_OnCreatureRespawn` @
//! 0x41c700) the snapshot is read back: the world returns to that instant
//! and he reappears at the respawn point. His **brain is not in the
//! snapshot** (the creature is not written to it), so he keeps what he
//! learnt. A saved game (`.gal`) is the same stream *plus* the creature,
//! brain included, and the game counters: opening one restores everything.
//!
//! Here the snapshot is a plain Rust value ([`WorldState`]); the saved game
//! is our own little-endian byte format, documented on [`Sim::save_game`].

use super::{mark, write_transform, Sim};
use crate::world::WorldMesh;
use galapagos_formats::logic::Actions;

/// Number of persistent `Save` variables (`0x41d530` checks `0..10`).
pub const VARS: usize = 10;

/// Magic and version of our saved-game format.
pub const MAGIC: [u8; 4] = *b"GLPS";
pub const VERSION: u32 = 1;

/// The dynamic part of one object: everything the simulation changes.
/// The original writes the same facts (`WorldObject` slot 0x4c @ 0x424010:
/// transform, state machine, running shape/appearance/motion, per-polygon
/// frames).
#[derive(Clone, Debug, PartialEq)]
pub struct ObjState {
    pub state: u16,
    pub ticks: u16,
    pub events: u32,
    pub prev_events: u32,
    pub pending: Option<Actions>,
    pub pos: [i32; 3],
    pub rot: [i32; 3],
    /// Running motion path: (path index, frame reached).
    pub motion: Option<(u16, u32)>,
    pub appearance: u32,
    pub poly_frame: Vec<u16>,
    pub animating: bool,
    pub skip: u32,
}

/// One world snapshot: all objects, the `Save` variables, the random
/// generator and the pending click.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldState {
    pub objs: Vec<ObjState>,
    pub vars: [i32; VARS],
    pub rng: u32,
    pub picked: Option<u16>,
    pub next_pick: Option<u16>,
}

impl Sim {
    /// Copies the dynamic world state (not Mendel).
    pub fn capture(&self) -> WorldState {
        WorldState {
            objs: self
                .objs
                .iter()
                .map(|o| ObjState {
                    state: o.state,
                    ticks: o.ticks,
                    events: o.events,
                    prev_events: o.prev_events,
                    pending: o.pending,
                    pos: o.pos,
                    rot: o.rot,
                    motion: o.motion.as_ref().map(|m| (o.motion_path, m.1 as u32)),
                    appearance: o.appearance as u32,
                    poly_frame: o.poly_frame.iter().map(|&f| f as u16).collect(),
                    animating: o.animating,
                    skip: o.skip,
                })
                .collect(),
            vars: self.vars,
            rng: self.rng.get(),
            picked: self.picked,
            next_pick: self.next_pick,
        }
    }

    /// Puts the world back as `w` describes it and re-applies every pose and
    /// appearance frame to the render buffers. Pending portal requests are
    /// dropped: they belonged to the abandoned timeline. Returns false (and
    /// changes nothing) if `w` was taken from another section.
    pub fn restore(&mut self, w: &WorldState, mesh: &mut WorldMesh) -> bool {
        if w.objs.len() != self.objs.len() {
            return false;
        }
        for (o, s) in self.objs.iter_mut().zip(&w.objs) {
            o.state = s.state;
            o.ticks = s.ticks;
            o.events = s.events;
            o.prev_events = s.prev_events;
            o.pending = s.pending;
            o.pos = s.pos;
            o.rot = s.rot;
            o.motion = s.motion.and_then(|(n, at)| {
                let path = o.obj.as_ref()?.logic.motions.get(n as usize)?;
                Some((path.frames(), at as usize, path.looping != 0, path.relative != 0))
            });
            o.motion_path = s.motion.map_or(0, |m| m.0);
            o.appearance = s.appearance as usize;
            o.poly_frame = s.poly_frame.iter().map(|&f| f as usize).collect();
            o.animating = s.animating;
            o.skip = s.skip;
            let (Some(obj), Some(d)) = (o.obj.as_ref(), o.draw) else { continue };
            write_transform(&mut mesh.transforms[d * 16..d * 16 + 16], o.pos, o.rot);
            let Some(state) = obj.appearance.as_ref().and_then(|a| a.get(o.appearance)) else { continue };
            for (p, frames) in state.polygons.iter().enumerate() {
                let f = o.poly_frame.get(p).copied().unwrap_or(0);
                if let Some(r) = mesh.apply_frame(d, p, frames.get(f).or(frames.first())) {
                    mark(&mut self.dirty, r.first, r.first + r.count);
                }
            }
        }
        self.vars = w.vars;
        self.rng.set(w.rng);
        self.picked = w.picked;
        self.next_pick = w.next_pick;
        self.requests.clear();
        true
    }

    /// `Game_TakeSnapshot` @ 0x41d170: remember the world as it is now, and
    /// where Mendel stands as his respawn point (if he is alive).
    pub fn take_snapshot(&mut self) {
        self.snapshot = Some(self.capture());
        if let Some(c) = self.creature.as_mut() {
            c.anchor_respawn();
        }
    }

    /// Respawn reason 3: the world goes back to the last snapshot.
    pub fn restore_snapshot(&mut self, mesh: &mut WorldMesh) -> bool {
        let Some(s) = self.snapshot.take() else { return false };
        let ok = self.restore(&s, mesh);
        self.snapshot = Some(s);
        ok
    }

    /// The time screen's four numbers: level ticks, total ticks, then level
    /// and total **seconds** computed exactly as `Game_LevelEnd` @ 0x41c8c0
    /// does (`total/24 - start/24`, `total/24`, unsigned).
    pub fn times(&self) -> [u32; 4] {
        let tps = super::TICKS_PER_SECOND;
        [
            self.total_ticks.wrapping_sub(self.level_start_ticks),
            self.total_ticks,
            (self.total_ticks / tps).wrapping_sub(self.level_start_ticks / tps),
            self.total_ticks / tps,
        ]
    }

    /// Serialises the whole game. Format `GLPS` version 1, little-endian:
    ///
    /// ```text
    /// u8[4] "GLPS"   u32 version (1)
    /// u32 section    u32 total_ticks   u32 level_start_ticks   u64 sim_tick
    /// WorldState     the live world
    /// u8 has_snapshot,  WorldState     the respawn snapshot
    /// u8 has_creature,  u32 brain_len, u8[brain_len]  Brain::to_bytes (.mnd)
    ///                   SavedCreature  (creature.rs, fixed 414 bytes)
    ///
    /// WorldState = u32 rng; u16 picked; u16 next_pick (0 = none);
    ///              i32 vars[10]; u32 object_count; object_count x ObjState
    /// ObjState   = u16 state; u16 ticks; u32 events; u32 prev_events;
    ///              u8 pending_mask; i16 x popcount(mask & 0x7e);
    ///              i32 pos[3]; i32 rot[3];
    ///              u8 has_motion; u16 path; u32 frame;
    ///              u32 appearance; u8 animating; u32 skip;
    ///              u32 n; u16 poly_frame[n]
    /// pending_mask: bit 0 present, 1 trigger, 2 shape, 3 appearance,
    ///              4 sound, 5 motion, 6 goto, 7 reset_timer
    ///              (the i16 values follow in that order).
    /// ```
    pub fn save_game(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.bytes(&MAGIC);
        w.u32(VERSION);
        w.u32(self.section as u32);
        w.u32(self.total_ticks);
        w.u32(self.level_start_ticks);
        w.u64(self.tick);
        write_world(&mut w, &self.capture());
        match &self.snapshot {
            Some(s) => {
                w.u8(1);
                write_world(&mut w, s);
            }
            None => w.u8(0),
        }
        match &self.creature {
            Some(c) => {
                w.u8(1);
                let brain = c.brain.to_bytes();
                w.u32(brain.len() as u32);
                w.bytes(&brain);
                c.saved().write(&mut w);
            }
            None => w.u8(0),
        }
        w.0
    }

    /// Restores a game written by [`Sim::save_game`] into this simulation,
    /// which must already run the saved section (see [`saved_section`]).
    pub fn load_game(&mut self, data: &[u8], mesh: &mut WorldMesh) -> Result<(), &'static str> {
        let mut r = Reader { data, at: 0 };
        let section = header(&mut r)?;
        if section as usize != self.section {
            return Err("saved game is for another section");
        }
        let total = r.u32()?;
        let start = r.u32()?;
        let tick = r.u64()?;
        let live = read_world(&mut r)?;
        let snapshot = if r.u8()? != 0 { Some(read_world(&mut r)?) } else { None };
        if live.objs.len() != self.objs.len() {
            return Err("saved game has another object count");
        }
        // Parse the brain and the creature block before touching anything.
        let creature = if r.u8()? != 0 {
            let n = r.u32()? as usize;
            let brain = galapagos_formats::creature::Brain::parse(r.take(n)?, 0).map_err(|_| "bad brain")?;
            let state = crate::creature::SavedCreature::read(&mut r)?;
            Some((brain, state))
        } else {
            None
        };
        self.restore(&live, mesh);
        self.snapshot = snapshot.filter(|s| s.objs.len() == self.objs.len());
        self.total_ticks = total;
        self.level_start_ticks = start;
        self.tick = tick;
        if let (Some((brain, state)), Some(c)) = (creature, self.creature.as_mut()) {
            c.brain = brain;
            c.apply_saved(state);
        }
        if self.snapshot.is_none() {
            self.take_snapshot();
        }
        Ok(())
    }
}

/// Reads the section index out of a saved game, so the host can build that
/// section before calling [`Sim::load_game`].
pub fn saved_section(data: &[u8]) -> Result<u32, &'static str> {
    header(&mut Reader { data, at: 0 })
}

fn header(r: &mut Reader) -> Result<u32, &'static str> {
    if r.take(4)? != MAGIC {
        return Err("not a Galapagos web save");
    }
    if r.u32()? != VERSION {
        return Err("unsupported save version");
    }
    r.u32()
}

fn write_world(w: &mut Writer, s: &WorldState) {
    w.u32(s.rng);
    w.u16(s.picked.unwrap_or(0));
    w.u16(s.next_pick.unwrap_or(0));
    for v in s.vars {
        w.i32(v);
    }
    w.u32(s.objs.len() as u32);
    for o in &s.objs {
        w.u16(o.state);
        w.u16(o.ticks);
        w.u32(o.events);
        w.u32(o.prev_events);
        write_actions(w, o.pending);
        for v in o.pos.iter().chain(&o.rot) {
            w.i32(*v);
        }
        let (has, path, at) = o.motion.map_or((0, 0, 0), |(p, a)| (1, p, a));
        w.u8(has);
        w.u16(path);
        w.u32(at);
        w.u32(o.appearance);
        w.u8(o.animating as u8);
        w.u32(o.skip);
        w.u32(o.poly_frame.len() as u32);
        for &f in &o.poly_frame {
            w.u16(f);
        }
    }
}

fn read_world(r: &mut Reader) -> Result<WorldState, &'static str> {
    let rng = r.u32()?;
    let nz = |v: u16| (v != 0).then_some(v);
    let picked = nz(r.u16()?);
    let next_pick = nz(r.u16()?);
    let mut vars = [0; VARS];
    for v in &mut vars {
        *v = r.i32()?;
    }
    let n = r.u32()? as usize;
    if n > 100_000 {
        return Err("absurd object count");
    }
    let mut objs = Vec::with_capacity(n);
    for _ in 0..n {
        let state = r.u16()?;
        let ticks = r.u16()?;
        let events = r.u32()?;
        let prev_events = r.u32()?;
        let pending = read_actions(r)?;
        let mut v = [0i32; 6];
        for x in &mut v {
            *x = r.i32()?;
        }
        let has = r.u8()?;
        let path = r.u16()?;
        let at = r.u32()?;
        let appearance = r.u32()?;
        let animating = r.u8()? != 0;
        let skip = r.u32()?;
        let k = r.u32()? as usize;
        if k > 1_000_000 {
            return Err("absurd polygon count");
        }
        let mut poly_frame = Vec::with_capacity(k);
        for _ in 0..k {
            poly_frame.push(r.u16()?);
        }
        objs.push(ObjState {
            state,
            ticks,
            events,
            prev_events,
            pending,
            pos: [v[0], v[1], v[2]],
            rot: [v[3], v[4], v[5]],
            motion: (has != 0).then_some((path, at)),
            appearance,
            poly_frame,
            animating,
            skip,
        });
    }
    Ok(WorldState { objs, vars, rng, picked, next_pick })
}

fn slots(a: &mut Actions) -> [&mut Option<i16>; 6] {
    [&mut a.trigger, &mut a.shape, &mut a.appearance, &mut a.sound, &mut a.motion, &mut a.goto_state]
}

fn write_actions(w: &mut Writer, a: Option<Actions>) {
    let Some(mut a) = a else {
        w.u8(0);
        return;
    };
    let mut mask = 1u8 | (a.reset_timer as u8) << 7;
    let mut values = Vec::new();
    for (k, s) in slots(&mut a).into_iter().enumerate() {
        if let Some(v) = *s {
            mask |= 2 << k;
            values.push(v);
        }
    }
    w.u8(mask);
    for v in values {
        w.u16(v as u16);
    }
}

fn read_actions(r: &mut Reader) -> Result<Option<Actions>, &'static str> {
    let mask = r.u8()?;
    if mask & 1 == 0 {
        return Ok(None);
    }
    let mut a = Actions { reset_timer: mask & 0x80 != 0, ..Default::default() };
    for (k, s) in slots(&mut a).into_iter().enumerate() {
        if mask & (2 << k) != 0 {
            *s = Some(r.u16()? as i16);
        }
    }
    Ok(Some(a))
}

/// A growable little-endian byte sink.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    pub fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.bytes(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.bytes(&v.to_le_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.bytes(&v.to_le_bytes());
    }
}

/// The matching reader; every read fails cleanly on truncated input.
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub at: usize,
}

impl<'a> Reader<'a> {
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.data.len()).ok_or("truncated save")?;
        let s = &self.data[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    pub fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, &'static str> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    pub fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub fn i32(&mut self) -> Result<i32, &'static str> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    pub fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    pub fn i64(&mut self) -> Result<i64, &'static str> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    pub fn f32(&mut self) -> Result<f32, &'static str> {
        Ok(f32::from_le_bytes(self.arr()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creature::{Creature, Mode};
    use crate::sim::{Request, TICKS_PER_SECOND};
    use galapagos_formats::creature::{overlay_for_age, Brain, CreatureBody};
    use galapagos_formats::worlds::Archive;

    fn data() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds")).ok()
    }

    /// Section `s`, optionally with Mendel (level-3 brain), snapshot taken
    /// as `world_build` does.
    fn world(d: &[u8], s: usize, mendel: bool) -> (Sim, WorldMesh) {
        let a = Archive::parse(d).unwrap();
        let section = a.world(s).unwrap();
        let mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mesh);
        sim.section = s;
        if mendel {
            let body = CreatureBody::parse(d, a.header.creature as usize).unwrap();
            let brain = Brain::parse(d, a.header.creature_overlays[overlay_for_age(128).unwrap()] as usize).unwrap();
            sim.creature = Some(Creature::new(&section, &mesh, body, brain));
        }
        sim.take_snapshot();
        (sim, mesh)
    }

    fn run(sim: &mut Sim, mesh: &mut WorldMesh, ticks: u32) {
        for _ in 0..ticks {
            sim.step(mesh);
        }
    }

    /// Section 0 after the Space-key skip: Mendel alive on the arrival pad.
    fn mendel_alive(d: &[u8]) -> (Sim, WorldMesh) {
        let (mut sim, mut mesh) = world(d, 0, true);
        let at = 5 * TICKS_PER_SECOND as u64;
        for _ in 0..10 * TICKS_PER_SECOND {
            sim.skip_held = (at..at + 2).contains(&sim.tick);
            sim.step(&mut mesh);
        }
        sim.skip_held = false;
        assert_eq!(sim.creature.as_ref().unwrap().mode, Mode::Alive);
        (sim, mesh)
    }

    /// Snapshot → let the world run on → restore: the objects, the render
    /// buffers and even the *future* are those of the snapshot (same
    /// state, same random generator: the world replays identically).
    #[test]
    fn snapshot_restore_round_trip_when_data_present() {
        let Some(d) = data() else { return };
        for s in [0, 2] {
            let (mut sim, mut mesh) = world(&d, s, false);
            sim.wake_all = true; // every machine runs: the hardest case
            run(&mut sim, &mut mesh, 10 * TICKS_PER_SECOND);
            let a = sim.capture();
            let (transforms, vertices) = (mesh.transforms.clone(), mesh.vertices.clone());
            run(&mut sim, &mut mesh, 100);
            let b = sim.capture();
            run(&mut sim, &mut mesh, 30 * TICKS_PER_SECOND);
            assert_ne!(sim.capture(), a, "section {s}: the world moved on");
            assert!(sim.restore(&a, &mut mesh));
            assert_eq!(sim.capture(), a, "section {s}: state restored");
            assert!(mesh.transforms == transforms, "section {s}: poses restored");
            assert!(mesh.vertices == vertices, "section {s}: appearance frames restored");
            run(&mut sim, &mut mesh, 100);
            assert_eq!(sim.capture(), b, "section {s}: the restored world replays identically");
        }
    }

    /// game_save → game_load into a fresh build of the section reproduces
    /// everything: saving again gives the same bytes, and both games then
    /// run on identically, Mendel and his learning included.
    #[test]
    fn saved_game_round_trip_when_data_present() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = mendel_alive(&d);
        run(&mut sim, &mut mesh, 20 * TICKS_PER_SECOND);
        sim.vars[3] = 7;
        sim.take_snapshot();
        let bytes = sim.save_game();
        println!("saved game: {} bytes", bytes.len());
        let mut w = Writer::default();
        sim.creature.as_ref().unwrap().saved().write(&mut w);
        assert_eq!(w.0.len(), 414, "SavedCreature size documented in save_game");
        assert_eq!(saved_section(&bytes), Ok(0));

        let (mut other, mut other_mesh) = world(&d, 0, true);
        other.load_game(&bytes, &mut other_mesh).unwrap();
        assert!(other.save_game() == bytes, "load then save is the identity");
        assert_eq!(other.vars[3], 7);
        assert_eq!(other.times(), sim.times());
        assert!(other_mesh.transforms == mesh.transforms);

        run(&mut sim, &mut mesh, 10 * TICKS_PER_SECOND);
        run(&mut other, &mut other_mesh, 10 * TICKS_PER_SECOND);
        assert!(sim.save_game() == other.save_game(), "both games continue identically");

        // Damaged input is refused without touching the running game.
        let before = other.save_game();
        assert!(other.load_game(&bytes[..bytes.len() - 9], &mut other_mesh).is_err());
        assert!(other.load_game(b"nope", &mut other_mesh).is_err());
        assert!(other.save_game() == before);
    }

    /// The readme's promise: after saving, a death restores every game
    /// element to its saved condition, except Mendel's brain.
    #[test]
    fn death_after_saving_restores_the_world_but_not_the_brain_when_data_present() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = mendel_alive(&d);
        sim.take_snapshot(); // what game_save does
        let saved = sim.capture();
        let saved_brain = sim.creature.as_ref().unwrap().brain.clone();
        let anchor = sim.creature.as_ref().unwrap().pos;

        run(&mut sim, &mut mesh, 60 * TICKS_PER_SECOND);
        let c = sim.creature.as_ref().unwrap();
        let changed = sim.objs.iter().zip(&saved.objs).filter(|(o, s)| o.state != s.state || o.pos != s.pos).count();
        println!("after 60 s: {changed} objects differ from the save, {} generations", c.stats.evolutions);
        assert!(changed > 0);
        assert_ne!(c.brain, saved_brain, "he learnt something in a minute");

        sim.requests.push(Request::Respawn); // portal kind 6 / a lethal floor
        let mut learnt = None;
        for _ in 0..60 {
            learnt = Some(sim.creature.as_ref().unwrap().brain.clone());
            sim.step(&mut mesh);
            if matches!(sim.creature.as_ref().unwrap().mode, Mode::Appearing(_)) {
                break;
            }
        }
        assert!(matches!(sim.creature.as_ref().unwrap().mode, Mode::Appearing(_)), "he died and came back");
        let now = sim.capture();
        let menc = sim.objs.iter().position(|o| o.obj.as_ref().is_some_and(|w| &w.kind == b"MEnc"));
        for (i, (o, s)) in now.objs.iter().zip(&saved.objs).enumerate() {
            if Some(i) != menc {
                assert_eq!(o, s, "object #{} back to its saved condition", i + 1);
            }
        }
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(Some(&c.brain), learnt.as_ref(), "the brain is not restored");
        let dist = (0..3).map(|k| (c.pos[k] - anchor[k]).powi(2)).sum::<f32>().sqrt();
        println!("respawned {dist:.0} units from where he stood when saving");
        assert!(dist < 1.0, "back on the spot where he saved");
    }

    /// The persistent variables of the `Save` class: section 2's objects
    /// #212/#213 load slot 1/0 at start and store their state back.
    #[test]
    fn save_variables_reach_the_hub_when_data_present() {
        let Some(d) = data() else { return };
        let (mut fresh, mut fresh_mesh) = world(&d, 2, false);
        run(&mut fresh, &mut fresh_mesh, 5 * TICKS_PER_SECOND);
        let (mut sim, mut mesh) = world(&d, 2, false);
        sim.vars[0] = 1;
        sim.vars[1] = 1;
        run(&mut sim, &mut mesh, 5 * TICKS_PER_SECOND);
        println!(
            "section 2, fresh: #212 {} #213 {} vars {:?}; after the first loop: #212 {} #213 {} vars {:?}",
            fresh.state_of(212),
            fresh.state_of(213),
            &fresh.vars[..2],
            sim.state_of(212),
            sim.state_of(213),
            &sim.vars[..2]
        );
        assert_eq!((sim.state_of(212), sim.state_of(213)), (1, 1), "the remembered states came back");
    }

    #[test]
    fn time_screen_seconds_match_the_original_rounding() {
        let Some(d) = data() else { return };
        let (mut sim, _) = world(&d, 0, false);
        sim.total_ticks = 24 * 125 + 23;
        sim.level_start_ticks = 24 * 60 + 23;
        // floor(3023/24) - floor(1463/24) = 65, as the exe computes it.
        assert_eq!(sim.times(), [1560, 3023, 65, 125]);
    }
}
