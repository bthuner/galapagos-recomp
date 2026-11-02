//! Predators: `Pred` and its hunting children `PBal`, `PFrk`, `PMan`, plus
//! the `PPol` that launches the balls. docs/re/predators.md is the spec.
//!
//! They are ordinary world objects (their own mesh, appearance states and
//! state machine, drawn by the world renderer) whose class adds a small
//! behaviour on top:
//!
//! * **`Pred`** (ctor 0x42a1a0, trigger 0x42a2d0, update 0x42a350) only
//!   knows how to *vanish* and *appear*. Trigger `shorts[0]` = 1 vanishes
//!   (the transition effect of kind `shorts[2]`, variant `shorts[3]`, if
//!   `shorts[1]`), then parks the object far away; 2 brings it back to its
//!   home and makes it **active** (mode 3). A `Pred` hurts only through
//!   its polygons' flags (section 7's four have lethal floors/walls).
//! * **`PBal` / `PFrk` / `PMan`** (base ctor 0x415e20) add a *chase*: a
//!   trapezoidal mover (the camera's, 0x427780) aims past Mendel, smoothed
//!   by an alpha-beta tracker (0x406a50); six probes around the body touch
//!   walls, floors and Mendel's click hull `MEnc`. Touching `MEnc` hands
//!   Mendel the predator's surface flags (`MEnc` Update 0x426150 →
//!   `Game_CreatureReact` @ 0x41cb80 → `creature+0xae8`): a lethal surface
//!   of kind 2, so he dies and respawns.
//! * **`PPol`** (0x4292b0, trigger 0x429430, update 0x429600) circles its
//!   home on the side Mendel is on, bobbing; `PBal` appears where it is.
//!
//! Each probe and timer constant below comes from the constructors
//! (`0x415f50` base, `0x4150d0` PBal, `0x415590` PFrk); the per-class
//! chase is `0x415220` (PBal) and `0x4156a0` (PFrk).

use crate::camera::{approx_len, filter3, Filter, Mover, Trig};
use crate::sim::save::{Reader, Writer};
use crate::sim::{self, HostEvent, Sim};
use crate::world::WorldMesh;

type I3 = [i32; 3];

/// The fixed-point trig tables (`Math_InitTables` @ 0x4308c0), built once.
fn trig() -> &'static Trig {
    static T: std::sync::OnceLock<Trig> = std::sync::OnceLock::new();
    T.get_or_init(Trig::new)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Pred,
    Ball,
    Fork,
    Man,
    Pole,
}

impl Class {
    pub fn of(kind: &[u8; 4]) -> Option<Class> {
        Some(match kind {
            b"Pred" => Class::Pred,
            b"PBal" => Class::Ball,
            b"PFrk" => Class::Fork,
            b"PMan" => Class::Man,
            b"PPol" => Class::Pole,
            _ => return None,
        })
    }
    fn hunts(self) -> bool {
        matches!(self, Class::Ball | Class::Fork | Class::Man)
    }
}

/// A hunter's constants (`+0x338..+0x346`, `+0x20c`, `+0x224`).
#[derive(Clone, Copy, Debug)]
struct Tuning {
    accel: i16,
    max: i16,
    /// How far past the target the mover aims (`+0x33c`).
    overshoot: i32,
    /// Tracker parameter `d` (16.16, `+0x340`).
    d: i32,
    /// A bounce pushes by the hit normal (×1024) `>> shift` (`+0x344`).
    shift: u32,
    /// Probe length along each body axis (`+0x346`).
    probe: i32,
    /// The surface flags it hands what it touches (`+0x20c`).
    flags: u32,
    /// Where it waits while vanished (`+0x224`).
    park: i32,
    /// Distance under which it growls (sound 1), and how often.
    growl_quiet: u16,
}

fn tuning(c: Class) -> Tuning {
    match c {
        // 0x4150d0. 0x405b2100 = hazard (bit 30) + lethal kind 2,
        // variant 3, then respawn reason 3, immune under affliction 1
        // (bit 22). No pain bit.
        Class::Ball => Tuning { accel: 30, max: 300, overshoot: 500, d: 0xe666, shift: 7, probe: 700, flags: 0x405b_2100, park: 100_000, growl_quiet: 200 },
        // 0x415590: kind 2, variant 0.
        Class::Fork => Tuning { accel: 15, max: 150, overshoot: 4000, d: 0xc000, shift: 5, probe: 300, flags: 0x4058_2100, park: 1_500_000, growl_quiet: 15 },
        // The base (0x415f50); `+0x20c` stays 0 from the Pred ctor.
        // inferred: PMan's chase (0x415c40) was not read; no PMan on the CD.
        _ => Tuning { accel: 10, max: 100, overshoot: 5000, d: 0xcccc, shift: 5, probe: 300, flags: 0, park: 100_000, growl_quiet: 15 },
    }
}

