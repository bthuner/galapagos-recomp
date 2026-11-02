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
//!   `Game_CreatureReact` @ 0x41cb80 → `creature+0xae8`): for `PBal` and
//!   `PFrk` a lethal surface of kind 2, so he dies and respawns. `PMan`
//!   (0x415930, on no CD section) is `PFrk`'s twin with `PBal`'s targeting,
//!   a long asymmetric body of probes and *harmless* flags: it only scares.
//! * **`PPol`** (0x4292b0, trigger 0x429430, update 0x429600) circles its
//!   home on the side Mendel is on, bobbing; `PBal` appears where it is.
//!
//! **Angles.** The exe's object matrix is `Math_RotationMatrix(yaw =
//! +0x72, pitch = +0x70, roll = +0x74)` (`0x4096a0` → `0x430ce0(+0xf2,
//! +0xf0, +0xf4)`, applied transposed by `0x409880` → `0x431ad0`), and
//! `sim::transform` reads `rot` as (yaw, pitch, roll); so a predator's
//! `rot` here is (`+0x72`, `+0x70`, `+0x74`), and a body's local +y is the
//! direction `Math_VectorAngles` (0x430c50) turned it to.
//!
//! Each probe and timer constant below comes from the constructors
//! (`0x415f50` base, `0x4150d0` PBal, `0x415590` PFrk, `0x415a40` PMan);
//! the per-class chase is `0x415220` (PBal), `0x4156a0` (PFrk) and
//! `0x415c40` (PMan).

use crate::camera::{approx_len, filter3, Filter, Mover, Trig};
use crate::sim::save::{Reader, Writer};
use crate::sim::{self, HostEvent, Sim};
use crate::transition::{self, Skin, Transition};
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
    /// `PBal` (0x415220) and `PMan` (0x415c40) go home when Mendel is
    /// camouflaged (affliction 4, `0x4198e0`); `PFrk` (0x4156a0, through
    /// `Game` 0x41c540) always follows him.
    fn spares_camouflage(self) -> bool {
        matches!(self, Class::Ball | Class::Man)
    }
    /// `PFrk` and `PMan` turn to face their lunge (their own tracker at
    /// `+0x354`, `Math_VectorAngles` 0x430c50); `PBal` just spins.
    fn faces(self) -> bool {
        matches!(self, Class::Fork | Class::Man)
    }
}

/// A hunter's constants (`+0x338..+0x346`, the probe table `+0x318`,
/// `+0x20c`, `+0x224`).
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
    /// The six probe tips in the body's frame (`+0x318`, `+0x314` = 6),
    /// each cast from the centre.
    probes: [I3; 6],
    /// `+0x346`: half the size of the box `0x4084e0` gathers candidate
    /// objects in (`± (broad + 400)` around the body, 0x41648f). Every
    /// probe tip lies inside it, so the port's own gather (creature.rs
    /// `Near`, sized by the probe) finds the same polygons.
    broad: i16,
    /// The surface flags it hands what it touches (`+0x20c`).
    flags: u32,
    /// Where it waits while vanished (`+0x224`).
    park: i32,
    /// Silence after a growl (`+0x23a`).
    growl_quiet: u16,
}

/// The base's probe table (0x415f50, also PBal's): `±r` along each axis.
const fn axes(r: i32) -> [I3; 6] {
    [[r, 0, 0], [-r, 0, 0], [0, r, 0], [0, -r, 0], [0, 0, r], [0, 0, -r]]
}