/// inferred: how long the vanish/appear transition effect (`+0x210`, the
/// same effect class as Mendel's dematerialisation, 0x424520) runs. Its
/// timing lives in the effect code, not ported; the state machines leave it
/// 8 ticks (PBal's vanish) and more.
pub const EFFECT_TICKS: u16 = 12;
/// "Close to Mendel" for the growl (`cmp eax, 0x3e8` at 0x415310).
const GROWL_DISTANCE: i32 = 1000;
/// Silence after the "got him" sound (`+0x23c = 0x1e`).
const BITE_QUIET: u16 = 30;
/// Polygon classes the probes collide with (`World_Cast` mask 0x103):
/// Mendel's click hull (bit 0), floors (bit 1), walls (bit 8).
const PROBE_MASK: u32 = 0x103;

/// `PPol` (0x429430 reads its trigger `ints[0..8]` into `+0x20c..+0x224`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Pole {
    /// 0 = still, 1 = turning towards Mendel's bearing, 2 = holding.
    mode: i16,
    radius: i32,
    angle: i16,
    bob_amp: i32,
    bob: i16,
    wobble_amp: i16,
    wobble: i16,
    lift: i32,
    filter: [Filter; 3],
}

/// One predator's dynamic state. Everything here is in the snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct Predator {
    pub obj: usize,
    pub class: Class,
    /// `+0x208`: 0 idle, 1 vanishing, 2 appearing, 3 active.
    pub mode: i16,
    /// `+0x218`: the transition effect has been started.
    started: bool,
    /// `+0x21a..+0x21e`: play an effect, its kind and variant.
    effect: bool,
    kind: i16,
    variant: i16,
    /// inferred stand-in for the effect object's own timer.
    effect_left: u16,
    // ——— the chase (`+0x2b8` mover + tracker, `+0x348` target) ———
    mover: Mover,
    filter: [Filter; 3],
    /// `+0x2fc`: the tracker's output, the body's centre.
    centre: I3,
    /// `+0x354` (PFrk): the tracker smoothing its heading.
    heading: [Filter; 3],
    /// `+0x23a`, `+0x23c`: growl and bite silences.
    quiet: [u16; 2],
    /// `+0x31c`: a probe touched something this tick.
    pub contact: bool,
    pole: Pole,
}

/// Mendel as the predators see him.
#[derive(Clone, Copy, Debug)]
pub struct Prey {
    pub pos: I3,
    pub affliction: u8,
    pub menc: Option<usize>,
}

impl Predator {
    pub fn new(obj: usize, class: Class) -> Self {
        Predator {
            obj,
            class,
            mode: 0,
            started: false,
            effect: false,
            kind: 0,
            variant: 0,
            effect_left: 0,
            mover: Mover::default(),
            filter: [Filter::default(); 3],
            centre: [0; 3],
            heading: [Filter::default(); 3],
            quiet: [0; 2],
            contact: false,
            pole: Pole::default(),
        }
    }

    /// The class trigger (vtable slot 0x18) with one params entry.
    pub fn trigger(&mut self, shorts: [i16; 10], ints: [i32; 10], sim: &mut Sim, mesh: &mut WorldMesh) {
        if self.class == Class::Pole {
            self.pole_trigger(ints, sim, mesh);
            return;
        }
        // 0x42a2d0: shorts[0] 1 = vanish, 2 = appear; ignored while one runs.
        let a = shorts[0];
        if (a == 1 || a == 2) && self.mode != 1 && self.mode != 2 {
            self.mode = a;
            self.effect = shorts[1] != 0;
            self.kind = shorts[2];
            self.variant = shorts[3];
        }
    }

    /// The class update (vtable slot 0x28, after the base update).
    pub fn tick(&mut self, sim: &mut Sim, mesh: &mut WorldMesh, prey: Option<Prey>) {
        match self.class {
            Class::Pole => self.pole_tick(sim, mesh, prey),
            _ => match self.mode {
                1 => self.vanish(sim, mesh),
                2 => self.appear(sim, mesh),
                3 if self.class.hunts() => self.chase(sim, mesh, prey),
                _ => {}
            },
        }
    }

    fn park_at(&self) -> i32 {
        tuning(self.class).park
    }