fn tuning(c: Class) -> Tuning {
    match c {
        // 0x4150d0. 0x405b2100 = hazard (bit 30) + lethal kind 2,
        // variant 3, then respawn reason 3, immune under affliction 1
        // (bit 22). No pain bit.
        Class::Ball => Tuning { accel: 30, max: 300, overshoot: 500, d: 0xe666, shift: 7, probes: axes(700), broad: 1000, flags: 0x405b_2100, park: 100_000, growl_quiet: 200 },
        // 0x415590: kind 2, variant 0. The probes stay the base's (±300).
        Class::Fork => Tuning { accel: 15, max: 150, overshoot: 4000, d: 0xc000, shift: 5, probes: axes(300), broad: 300, flags: 0x4058_2100, park: 1_500_000, growl_quiet: 15 },
        // 0x415a40: PFrk's mover and bounce, a stiffer tracker (0xd999),
        // a nearer park (150,000), and a body shaped by its own probe
        // table: a nose 1,554 ahead on its +y, two "arms" at (±999, 1,295),
        // a short fin up and down at 925 ahead (±296), a tail 1,406 behind.
        // Its flags 0x40000100 are only "a hazard he sees" (bit 30) and
        // bit 8, the creature's contact class: no lethal bits, no pain.
        Class::Man => Tuning {
            accel: 15,
            max: 150,
            overshoot: 4000,
            d: 0xd999,
            shift: 5,
            probes: [[0, 0x612, 0], [999, 0x50f, 0], [-999, 0x50f, 0], [0, 0x39d, 0x128], [0, 0x39d, -0x128], [0, -0x57e, 0]],
            broad: 0x640,
            flags: 0x4000_0100,
            park: 150_000,
            growl_quiet: 15,
        },
        // The base (0x415f50); `+0x20c` stays 0 from the Pred ctor. No CD
        // object is a bare hunter or a Pred that hunts.
        _ => Tuning { accel: 10, max: 100, overshoot: 5000, d: 0xcccc, shift: 5, probes: axes(300), broad: 300, flags: 0, park: 100_000, growl_quiet: 15 },
    }
}