    /// Mode 1 (`0x42a3e0`): the effect, then park it out of the world.
    fn vanish(&mut self, sim: &mut Sim, mesh: &mut WorldMesh) {
        if self.effect && self.run_effect(sim, mesh) {
            return;
        }
        self.mode = 0;
        self.started = false;
        let p = self.park_at();
        let (_, rot) = sim.pose(self.obj);
        sim.set_pose(self.obj, [p, p, p], rot, mesh);
    }

    /// Mode 2 (`0x42a4b0`): back home, the class's own setup (slot 0x9c),
    /// the effect in reverse, then active.
    fn appear(&mut self, sim: &mut Sim, mesh: &mut WorldMesh) {
        if !self.started {
            let (_, rot) = sim.pose(self.obj);
            sim.set_pose(self.obj, sim.rest_of(self.obj), rot, mesh);
            self.setup_chase(sim, mesh);
            if !self.effect {
                self.mode = 3;
                return;
            }
        }
        if self.run_effect(sim, mesh) {
            return;
        }
        self.mode = 3;
        self.started = false;
    }

    /// Steps the transition effect; true while it runs. inferred: a blink
    /// stands in for the original's explosion/implosion effects.
    fn run_effect(&mut self, _sim: &mut Sim, mesh: &mut WorldMesh) -> bool {
        if !self.started {
            self.started = true;
            self.effect_left = EFFECT_TICKS;
        }
        self.effect_left = self.effect_left.saturating_sub(1);
        let draw = mesh.draw_of_object.get(self.obj).copied().flatten();
        let running = self.effect_left > 0;
        if let Some(d) = draw {
            mesh.set_blinking(d, running && self.effect_left % 4 < 2);
        }
        running
    }

    /// Slot 0x9c: PBal (0x4153a0) starts where the section's `PPol` is
    /// (`Game+0x17efc0`, 0x41cbd0); the others (0x416300) where they are.
    fn setup_chase(&mut self, sim: &mut Sim, mesh: &mut WorldMesh) {
        if !self.class.hunts() {
            return;
        }
        let mut at = sim.position_of(self.obj as u16 + 1);
        if self.class == Class::Ball {
            at = sim.pole.map_or([0; 3], |p| sim.position_of(p as u16 + 1));
            let (_, rot) = sim.pose(self.obj);
            sim.set_pose(self.obj, at, rot, mesh);
        }
        let t = tuning(self.class);
        // 0x414b70: mover at `at`, tracker primed there.
        self.mover.setup(at, t.accel, t.max);
        for f in &mut self.filter {
            f.reset(t.d);
        }
        self.centre = filter3(&mut self.filter, at);
        for f in &mut self.heading {
            f.reset(0xf333);
        }
    }

    /// Mode 3 of the hunters (`0x415220` PBal, `0x4156a0` PFrk), with the
    /// shared step `0x416360` and the probes `0x4160d0`.
    fn chase(&mut self, sim: &mut Sim, mesh: &mut WorldMesh, prey: Option<Prey>) {
        let trig = trig();
        let t = tuning(self.class);
        let (pos, mut rot) = sim.pose(self.obj);
        // PBal leaves a camouflaged Mendel (affliction 4) alone and goes
        // home; PFrk always follows him.
        let target = match prey {
            Some(p) if self.class != Class::Ball || p.affliction != 4 => p.pos,
            _ => sim.rest_of(self.obj),
        };
        // 0x416360: when the mover has arrived, aim again, `overshoot`
        // past the target along the line from here.
        self.mover.step();
        let mut out = filter3(&mut self.filter, self.mover.pos);
        if !self.mover.moving {
            let d = [target[0] - pos[0], target[1] - pos[1], target[2] - pos[2]];
            let len = approx_len(d).max(1);
            let aim = [0, 1, 2].map(|k| target[k] + t.overshoot * d[k] / len);
            self.mover.set_target(aim);
        }
        // 0x4160d0: six probes along the body's axes.
        self.contact = false;
        let m = sim::transform(out, rot);
        for k in 0..6 {
            let mut off = [0.0f32; 3];
            off[k / 2] = if k % 2 == 0 { t.probe as f32 } else { -t.probe as f32 };
            let from = out.map(|c| c as f32);
            let to = sim::apply(&m, off);
            let Some(hit) = sim.creature.as_ref().and_then(|c| c.probe(sim, from, to, PROBE_MASK, self.obj, t.shift)) else { continue };
            self.contact = true;
            // `WorldObject_PostEvent` with the predator's flags; MEnc
            // forwards them to Mendel (0x426150 → 0x41cb80).
            sim.post_event(hit.obj, t.flags);
            if prey.is_some_and(|p| p.menc == Some(hit.obj)) {
                if let Some(c) = sim.creature.as_mut() {
                    c.react(t.flags);
                }
            }
            // The bounce (`0x408c50` + the stored normal >> shift): only
            // when the probe's tip went behind the polygon.
            let Some(push) = hit.push else { continue };
            for i in 0..3 {
                out[i] += push[i];
                self.mover.pos[i] += push[i];
                self.filter[i].out += push[i];
            }
        }
        self.centre = out;
        // The body's spin or heading.
        match self.class {
            Class::Ball => {
                rot = [rot[0] - 20, rot[1] + 20, rot[2] + 20];
            }
            _ => {
                // PFrk faces where it is heading: `+0x32c` (target − here)
                // smoothed by its own tracker, then `Math_VectorAngles`.
                let d = [target[0] - pos[0], target[1] - pos[1], target[2] - pos[2]];
                let s = filter3(&mut self.heading, d);
                let (yaw, pitch) = trig.dir_angles(s);
                // inferred: `+0x70, +0x72, +0x74` are our rot[0..3] (as the
                // motion paths' rotations), 0x430c50 writes (+0x72, +0x70).
                rot = [pitch & 0x3ff, yaw & 0x3ff, rot[2] + 10];
            }
        }
        sim.set_pose(self.obj, out, rot.map(|r| r & 0x3ff), mesh);
        // Sounds (predator bank): a growl near Mendel, a bite on contact.
        if self.quiet[0] == 0 {
            if let Some(p) = prey {
                let d = [p.pos[0] - pos[0], p.pos[1] - pos[1], p.pos[2] - pos[2]];
                if approx_len(d) < GROWL_DISTANCE {
                    self.sound(sim, 0, 0x10000);
                    self.quiet[0] = t.growl_quiet;
                }
            }
        } else {
            self.quiet[0] -= 1;
        }
        if self.quiet[1] == 0 {
            if self.contact {
                self.sound(sim, 1, 0x8000);
                self.quiet[1] = BITE_QUIET;
            }
        } else {
            self.quiet[1] -= 1;
        }
    }

    /// The two cues of `0x42a5a0`: sound 1 at pitch 1.0 (heard 5000 to
    /// 10000 away), sound 2 at half pitch (5000).
    fn sound(&self, sim: &mut Sim, sound: i32, pitch: i32) {
        sim.events.push(HostEvent { kind: sim::EV_PREDATOR_SOUND, a: sound, b: self.obj as i32 + 1, c: pitch });
    }

    // ——— PPol ———

    /// `0x429430`: read the orbit, jump to its start point.
    fn pole_trigger(&mut self, ints: [i32; 10], sim: &mut Sim, mesh: &mut WorldMesh) {
        let trig = trig();
        let p = &mut self.pole;
        p.mode = ints[0] as i16;
        p.radius = ints[1];
        p.angle = ints[2] as i16;
        p.bob_amp = ints[3];
        p.bob = ints[4] as i16;
        p.wobble_amp = ints[5] as i16;
        p.wobble = ints[6] as i16;
        p.lift = ints[7];
        let home = sim.rest_of(self.obj);
        let a = (p.angle as i32 & 0x3ff) as usize;
        // The start height uses sin(angle) × 2000, as the exe does.
        let at = [
            home[0] + (trig.cos[a] * p.radius >> 14),
            home[1] + (trig.sin[a] * p.radius >> 14),
            home[2] + (trig.sin[a] * 2000 >> 14) + p.lift,
        ];
        for f in &mut p.filter {
            f.reset(0xf333);
        }
        let at = filter3(&mut p.filter, at);
        let (_, rot) = sim.pose(self.obj);
        sim.set_pose(self.obj, at, rot, mesh);
    }