/// The height a predator hands its transition effect (`0x424870(kind,
/// variant, 0x88)` at 0x42a40a / 0x42a510): the effect's floor is 0x88 − 10.
const EFFECT_HEIGHT: i16 = 0x88;
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
    /// `+0x210`: the effect (transition.rs) and its paint on our polygons.
    transition: Transition,
    skin: Skin,
    // ——— the chase (`+0x2b8` mover + tracker, `+0x348` target) ———
    mover: Mover,
    filter: [Filter; 3],
    /// `+0x2fc`: the tracker's output, the body's centre.
    centre: I3,
    /// `+0x354` (PFrk, PMan): the tracker smoothing its heading. Built
    /// once by the class ctor (d 0xf333, 0x401140) and never reset.
    heading: [Filter; 3],
    /// `+0x32c`: target − position, taken when the mover re-aims
    /// (0x416360); what the heading tracker is fed every tick.
    toward: I3,
    /// `+0xf0..+0xf4`: the angles `0x416360` copies at the end of each
    /// tick, from which `PMan`'s probe step (slot 0xb0 = 0x415c20)
    /// rebuilds the body matrix (`0x4096a0`) before probing.
    probe_rot: I3,
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
            transition: Transition::default(),
            skin: Skin::default(),
            mover: Mover::default(),
            filter: [Filter::default(); 3],
            centre: [0; 3],
            heading: {
                let mut f = Filter::default();
                f.reset(0xf333);
                [f; 3]
            },
            toward: [0; 3],
            probe_rot: [0; 3],
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
        if self.effect && self.run_effect(false, sim, mesh) {
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
        if self.run_effect(true, sim, mesh) {
            return;
        }
        self.mode = 3;
        self.started = false;
    }

    /// Steps the transition effect (`+0x210` slot 1); true while it runs.
    /// The first call starts it (`0x424870(kind, variant, 0x88)`, then
    /// `0x4249b0` to vanish or `0x4248c0` to appear). A burst spins the
    /// object's `+0x74` (our `rot[2]`).
    fn run_effect(&mut self, appear: bool, sim: &mut Sim, mesh: &mut WorldMesh) -> bool {
        let Some(draw) = mesh.draw_of_object.get(self.obj).copied().flatten() else { return false };
        let (pos, rot) = sim.pose(self.obj);
        let mut t = std::mem::take(&mut self.transition);
        let running = {
            let shape = || Skin::shape(mesh, draw);
            let rand = || sim.rand();
            let ctx = transition::Ctx { shape: &shape, spin: rot[2], rand: &rand };
            if !self.started {
                self.started = true;
                let (k, v) = (self.kind as u16, self.variant as u16);
                if appear {
                    t.start_reversed(k, v, EFFECT_HEIGHT, &ctx);
                } else {
                    t.start(k, v, EFFECT_HEIGHT, &ctx);
                }
            }
            t.update(&ctx)
        };
        if let Some(s) = t.spin() {
            sim.set_pose(self.obj, pos, [rot[0], rot[1], s & 0x3ff], mesh);
        }
        self.skin.paint(t.frame(), mesh, draw, sim);
        transition::emit(&mut t, sim, self.obj as i32 + 1);
        self.transition = t;
        running
    }

    /// Puts our own polygons back (a snapshot replaces this timeline).
    pub(crate) fn unpaint(&mut self, sim: &mut Sim, mesh: &mut WorldMesh) {
        if let Some(d) = mesh.draw_of_object.get(self.obj).copied().flatten() {
            self.skin.paint(None, mesh, d, sim);
        }
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
        let (_, rot) = sim.pose(self.obj);
        self.probe_rot = rot;
    }

    /// Mode 3 of the hunters (`0x415220` PBal, `0x4156a0` PFrk, `0x415c40`
    /// PMan), with the shared step `0x416360` and the probes `0x4160d0`
    /// (PMan: `0x415c20`, the same after rebuilding its matrix).
    fn chase(&mut self, sim: &mut Sim, mesh: &mut WorldMesh, prey: Option<Prey>) {
        let trig = trig();
        let t = tuning(self.class);
        let (pos, mut rot) = sim.pose(self.obj);
        // The target (`+0x348`). PBal and PMan leave a camouflaged Mendel
        // (affliction 4) alone and go home (`+0xd0`); PFrk always follows
        // him. (Both also test `mode == 3`: slot 0xa4 = 0x415df0 re-enters
        // the chase in mode 4, which nothing sets.)
        let target = match prey {
            Some(p) if !(self.class.spares_camouflage() && p.affliction == 4) => p.pos,
            _ => sim.rest_of(self.obj),
        };
        // 0x416360: when the mover has arrived, aim again, `overshoot`
        // past the target along the line from here; that line is also
        // the heading's input `+0x32c` until the next re-aim.
        self.mover.step();
        let mut out = filter3(&mut self.filter, self.mover.pos);
        if !self.mover.moving {
            let d = [target[0] - pos[0], target[1] - pos[1], target[2] - pos[2]];
            self.toward = d;
            let len = approx_len(d).max(1);
            let aim = [0, 1, 2].map(|k| target[k] + t.overshoot * d[k] / len);
            self.mover.set_target(aim);
        }
        // 0x4160d0: the six probes of the class's table, turned by the
        // body (`0x409880`), each from the current centre (a push moves
        // the centre for the probes after it). PMan's slot 0xb0 rebuilds
        // its matrix from `+0xf0`, last tick's copy of the angles (taken
        // before that tick's turn); inferred: the others' matrix is the
        // current pose (theirs is refreshed elsewhere, and their probe
        // sets are symmetric).
        let body = if self.class == Class::Man { self.probe_rot } else { rot };
        self.probe_rot = rot;
        self.contact = false;
        for off in t.probes {
            // The broad phase (0x4084e0) would miss nothing beyond the tip.
            debug_assert!(off.iter().all(|c| c.abs() <= t.broad as i32 + 400));
            let m = sim::transform(out, body);
            let from = out.map(|c| c as f32);
            let to = sim::apply(&m, off.map(|c| c as f32));
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
        if self.class.faces() {
            // PFrk / PMan face where they lunge: `+0x32c` smoothed by their
            // own tracker (0x401300), then `Math_VectorAngles` (0x430c50)
            // writes yaw to `+0x72` and pitch to `+0x70`; then `+0x74`
            // rolls by 10. In our angle order (module docs) the nose (+y)
            // points along the lunge, as in the exe.
            let s = filter3(&mut self.heading, self.toward);
            let (yaw, pitch) = trig.dir_angles(s);
            rot = [yaw & 0x3ff, pitch & 0x3ff, rot[2] + 10];
        } else {
            // PBal: (+0x70, +0x72, +0x74) += (−20, +20, +20) (0x41528f),
            // in our order (+0x72, +0x70, +0x74).
            rot = [rot[0] + 20, rot[1] - 20, rot[2] + 20];
        }
        sim.set_pose(self.obj, out, rot.map(|r| r & 0x3ff), mesh);
        // Sounds (predator bank, identical in all three): a growl when
        // Mendel is nearer than 1,000 to where it now is (`+0xe4`, which
        // 0x416360 just set) (cue `+0x240`), a bite on contact (cue
        // `+0x27c`), each followed by its silence.
        if self.quiet[0] == 0 {
            if let Some(p) = prey {
                let d = [p.pos[0] - out[0], p.pos[1] - out[1], p.pos[2] - out[2]];
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
        // It faces Mendel (0x4298ea: yaw to `+0x72`, pitch to `+0x70`, then
        // its matrix at once, 0x42990e), in our angle order (module docs).
        let d = [prey.pos[0] - out[0], prey.pos[1] - out[1], prey.pos[2] - out[2]];
        let (yaw, pitch) = trig.dir_angles(d);
        let (_, rot) = sim.pose(self.obj);
        sim.set_pose(self.obj, out, [yaw & 0x3ff, pitch & 0x3ff, rot[2]], mesh);
    }

    // ——— snapshot / saved game ———

    pub(crate) fn write(&self, w: &mut Writer) {
        w.u16(self.mode as u16);
        w.u8(self.started as u8);
        w.u8(self.effect as u8);
        w.u16(self.kind as u16);
        w.u16(self.variant as u16);
        w.u16(0); // was a blink timer; the effect restarts nothing on load
        write_mover(w, &self.mover);
        for f in self.filter.iter().chain(&self.heading).chain(&self.pole.filter) {
            write_filter(w, f);
        }
        self.centre.iter().for_each(|&c| w.i32(c));
        self.quiet.iter().for_each(|&q| w.u16(q));
        w.u8(self.contact as u8);
        self.toward.iter().chain(&self.probe_rot).for_each(|&c| w.i32(c));
        let p = &self.pole;
        for v in [p.mode as i32, p.radius, p.angle as i32, p.bob_amp, p.bob as i32, p.wobble_amp as i32, p.wobble as i32, p.lift] {
            w.i32(v);
        }
    }

    /// `version` is the saved game's (save.rs): 4 added `toward` and
    /// `probe_rot`; older saves leave them zero.
    pub(crate) fn read(&mut self, r: &mut Reader, version: u32) -> Result<(), &'static str> {
        self.mode = r.u16()? as i16;
        self.started = r.u8()? != 0;
        self.effect = r.u8()? != 0;
        self.kind = r.u16()? as i16;
        self.variant = r.u16()? as i16;
        r.u16()?;
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
        if version >= 4 {
            for c in self.toward.iter_mut().chain(&mut self.probe_rot) {
                *c = r.i32()?;
            }
        }
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

/// `PMan` against the CD. No section has one, so the tests take section
/// 2's fork `PFrk` #130 and re-class it, as if the factory had read
/// `PMan` for it (the classes share the `Pred` layout and trigger). They
/// skip themselves without data.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::creature::{Creature, Mode};
    use galapagos_formats::creature::{overlay_for_age, Brain, CreatureBody};
    use galapagos_formats::worlds::Archive;

    fn data() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds")).ok()
    }

    /// Section 2's fork #130 (list index 129).
    const FORK: usize = 130 - 1;

    /// Section 2 with Mendel spawned by its `Strt`, the fork re-classed.
    fn section2(d: &[u8], class: Class) -> (Sim, WorldMesh, usize) {
        let a = Archive::parse(d).unwrap();
        let section = a.world(2).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mut mesh);
        let body = CreatureBody::parse(d, a.header.creature as usize).unwrap();
        let brain = Brain::parse(d, a.header.creature_overlays[overlay_for_age(128).unwrap()] as usize).unwrap();
        sim.creature = Some(Creature::new(&section, &mesh, body, brain));
        for _ in 0..20 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().mode == Mode::Alive {
                break;
            }
        }
        assert_eq!(sim.creature.as_ref().unwrap().mode, Mode::Alive, "Mendel spawned");
        let k = sim.predators.iter().position(|p| p.obj == FORK).expect("#130 is a predator");
        assert_eq!(sim.predators[k].class, Class::Fork, "#130 is the fork");
        sim.predators[k] = Predator::new(FORK, class);
        (sim, mesh, k)
    }

    fn prey(sim: &Sim, affliction: u8) -> Prey {
        Prey { affliction, ..sim.creature.as_ref().unwrap().prey().unwrap() }
    }

    const fn shorts(s: [i16; 4]) -> [i16; 10] {
        [s[0], s[1], s[2], s[3], 0, 0, 0, 0, 0, 0]
    }

    /// Every section's objects, by class: one `PBal`, `PPol`, `PFrk`
    /// (section 2), four `Pred` (section 7), and no `PMan` anywhere.
    #[test]
    fn no_pman_on_the_cd() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let mut found: Vec<(usize, usize, Class)> = Vec::new();
        for s in 0..a.sections.len() {
            for (i, o) in a.world(s).unwrap().objects.iter().enumerate() {
                if let Some(c) = Class::of(&o.kind()) {
                    found.push((s, i + 1, c));
                }
            }
        }
        let count = |c: Class| found.iter().filter(|f| f.2 == c).count();
        println!("predators on the CD: {found:?}");
        assert_eq!(count(Class::Man), 0, "no PMan in any section");
        assert_eq!((count(Class::Ball), count(Class::Pole), count(Class::Fork), count(Class::Pred)), (1, 1, 1, 4));
        assert!(found.contains(&(2, 130, Class::Fork)));
    }

    /// The constructor (0x415a40): every probe tip lies inside the broad
    /// phase box `± (0x640 + 400)`, and the body is longest along +y.
    #[test]
    fn pman_tuning() {
        let t = tuning(Class::Man);
        assert_eq!((t.accel, t.max, t.overshoot, t.d, t.shift), (15, 150, 4000, 0xd999, 5));
        assert_eq!((t.flags, t.park, t.growl_quiet), (0x4000_0100, 150_000, 15));
        assert_eq!(t.flags & 0xf000, 0, "no lethal kind: PMan never kills");
        assert_eq!(t.flags & 0x100_0000, 0, "no pain");
        for p in t.probes {
            assert!(p.iter().all(|c| c.abs() <= t.broad as i32 + 400));
        }
        assert_eq!(t.probes.iter().map(|p| p[1]).max(), Some(1554), "the nose");
        assert_eq!(t.probes.iter().map(|p| p[1]).min(), Some(-1406), "the tail");
    }

    /// The targeting of 0x415c40 and the re-aim of 0x416360, tick by tick
    /// against the exe's formulas: the target is Mendel (home when he is
    /// camouflaged, unlike PFrk), the mover aims `4000·d/approx_len(d)`
    /// past it, the heading input `+0x32c` is `d` and holds until the next
    /// re-aim, and `+0x74` rolls by 10 a tick.
    #[test]
    fn pman_aims_past_mendel_and_spares_camouflage() {
        let Some(d) = data() else { return };
        for (class, affliction, homes) in [(Class::Man, 0, false), (Class::Man, 4, true), (Class::Fork, 4, false)] {
            let (mut sim, mut mesh, k) = section2(&d, class);
            let mendel = prey(&sim, affliction);
            let start = [mendel.pos[0] + 6000, mendel.pos[1] - 3000, mendel.pos[2] + 500];
            let (_, rot0) = sim.pose(FORK);
            let mut p = sim.predators[k].clone();
            p.mode = 3;
            sim.set_pose(FORK, start, rot0, &mut mesh);
            p.setup_chase(&mut sim, &mut mesh);
            p.tick(&mut sim, &mut mesh, Some(mendel));
            let target = if homes { sim.rest_of(FORK) } else { mendel.pos };
            let dd = [0, 1, 2].map(|i| target[i] - start[i]);
            let len = approx_len(dd);
            let aim = [0, 1, 2].map(|i| target[i] + 4000 * dd[i] / len);
            assert_eq!(p.toward, dd, "{class:?} aff {affliction}: +0x32c = target − position");
            assert_eq!(p.mover.delta, [0, 1, 2].map(|i| aim[i] - start[i]), "{class:?}: the lunge overshoots by 4000");
            assert!(p.mover.moving);
            let (_, rot1) = sim.pose(FORK);
            assert_eq!(rot1[2], (rot0[2] + 10) & 0x3ff, "the roll");
            // While the lunge runs the heading input does not change.
            let held = p.toward;
            for _ in 0..5 {
                p.tick(&mut sim, &mut mesh, Some(Prey { pos: [0; 3], ..mendel }));
                assert_eq!(p.toward, held, "held until the next re-aim");
            }
        }
    }

    /// The chase in the running game: `PMan` lunges at Mendel, touches
    /// him (its flags land on `MEnc`), plays its bite, faces where it
    /// lunges with its nose (+y), and he lives: its flags are only a
    /// scare. The same scene with the class left `PFrk` kills him. (Its
    /// nose strikes him from ~1,300 away, outside the growl distance.)
    #[test]
    fn pman_chases_bites_and_only_scares() {
        let Some(d) = data() else { return };
        for class in [Class::Man, Class::Fork] {
            let (mut sim, mut mesh, k) = section2(&d, class);
            sim.wake_all = true;
            let deaths = sim.creature.as_ref().unwrap().stats.deaths;
            // Appear without an effect (`[2, 0, 0, 0]`), then start the
            // lunge 3,000 over him (on his side of the upside-down floor
            // he stands on) and 1,500 aside.
            let mut p = sim.predators[k].clone();
            p.trigger(shorts([2, 0, 0, 0]), [0; 10], &mut sim, &mut mesh);
            assert_eq!(p.mode, 2, "appearing");
            p.tick(&mut sim, &mut mesh, None);
            assert_eq!(p.mode, 3, "no effect: active at once");
            let at = prey(&sim, 0).pos;
            let up = sim.creature.as_ref().unwrap().up;
            let start = [0, 1, 2].map(|i| at[i] + [1500, 0, 0][i] + (up[i] * 3000.0) as i32);
            let (_, rot) = sim.pose(FORK);
            sim.set_pose(FORK, start, rot, &mut mesh);
            p.setup_chase(&mut sim, &mut mesh);
            sim.predators[k] = p;
            sim.events.clear();
            let (mut closest, mut touched, mut facing) = (f32::MAX, None, f32::MIN);
            let (mut died, mut felt) = (false, None);
            let menc = prey(&sim, 0).menc.expect("section 2 has its MEnc");
            for t in 0..120 {
                sim.step(&mut mesh);
                let p = &sim.predators[k];
                if p.mode != 3 {
                    break; // its own machine sent it away
                }
                let c = sim.creature.as_ref().unwrap();
                died |= c.stats.deaths > deaths;
                let (pos, rot) = sim.pose(FORK);
                let off = [0, 1, 2].map(|i| c.pos[i] - pos[i] as f32);
                closest = closest.min((off[0] * off[0] + off[1] * off[1] + off[2] * off[2]).sqrt());
                if p.contact && touched.is_none() {
                    touched = Some(t);
                }
                // `WorldObject_PostEvent` left the predator's flags on MEnc.
                let flags = tuning(class).flags;
                if felt.is_none() && sim.capture().objs[menc].events & flags == flags {
                    felt = Some(t);
                }
                if t == 30 {
                    // Its nose (+y) turned along the lunge.
                    let m = sim::transform([0; 3], rot);
                    let nose = sim::apply(&m, [0.0, 1.0, 0.0]);
                    let w = p.toward.map(|c| c as f32);
                    let n = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt().max(1.0);
                    facing = (nose[0] * w[0] + nose[1] * w[1] + nose[2] * w[2]) / n;
                    // A saved game carries its state (GLPS v4).
                    sim.take_snapshot();
                    let bytes = sim.save_game();
                    let (mut other, mut other_mesh, _) = section2(&d, class);
                    other.load_game(&bytes, &mut other_mesh).unwrap();
                    assert!(other.predators == sim.predators, "predators restored");
                    assert!(other.save_game() == bytes, "load then save is the identity");
                }
            }
            let sounds: Vec<(i32, i32)> =
                sim.events.iter().filter(|e| e.kind == sim::EV_PREDATOR_SOUND && e.b == FORK as i32 + 1).map(|e| (e.a, e.c)).collect();
            println!("{class:?}: closest {closest:.0}, touched at {touched:?}, MEnc felt it at {felt:?}, facing {facing:.2}, died {died}, sounds {sounds:?}");
            assert!(closest < 1600.0, "{class:?} reaches him");
            assert!(touched.is_some(), "{class:?}: a probe touches");
            assert!(felt.is_some(), "{class:?}: its flags reach Mendel's click hull");
            assert!(sounds.contains(&(1, 0x8000)), "{class:?}: the bite, half pitch");
            assert!(facing > 0.9, "{class:?}: faces its lunge ({facing:.2})");
            match class {
                Class::Man => assert!(!died && sim.creature.as_ref().unwrap().mode == Mode::Alive, "PMan only scares"),
                _ => assert!(died, "PFrk kills"),
            }
        }
    }

    /// The growl timer (0x415ce8..0x415d91): a growl whenever Mendel is
    /// nearer than 1,000 and the timer is out, then 15 silent ticks;
    /// nothing farther. The distance is `Math_ApproxLength` (max − max/16
    /// along an axis), so 1,000 straight ahead still growls; 1,100 does
    /// not. (Far above the section, where no probe touches anything.)
    #[test]
    fn pman_growls_every_16_ticks() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh, k) = section2(&d, Class::Man);
        let mut p = sim.predators[k].clone();
        p.mode = 3;
        let far = [0, 0, 5_000_000];
        let (_, rot) = sim.pose(FORK);
        sim.set_pose(FORK, far, rot, &mut mesh);
        p.setup_chase(&mut sim, &mut mesh);
        let mendel = prey(&sim, 0);
        let mut growls = Vec::new();
        for t in 0..40 {
            // Mendel stays 900 ahead of where it will be (1,100 from tick 30).
            let gap = if t < 30 { 900 } else { 1100 };
            let (mut mover, mut f) = (p.mover, p.filter);
            mover.step();
            let next = filter3(&mut f, mover.pos);
            sim.events.clear();
            p.tick(&mut sim, &mut mesh, Some(Prey { pos: [next[0] + gap, next[1], next[2]], ..mendel }));
            assert_eq!(sim.pose(FORK).0, next, "it went where its mover and tracker said");
            assert!(!p.contact, "nothing to touch up here");
            for e in sim.events.iter().filter(|e| e.kind == sim::EV_PREDATOR_SOUND) {
                assert_eq!((e.a, e.b, e.c), (0, FORK as i32 + 1, 0x10000), "the growl: sound 1, full pitch");
                growls.push(t);
            }
        }
        assert_eq!(growls, [0, 16], "growl, 15 quiet ticks, growl; none at 1,100");
    }

    /// Vanish and appear with an effect (`Pred`'s 0x42a3e0 / 0x42a4b0,
    /// shared): the fork's own triggers `[2, 1, 2, 0]` (burst in reverse,
    /// 60 ticks) then `[1, 1, 2, 0]` (burst, 100 ticks), after which
    /// `PMan` is parked at its own 150,000.
    #[test]
    fn pman_appears_and_vanishes() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh, k) = section2(&d, Class::Man);
        let mut p = sim.predators[k].clone();
        let mendel = prey(&sim, 0);
        p.trigger(shorts([2, 1, 2, 0]), [0; 10], &mut sim, &mut mesh);
        let mut appear = 0;
        while p.mode == 2 && appear < 500 {
            p.tick(&mut sim, &mut mesh, Some(mendel));
            appear += 1;
        }
        assert_eq!(p.mode, 3, "active after the effect");
        assert_eq!(sim.pose(FORK).0, sim.rest_of(FORK), "appeared at home");
        p.tick(&mut sim, &mut mesh, Some(mendel));
        p.trigger(shorts([1, 1, 2, 0]), [0; 10], &mut sim, &mut mesh);
        let mut vanish = 0;
        while p.mode == 1 && vanish < 500 {
            p.tick(&mut sim, &mut mesh, Some(mendel));
            if vanish == 3 {
                // A trigger is ignored while an effect runs.
                p.trigger(shorts([2, 1, 2, 0]), [0; 10], &mut sim, &mut mesh);
                assert_eq!(p.mode, 1, "ignored while vanishing");
            }
            vanish += 1;
        }
        println!("PMan: appeared in {appear} ticks, vanished in {vanish}");
        assert_eq!(p.mode, 0);
        assert!((55..=65).contains(&appear), "{appear} ticks appearing");
        assert!((95..=105).contains(&vanish), "{vanish} ticks vanishing");
        assert_eq!(sim.pose(FORK).0, [150_000; 3], "parked at PMan's own +0x224");
    }
}