    /// `0x429600`: turn towards Mendel's bearing from home, bob, wobble.
    fn pole_tick(&mut self, sim: &mut Sim, mesh: &mut WorldMesh, prey: Option<Prey>) {
        let trig = trig();
        if self.pole.mode == 0 {
            return;
        }
        let Some(prey) = prey else { return };
        let p = &mut self.pole;
        let home = sim.rest_of(self.obj);
        let bearing = trig.angle(prey.pos[0] - home[0], prey.pos[1] - home[1]);
        let mut diff = (((bearing + 0x400) & 0x3ff) - (p.angle as i32 & 0x3ff)) as i16 as i32;
        if diff > 0x200 {
            diff -= 0x400;
        } else if diff < -0x200 {
            diff += 0x400;
        }
        match p.mode {
            1 => {
                p.angle = p.angle.wrapping_add(if diff > 0 { 2 } else { -2 });
                if diff.abs() <= 2 {
                    p.mode = 2;
                }
            }
            2 if diff.abs() > 0x3c => p.mode = 1,
            _ => {}
        }
        let s = |a: i32| trig.sin[(a & 0x3ff) as usize];
        let c = |a: i32| trig.cos[(a & 0x3ff) as usize];
        let z = home[2] + (s(p.bob as i32) * p.bob_amp >> 14) + p.lift;
        p.bob = p.bob.wrapping_add(20);
        let wob = (s(p.wobble as i32) * p.wobble_amp as i32) >> 14;
        p.wobble = p.wobble.wrapping_add(4);
        let a = wob + p.angle as i32;
        let at = [home[0] + (c(a) * p.radius >> 14), home[1] + (s(a) * p.radius >> 14), z];
        let out = filter3(&mut p.filter, at);
        // It faces Mendel.
        let d = [prey.pos[0] - out[0], prey.pos[1] - out[1], prey.pos[2] - out[2]];
        let (yaw, pitch) = trig.dir_angles(d);
        let (_, rot) = sim.pose(self.obj);
        sim.set_pose(self.obj, out, [pitch & 0x3ff, yaw & 0x3ff, rot[2]], mesh);
    }

    // ——— snapshot / saved game ———

    pub(crate) fn write(&self, w: &mut Writer) {
        w.u16(self.mode as u16);
        w.u8(self.started as u8);
        w.u8(self.effect as u8);
        w.u16(self.kind as u16);
        w.u16(self.variant as u16);
        w.u16(self.effect_left);
        write_mover(w, &self.mover);
        for f in self.filter.iter().chain(&self.heading).chain(&self.pole.filter) {
            write_filter(w, f);
        }
        self.centre.iter().for_each(|&c| w.i32(c));
        self.quiet.iter().for_each(|&q| w.u16(q));
        w.u8(self.contact as u8);
        let p = &self.pole;
        for v in [p.mode as i32, p.radius, p.angle as i32, p.bob_amp, p.bob as i32, p.wobble_amp as i32, p.wobble as i32, p.lift] {
            w.i32(v);
        }
    }

    pub(crate) fn read(&mut self, r: &mut Reader) -> Result<(), &'static str> {
        self.mode = r.u16()? as i16;
        self.started = r.u8()? != 0;
        self.effect = r.u8()? != 0;
        self.kind = r.u16()? as i16;
        self.variant = r.u16()? as i16;
        self.effect_left = r.u16()?;
        self.mover = read_mover(r)?;
        for f in self.filter.iter_mut().chain(&mut self.heading).chain(&mut self.pole.filter) {
            *f = read_filter(r)?;
        }
        for c in &mut self.centre {
            *c = r.i32()?;
        }
        for q in &mut self.quiet {
            *q = r.u16()?;
        }
        self.contact = r.u8()? != 0;
        let mut v = [0i32; 8];
        for x in &mut v {
            *x = r.i32()?;
        }
        let p = &mut self.pole;
        p.mode = v[0] as i16;
        p.radius = v[1];
        p.angle = v[2] as i16;
        p.bob_amp = v[3];
        p.bob = v[4] as i16;
        p.wobble_amp = v[5] as i16;
        p.wobble = v[6] as i16;
        p.lift = v[7];
        Ok(())
    }
}

fn write_mover(w: &mut Writer, m: &Mover) {
    m.pos.iter().chain(&m.delta).for_each(|&c| w.i32(c));
    [m.brake, m.stop_dist, m.total, m.remain].iter().for_each(|&c| w.i32(c));
    [m.max, m.accel, m.speed].iter().for_each(|&c| w.u16(c as u16));
    w.u8(m.moving as u8);
}

fn read_mover(r: &mut Reader) -> Result<Mover, &'static str> {
    let mut v = [0i32; 10];
    for x in &mut v {
        *x = r.i32()?;
    }
    let (max, accel, speed) = (r.u16()? as i16, r.u16()? as i16, r.u16()? as i16);
    Ok(Mover {
        pos: [v[0], v[1], v[2]],
        delta: [v[3], v[4], v[5]],
        brake: v[6],
        stop_dist: v[7],
        total: v[8],
        remain: v[9],
        max,
        accel,
        speed,
        moving: r.u8()? != 0,
    })
}

fn write_filter(w: &mut Writer, f: &Filter) {
    [f.d, f.alpha, f.beta, f.vel, f.out].iter().for_each(|&c| w.i32(c));
    w.u8(f.primed as u8);
}

fn read_filter(r: &mut Reader) -> Result<Filter, &'static str> {
    Ok(Filter { d: r.i32()?, alpha: r.i32()?, beta: r.i32()?, vel: r.i32()?, out: r.i32()?, primed: r.u8()? != 0 })
}
