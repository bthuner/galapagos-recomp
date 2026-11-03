//! Mendel, alive: body, mover, senses, NERM brain and learning, ticked once
//! per simulation step right after the objects (`Game_Tick` @ 0x41bc50:
//! objects → creature → camera).
//!
//! The data side (mesh, key frames, brains, the Markov chain and the
//! genetic algorithm) lives in `galapagos_formats::creature`; this module
//! wires those pieces into a creature that walks around a running world.
//! docs/re/mendel.md is the spec; the exe addresses below point into it.
//!
//! One tick of a living Mendel (`Creature_Update` @ 0x417da0):
//!
//! ```text
//! mover      walk in the floor's plane, hull vs walls, find the floor  0x417f40 (walker.rs)
//! touch      hull points that hit something → touch inputs, event 1  0x418450
//! senses     two infrared eyes, three rays each                       0x418b00
//! think      three NERM controllers → three slew-limited actuators    0x425dc0
//! motor      actuators → walking speed, turn rate, stance, pose       0x4189c0
//! learn      running costs; every 65,536 cost-ticks the GA steps      0x419470
//! effects    the running affliction (affliction.rs)                   0x405fc0
//! surface    lethal floors, pain, delayed hazards, afflictions        0x419550
//! ```
//!
//! Coordinates: world space is the section's (Z up, but Mendel does not
//! care: his up is the normal of the polygon he stands on, see
//! walker.rs). His heading turns in that polygon's plane, so a growing
//! heading turns towards his left. Mendel's model space, after the
//! load-time axis swap of `BodyPart::transform`, is `x` forward, `y` down
//! (towards his feet), `z` left; his origin floats `h` units above the
//! floor (41 + stance·166/256 at rest: exactly where the posed feet end,
//! checked on the CD data), so `world = pos + R · model` with
//! `R = (forward, −up, left)`.

use crate::affliction::{Afflictions, Ctx, NORMAL};
use crate::predator::Prey;
use crate::sim::save::{Reader, Writer};
use crate::sim::{self, Request, Sim};
use crate::transition::{self, Transition};
use crate::walker::{
    self, add, cross, dot, length, local_polys, normalize, ray_tri, scale, sub, turns, Drive, Faces, Frame, LocalPoly, Mover, Near, Tri,
    FLOOR, HAZARD, IDENTITY, SOLID, UP, V3,
};
use crate::world::{self, Light, WorldMesh, FLOATS_PER_VERTEX};
use galapagos_formats::creature::{Brain, CreatureBody, Lcg};
use galapagos_formats::worlds::Section;

// Gravity: grounded, the mover adds −7 to the speed of his height above
// the floor; airborne it adds `up · −0x700 >> 10` to a velocity kept in
// 1/256 units: −7 units/tick² both ways (`CreatureMover_Integrate` @
// 0x418060, verified in the listing). Afflictions 10..12 change it, and
// afflictions 3 and 7..9 change the gait (24): see affliction.rs.
/// A right click is felt for this many ticks (`MEnc_PopTouchSide` @ 0x4261a0).
const TOUCH_TICKS: u8 = 4;
/// Ticks of contact before a "delayed" (0x800000) lethal surface fires.
const DELAY_TICKS: u16 = 80;
/// Pain: added to all three brain costs (`Creature_HandleSurfaceEvents` @ 0x419550).
const PAIN: i64 = 500 << 16;

/// Radius around Mendel inside which polygons are gathered each tick: the
/// longest ray (eye range 2,000 + 256) plus the hull, with margin.
const GATHER_RADIUS: f32 = 3200.0;

// ——— Polygon flags (docs/re/objects.md §3, docs/re/mendel.md §2.3) ———
/// What an eye ray hits (mask 0x40000100 at `0x419122`) and what of it
/// it sees: IR-visible (0xc0) or hazard polygons.
const IR_VISIBLE: u32 = 0xc0;
const LETHAL: u32 = 0xf000;
const DELAYED: u32 = 0x80_0000;
const HURTS: u32 = 0x100_0000;
/// Under affliction 1 a lethal surface with bit 22 is harmless, and an
/// affliction surface with bit 29 afflicts no more (0x41955b..0x419590).
const SHIELDED: u32 = 0x40_0000;
const UNAFFLICTING: u32 = 0x2000_0000;
const AFFLICTION_BITS: u32 = 0x1e00_0000;
/// Falling longer than 65 ticks (130 if the last surface had bit 4) makes
/// the game inject this surface: lethal kind 1, then death (0x41960a).
const FELL_TOO_LONG: u32 = 0x18_1000;
/// Frame flag bit 5: a save pad ("blue, shimmering floor panel").
const SAVE_PAD: u32 = 0x20;
/// The eye rays: start heights above his feet, along his up (0x418b00:
/// `up/4`, `up/64`, `−up/32` from `pos − up·stance_height/1024`).
const EYE_HIGH: f32 = 256.0;
const EYE_MID: f32 = 16.0;
const EYE_LOW: f32 = -32.0;
/// A hit's distance counts from 656 units (`- 0x290`, 0x41915c).
const EYE_NEAR: i32 = 656;
/// The ledge ray: a gap of at least this between two edge hits is a drop
/// (`< 0xfb` keeps looking, 0x4192c8).
const LEDGE_GAP: i32 = 251;

// ——— Actuators ——————————————————————————————————————————————————————

/// A slew-limited servo in 16.16 fixed point (`Actuator_ctor` @ 0x42aa00,
/// `Actuator_Step` @ 0x42aa50). It chases `target` with a velocity capped
/// at `rate` units per tick, braking as it approaches: the brain's twitchy
/// 0..256 decisions become smooth muscle movements.
#[derive(Clone, Debug)]
struct Actuator {
    rate: i32,
    shift: u32,
    pos: i32,
    vel: i32,
    last_dt: i32,
    target: i32,
    value: i32,
}

impl Actuator {
    fn new(rate: i32, start: i32) -> Self {
        Self { rate, shift: 3, pos: start << 16, vel: 0, last_dt: 256, target: start << 16, value: start }
    }

    fn set_target(&mut self, v: i32) {
        self.target = v << 16;
    }

    fn step(&mut self, bias: i32, dt: i32) {
        if dt == 0 {
            return;
        }
        let vmax = ((((self.rate as i64) << 16) * dt as i64) >> 8) as i32;
        self.vel = (((((self.vel as i64) << 8) / self.last_dt as i64) * dt as i64) >> 8) as i32;
        self.last_dt = dt;
        let base = vmax;
        let v = self.vel;
        let acc = match self.target - self.pos {
            d if d > 0 && v < vmax => (base - if v > 0 { v } else { 4 * v }) >> self.shift,
            d if d < 0 && v > -vmax => (if v < 0 { -(base + v) } else { -base - 4 * v }) >> self.shift,
            _ => 0,
        };
        self.vel = (self.vel + (((acc - bias) as i64 * dt as i64) >> 8) as i32).clamp(-vmax, vmax);
        self.pos += self.vel;
        if self.pos > 1 << 24 {
            self.pos = 1 << 24;
        } else if self.pos < 0 {
            self.pos = 0;
            self.vel = 0;
        }
        self.value = self.pos >> 16;
    }
}


// ——— Probes for the other modules ————————————————————————————————————

/// What a predator's probe touched: the object, and the push that moves
/// the predator out of it (`None` when `0x408c50` refuses the hit).
#[derive(Clone, Copy, Debug)]
pub struct ProbeHit {
    pub obj: usize,
    pub push: Option<[i32; 3]>,
}

/// What an eye sees (`Creature_CastIrRay` @ 0x4190f0): the nearest polygon
/// of mask 0x40000100, if it is IR-visible (0xc0) or a hazard.
fn is_seen(flags: u32) -> bool {
    flags & (IR_VISIBLE | HAZARD) != 0
}

// ——— The creature ———————————————————————————————————————————————————

/// What Mendel is doing, `creature` mode byte (`Creature_Update` @ 0x417da0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// Not spawned yet: the section's `Strt` has not fired.
    Absent,
    /// Rematerialising (mode 1), ticks left.
    Appearing(u16),
    /// Alive (mode 2).
    Alive,
    /// Dying or teleporting (mode 0): the effect runs, then `then` happens.
    Vanishing(u16, After),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum After {
    /// Respawn reason 3: back to the respawn point.
    Respawn,
    /// Reasons 1/2: a teleporter's destination (`Game+0x17efd6..`).
    Teleport(Place),
    /// Reasons 4/5: the level ends (`0x41c8c0`); the host gets the event.
    /// (A saved game keeps it as a respawn.)
    LevelEnd(sim::HostEvent),
}

/// A spot for Mendel the way the game stores one (`Game+0x17efc4..`, the
/// respawn point, and `+0x17efd6..`, the pending teleport): an object, a
/// polygon, `(u, v, h)` in that polygon's frame, and a heading in its
/// plane, i.e. the arguments of `Mover_Place` @ 0x411bf0. `on = None`
/// means `at` is a world position (a portal whose polygon is missing, or
/// a respawn point from a version 1–2 saved game).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    pub on: Option<(usize, usize)>,
    pub at: V3,
    pub heading: i32,
}

impl Place {
    fn of_request(at: V3, heading: i32, on: Option<sim::PortalPlace>) -> Place {
        match on {
            Some(p) => Place { on: Some((p.obj, p.poly)), at: p.local, heading },
            None => Place { on: None, at, heading },
        }
    }
}

pub struct Creature {
    body: CreatureBody,
    pub brain: Brain,
    rng: Lcg,
    polys: Vec<Vec<LocalPoly>>,
    menc: Option<usize>,
    /// The section's light and materials: his polygons are lit by the
    /// section colour ramps their records name (4 or 8).
    light: Light,
    materials: Vec<[u16; 6]>,
    pub mode: Mode,

    /// The polygon walker (`creature+0x50`), walker.rs.
    pub mover: Mover,
    /// Derived each tick from the mover, for the senses and the other
    /// modules: his origin and up (the floor's normal, `+0x24`) in world
    /// space. How he is *drawn* is the mover's render angles.
    pub pos: V3,
    pub up: V3,
    /// Distance and turn of this tick (`creature+0x7d0`, `+0x7d2`).
    forward: i32,
    turn_d: i32,
    /// Stance height (`+0x7d4`, the mover's `+0x784`): 41..207.
    stance_height: i32,
    surface: u32,
    delay: u16,

    // Agent (`Agent_ctor` @ 0x425a80).
    /// Left eye/touch (`agent+4`) and right (`agent+8`), 0..=256.
    pub left: i32,
    pub right: i32,
    touch: Option<(bool, u8)>,
    outputs: [i32; 3],
    act: [Actuator; 3],
    cost: [i64; 3],
    count: [i64; 3],
    fwd_acc: i64,
    turn_acc: i64,
    phase: u16,
    stance: i32,
    turn: i32,
    side: bool,

    /// The respawn point (`Game+0x17efc4..0x17efd4`): the last spawn
    /// request, or where he stood when the last world snapshot was taken.
    respawn: Option<Place>,
    /// `creature+0xaf0`: the surface he touches this tick has frame flag
    /// bit 5 (0x20), i.e. he stands on a save pad (`0x419730`).
    pub on_save_pad: bool,

    /// The posed mesh in world space, `world::FLOATS_PER_VERTEX` per vertex.
    pub vertices: Vec<f32>,
    /// The mesh of the tick before, for the renderer's in-between frames
    /// ([`Creature::vertices_at`]); empty after a cut.
    prev_vertices: Vec<f32>,
    /// Teaching/debug counters.
    pub stats: Stats,

    /// Sound cue inputs gathered during the tick: flags of every polygon an
    /// eye ray hit, and whether he landed after a fall (event 4).
    seen_flags: u32,
    landed: bool,
    /// Silence timers (ticks): `[0]` the landing cue's (`+0xa32`), `[1]`
    /// the one the four other cues share (`+0xa30`); `[2..]` unused.
    cue_quiet: [u16; 5],
    /// A ray of this tick hit a `0x180000`-class polygon (event 8).
    seen_class: bool,

    // ——— Afflictions and hazards ———
    /// The active effect manager (`creature+0x8e8`), affliction.rs.
    pub afflictions: Afflictions,
    /// `creature+0xae8`: surface flags handed over by the game (a
    /// predator's bite through `MEnc`, `Game_CreatureReact` @ 0x41cb80),
    /// felt with the next touch.
    injected: u32,
    /// `+0x8f4` / `+0x8f6`: airborne ticks and the fall limit (0 = unset).
    fall_count: u16,
    fall_limit: u16,
    /// `+0xafc`, `+0xafe`: affliction 6's held sensor noise.
    noise: [i32; 2],
    /// The section's last texture layer (affliction 1's skin).
    last_texture: i16,
    /// `creature+0x8ec`: the dematerialise/materialise effect
    /// (transition.rs), what modes 0 and 1 wait for.
    transition: Transition,
}

/// Mendel's five sound cues (`Creature_ReadSoundCues` @ 0x417ab0, played by
/// `Creature_HandleSurfaceEvents` @ 0x419550): the 1-based sounds of the
/// creature bank one cue picks from, and its minimum silence after
/// playing (plus `0..31` ticks from the brain's LCG). Order: landing
/// (own timer), then by priority agitated, saw a hazard, saw a
/// `0x180000`-class surface, strong left/right asymmetry (one shared
/// timer, one cue per tick at most).
const SOUND_CUES: [(&[i16], u16); 5] = [(&[5], 5), (&[1, 2], 5), (&[3, 4], 20), (&[6, 7], 20), (&[8, 9, 10, 11], 20)];
/// Event 16: `|L - R| > 230` (`Creature_SenseInfrared` @ 0x418b00).
const ASYMMETRY: i32 = 230;

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub ticks_alive: u32,
    pub walked: f32,
    pub touches_posted: u32,
    pub deaths: u32,
    pub teleports: u32,
    pub evolutions: u32,
}


impl Creature {
    /// `LoadCreature` @ 0x41cde0, minus the parts that belong to the host
    /// (sounds). The brain is whatever the caller chose: an overlay from the
    /// CD (`Game_LoadCreatureBrainForSection` @ 0x41ced0) or the brain he
    /// carried out of the previous section.
    pub fn new(section: &Section, mesh: &WorldMesh, body: CreatureBody, brain: Brain) -> Self {
        let polys = local_polys(section);
        let menc = section.objects.iter().position(|o| o.kind() == *b"MEnc");
        // Affliction 5's eyes (`0x414e10`): the polygons sorted by their mean
        // x; inferred: the four most forward ones, at rest.
        let rest = body.pose_turning(0, 128, 0, true);
        let mut by_x: Vec<(i64, usize)> = rest
            .faces
            .iter()
            .enumerate()
            .map(|(k, f)| {
                let n = f.indices.len().max(1) as i64;
                (f.indices.iter().map(|&i| rest.vertices[i as usize][0] as i64).sum::<i64>() / n, k)
            })
            .collect();
        by_x.sort_by(|a, b| b.0.cmp(&a.0));
        let eyes = core::array::from_fn(|k| by_x.get(k).map_or(0, |e| e.1));
        let afflictions = Afflictions::new(rest.faces.len(), eyes);
        // Engine's load-time pose: SetPose(128, 1, 128, 0, 0).
        let stance = 128;
        let mut c = Creature {
            body,
            brain,
            rng: Lcg(1), // inferred: the engine-wide LCG's seed at load is not known
            polys,
            menc,
            light: Light::of(&section.environment),
            materials: section.color_ramps.clone(),
            mode: Mode::Absent,
            mover: Mover::default(),
            pos: [0.0; 3],
            up: UP,
            forward: 0,
            turn_d: 0,
            stance_height: stance_height(stance),
            surface: 0,
            delay: DELAY_TICKS,
            left: 0,
            right: 0,
            touch: None,
            outputs: [stance, 128, 128],
            // act38 = (13, 256) drives posture, act3c/act40 = (20, 128) speed/turn.
            act: [Actuator::new(13, 256), Actuator::new(20, 128), Actuator::new(20, 128)],
            cost: [0; 3],
            count: [0; 3],
            fwd_acc: 0,
            turn_acc: 0,
            phase: 0,
            stance,
            turn: 0,
            side: true,
            respawn: None,
            on_save_pad: false,
            vertices: Vec::new(),
            prev_vertices: Vec::new(),
            stats: Stats::default(),
            seen_flags: 0,
            landed: false,
            cue_quiet: [0; 5],
            seen_class: false,
            afflictions,
            injected: 0,
            fall_count: 0,
            fall_limit: 0,
            noise: [0; 2],
            last_texture: mesh.texture_count as i16 - 1,
            transition: Transition::default(),
        };
        c.act[0] = Actuator::new(13, stance);
        c
    }

    /// A right click on his left (`true`) or right side: held for 4 ticks.
    pub fn touch(&mut self, left: bool) {
        self.touch = Some((left, TOUCH_TICKS));
    }

    /// The floor he stands on (object, polygon), if grounded.
    pub fn ground(&self) -> Option<(usize, usize)> {
        self.mover.on.filter(|_| self.mover.grounded)
    }

    /// The polygon whose frame he lives in (`Mover_GetSurface` @ 0x4124a0):
    /// the floor he stands on, or the last one while airborne.
    pub fn surface(&self) -> Option<(usize, usize)> {
        self.mover.on
    }

    pub fn outputs(&self) -> [i32; 3] {
        [self.act[0].value, self.act[1].value, self.act[2].value]
    }

    /// His heading in 1024ths of a turn, in the plane of his polygon
    /// (`+0x50`; on a floor facing +z, the world's `(cos, sin, 0)`).
    pub fn heading(&self) -> i32 {
        self.mover.heading
    }

    /// His body's rotation: `sim::transform` of the mover's render angles
    /// (`+0xb6, +0xb4, +0xb8`), applied to the model's (forward, down,
    /// left) axes as the mesh draw does (`0x4305a0` from mesh `+0x3c..`).
    fn body_rotation(&self) -> walker::M3 {
        let m = sim::transform([0; 3], self.mover.angles);
        [m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]]
    }

    /// His drawn forward (model `+x`) in world space.
    #[cfg(test)]
    pub fn fwd(&self) -> V3 {
        walker::mul(&self.body_rotation(), [1.0, 0.0, 0.0])
    }

    /// His drawn left (model `+z`) in world space.
    fn left_dir(&self) -> V3 {
        walker::mul(&self.body_rotation(), [0.0, 0.0, 1.0])
    }

    /// Re-derives `pos` and `up` from the mover.
    fn sync(&mut self, _sim: &Sim) {
        self.pos = self.mover.pos;
        self.up = normalize(self.mover.up);
    }

    // ——— the tick ———

    pub fn tick(&mut self, sim: &mut Sim, mesh: &mut WorldMesh) {
        self.reshape(sim);
        for r in std::mem::take(&mut sim.requests) {
            self.request(r, sim);
        }
        self.on_save_pad = false;
        // `Creature_Update` @ 0x417da0: modes 0 and 1 step the effect
        // (`+0x8ec` slot 1, 0x424960) until it says done.
        match self.mode {
            Mode::Absent => return,
            Mode::Appearing(n) => {
                self.mode = if self.effect(sim, |t, c| t.update(c)) { Mode::Appearing(n.saturating_add(1)) } else { Mode::Alive };
            }
            Mode::Vanishing(n, then) => {
                if self.effect(sim, |t, c| t.update(c)) {
                    self.mode = Mode::Vanishing(n.saturating_add(1), then);
                } else {
                    self.finish_vanishing(then, sim, mesh);
                    // Mode 1's first step runs the same tick (0x417e8e).
                    if !self.effect(sim, |t, c| t.update(c)) {
                        self.mode = Mode::Alive;
                    }
                }
            }
            Mode::Alive => self.live(sim),
        }
        // Dying and rematerialising, the mover still runs (0x417eb8): he
        // keeps riding his platform, settles on his new floor, or falls.
        if matches!(self.mode, Mode::Appearing(_) | Mode::Vanishing(..)) {
            let near = Near::gather(&self.polys, sim, self.mover.pos, GATHER_RADIUS, SOLID | FLOOR | HAZARD);
            self.step_mover(&near, sim);
        }
        self.sync(sim);
        self.attach_menc(sim, mesh);
        std::mem::swap(&mut self.prev_vertices, &mut self.vertices);
        self.build_vertices();
    }

    /// Follows the objects' vertex morphs: the casts read each object's
    /// current shape frame (`0x423130`), so his polygons of an object
    /// whose frame changed are rebuilt from it. The scrolling basis stays
    /// the one built at load (`0x41d9e0`).
    pub(crate) fn reshape(&mut self, sim: &mut Sim) {
        for i in sim.take_reshaped() {
            let (Some(verts), Some(polys)) = (sim.vertices_of(i), self.polys.get_mut(i)) else { continue };
            let Some(m) = sim.mesh_of(i) else { continue };
            for (lp, poly) in polys.iter_mut().zip(&m.polygons) {
                let texel = lp.texel;
                *lp = walker::local_poly(walker::polygon_points(poly, verts), crate::world::DEFAULT_TEXELS);
                lp.texel = texel;
            }
        }
    }

    /// Spawn, teleport and save requests queued by the portals this tick.
    fn request(&mut self, r: Request, sim: &mut Sim) {
        match r {
            Request::Spawn { at, heading, on, effect } => {
                // `Game_SpawnCreature` @ 0x41c590: the first call places
                // him; every call (section 0's arrival pad #83 makes one
                // while he is alive) overwrites the respawn point. A
                // non-zero effect kind materialises him (`0x4198f0`: the
                // effect backwards, mode 1); without one he is simply
                // there (mode 2, the creature's initial `+0x8fa`).
                let place = Place::of_request(at, heading, on);
                self.respawn = Some(place);
                if self.mode == Mode::Absent {
                    self.place(place, sim);
                    self.mode = if effect[0] != 0 {
                        let h = self.stance_height as i16;
                        self.effect(sim, |t, c| {
                            t.start_reversed(effect[0], effect[1], h, c);
                            true
                        });
                        Mode::Appearing(0)
                    } else {
                        Mode::Alive
                    };
                }
            }
            Request::Teleport { at, heading, snap, on, effect } => {
                // inferred: a request whose destination he is already close
                // to is ignored. Pads repeat their trigger every tick until
                // he touches them; without this he would never land.
                if self.mode == Mode::Alive && length(sub(at, self.pos)) > 1500.0 {
                    self.dematerialise(effect, sim);
                    self.mode = Mode::Vanishing(0, After::Teleport(Place::of_request(at, heading, on)));
                    self.stats.teleports += 1;
                    if snap {
                        sim.events.push(sim::HostEvent { kind: sim::EV_CAMERA_SNAP, a: 0, b: 0, c: 0 });
                    }
                }
            }
            Request::LevelEnd(ev) => {
                // Reactions 0x251000 / 0x2d1000: effect 1 v5, reason 4/5.
                if self.mode == Mode::Alive {
                    self.dematerialise([1, 5], sim);
                    self.mode = Mode::Vanishing(0, After::LevelEnd(ev));
                } else {
                    sim.events.push(ev);
                }
            }
            Request::Respawn => {
                // Portal kind 6's reaction 0x1b2000: effect 2 v3, reason 3.
                if self.mode == Mode::Alive {
                    self.dematerialise([2, 3], sim);
                    self.mode = Mode::Vanishing(0, After::Respawn);
                }
            }
        }
    }

    // ——— The transition effect (`+0x8ec`, transition.rs) ———

    /// Runs `f` on the effect with his current shape (re-posed: modes 0
    /// and 1 never pose him, so it is the last pose), heading and the CRT
    /// `rand()`, then applies what it did: a burst spins his heading (the
    /// mover's `+0x50`, the short `0x4105d0` was given), flashes, sounds.
    fn effect(&mut self, sim: &mut Sim, f: impl FnOnce(&mut Transition, &transition::Ctx) -> bool) -> bool {
        let mut t = std::mem::take(&mut self.transition);
        let running = {
            let shape = || {
                let posed = self.body.pose_turning((self.phase >> 8) as u8, self.stance, self.turn, self.side);
                transition::shape_of_posed(&posed, |r| self.materials.get(r as usize).copied().unwrap_or([0xffff, 0, 0, 0, 0, 0]))
            };
            let rand = || sim.rand();
            f(&mut t, &transition::Ctx { shape: &shape, spin: self.mover.heading, rand: &rand })
        };
        if let Some(h) = t.spin() {
            self.mover.heading = h & 0x3ff;
        }
        transition::emit(&mut t, sim, 0);
        self.transition = t;
        running
    }

    /// `0x424870(kind, variant, stance height)` + `0x4249b0`: mode 0 starts.
    fn dematerialise(&mut self, [kind, variant]: [u16; 2], sim: &mut Sim) {
        let h = self.stance_height as i16;
        self.effect(sim, |t, c| {
            t.start(kind, variant, h, c);
            true
        });
    }

    /// The mover's placement (slot 1, `0x418310` → `Mover_Place` @
    /// 0x411bf0): at `(u, v, h)` on the polygon, in its frame, then the
    /// floor test right away. A portal whose offset lifts him clear of the
    /// polygon (section 6's start, 4,000 up) leaves him airborne: he falls
    /// along the polygon's −normal.
    fn place(&mut self, p: Place, sim: &mut Sim) {
        self.mover.place(&self.polys, sim, p.on, p.at, p.heading, self.stance_height);
        self.delay = DELAY_TICKS;
        self.sync(sim);
    }

    /// Debug/test: put him at a world position, airborne, upright in the
    /// world's frame. The next tick's floor probe catches whatever lies
    /// within his reach below.
    #[cfg(test)]
    pub fn drop_at(&mut self, at: V3, heading: i32) {
        self.mover = Mover { pos: at, prev: at, heading: heading & 0x3ff, ..Mover::default() };
        self.fall_count = 0;
        self.fall_limit = 0;
        self.mover.angles = walker::upright_angles(heading);
        self.pos = at;
        self.up = UP;
    }

    /// `Game_OnCreatureRespawn` @ 0x41c700 after the effect has run.
    /// Reasons 1/2 place him on the pending teleport; reason 3 first
    /// restores the world snapshot (level start, last save or open), then
    /// puts him back on the respawn point and cuts the camera. The brain
    /// is never restored: he keeps what he learnt.
    fn finish_vanishing(&mut self, then: After, sim: &mut Sim, mesh: &mut WorldMesh) {
        match then {
            After::Teleport(p) => self.place(p, sim),
            // The host shows the time screen and builds the next section
            // (a new Mendel); until it does, this one comes back in place.
            After::LevelEnd(ev) => sim.events.push(ev),
            After::Respawn => {
                // Respawn reason 3 ends any affliction (`0x405f10` at 0x417e68).
                self.afflictions.end();
                if sim.restore_snapshot(mesh) {
                    sim.events.push(sim::HostEvent { kind: sim::EV_CAMERA_SNAP, a: 0, b: 0, c: 0 });
                }
                if let Some(p) = self.respawn {
                    self.place(p, sim);
                }
            }
        }
        // `0x4248c0`: the same effect backwards (it reads his new heading).
        self.effect(sim, |t, c| {
            t.reverse(c);
            true
        });
        self.mode = Mode::Appearing(0);
    }

    fn live(&mut self, sim: &mut Sim) {
        self.stats.ticks_alive += 1;
        let near = Near::gather(&self.polys, sim, self.mover.pos, GATHER_RADIUS, SOLID | FLOOR | HAZARD);
        self.step_mover(&near, sim);
        self.sync(sim);
        self.sense(&near, sim);
        self.think();
        self.apply_motor_outputs();
        self.accumulate_cost();
        // The effect manager's tick (0x405fc0), then the surface events.
        let floor_texture = self.ground().and_then(|(o, p)| sim.frame_texture(o, p));
        let last_texture = self.last_texture;
        self.afflictions.tick(Ctx { sim, floor_texture, last_texture });
        self.surface_events(sim);
        self.on_save_pad = self.surface & SAVE_PAD != 0;
        self.sound_cues(sim);
    }

    /// `CreatureMover_Step` @ 0x417f40, the touch inputs and the click.
    fn step_mover(&mut self, near: &Near, sim: &mut Sim) {
        let drive = Drive {
            forward: std::mem::take(&mut self.forward),
            turn: std::mem::take(&mut self.turn_d),
            stance: self.stance_height,
            gravity: self.afflictions.gravity(),
            injected: std::mem::take(&mut self.injected),
        };
        let mut posted = Vec::new();
        let was = self.mover.pos;
        let felt = self.mover.step(&self.polys, sim, near, &drive, &mut posted);
        if self.mover.grounded {
            self.stats.walked += length(sub(self.mover.pos, was));
        }
        // Event bit 1, "Mendel touches me", to each wall the hull pushed
        // against and to his polygon's object (`vtable[9]`, 0x4185e0 and
        // 0x418956 → `WorldObject_PostEvent` @ 0x4232f0).
        for o in posted {
            sim.post_event(o, 1);
            self.stats.touches_posted += 1;
        }
        self.left = if felt.left { 256 } else { 0 };
        self.right = if felt.right { 256 } else { 0 };
        self.surface = felt.surface;
        // The right click (`MEnc`): felt exactly like a bump on that side.
        if let Some((left, n)) = self.touch {
            if left { self.left = 256 } else { self.right = 256 }
            self.touch = (n > 1).then_some((left, n - 1));
        }
    }


    /// The cue part of `Creature_HandleSurfaceEvents` @ 0x419550. The
    /// landing cue has its own 5-tick silence; the four others share one
    /// timer and play by priority, one per tick, then stay quiet for their
    /// minimum plus `0..31` ticks drawn from the brain's LCG.
    fn sound_cues(&mut self, sim: &mut Sim) {
        let landed = std::mem::take(&mut self.landed);
        let agitated = matches!(self.afflictions.kind, 7..=9);
        let events = [
            agitated,
            self.seen_flags & HAZARD != 0,
            self.seen_class,
            (self.left - self.right).abs() > ASYMMETRY,
        ];
        self.seen_flags = 0;
        self.seen_class = false;
        if self.cue_quiet[0] > 0 {
            self.cue_quiet[0] -= 1;
        } else if landed {
            self.play_cue(sim, 0);
            self.cue_quiet[0] = SOUND_CUES[0].1;
        }
        match events.iter().position(|&e| e) {
            Some(k) if self.cue_quiet[1] == 0 => {
                self.play_cue(sim, k + 1);
                self.cue_quiet[1] = SOUND_CUES[k + 1].1 + (self.rng.next() & 0x1f) as u16;
            }
            _ => self.cue_quiet[1] = self.cue_quiet[1].saturating_sub(1),
        }
    }

    fn play_cue(&mut self, sim: &mut Sim, k: usize) {
        // The sound system's own randomness (the CRT rand) picks the sample.
        let sounds = SOUND_CUES[k].0;
        let pick = sounds[sim.rand() as usize % sounds.len()];
        sim.events.push(sim::HostEvent { kind: sim::EV_CREATURE_SOUND, a: pick as i32 - 1, b: 0, c: 0 });
    }


    // ——— senses ———

    /// `Creature_SenseInfrared` @ 0x418b00. Two eyes at heading ± `a`, each
    /// casting three rays; an eye already set by touch stays as it is.
    fn sense(&mut self, near: &Near, sim: &Sim) {
        let posture = self.act[0].value;
        let a = 100 - posture * 50 / 256;
        let range = 1500 + posture * 500 / 256;
        if self.left == 0 {
            self.left = self.eye(near, sim, a, range);
        }
        if self.right == 0 {
            self.right = self.eye(near, sim, -a, range);
        }
        match self.afflictions.kind {
            // Blind (0x418fe8).
            5 => {
                self.left = 0;
                self.right = 0;
            }
            // Confused (0x418ff9): each eye holds 0 or 256, flipping on
            // `seed & 0xf0000 == 0` draws of the brain's LCG.
            6 => {
                for k in 0..2 {
                    if self.lcg_low() {
                        self.noise[k] = 0;
                    } else if self.lcg_low() {
                        self.noise[k] = 256;
                    }
                }
                self.left = self.noise[0];
                self.right = self.noise[1];
            }
            _ => {}
        }
        // Falling (0x41906a): both eyes scream, unless the fall started
        // from a bit-4 floor (limit 130).
        if self.fall_count != 0 && self.fall_limit != 130 {
            self.left = 256;
            self.right = 256;
        }
    }

    /// One LCG step, true when bits 16..19 of the seed are all 0 (1 in 16).
    fn lcg_low(&mut self) -> bool {
        self.rng.next();
        self.rng.0 & 0xf_0000 == 0
    }

    /// One eye: three parallel rays of length `range` along heading +
    /// `angle` in his polygon's plane, from his feet (`pos − up · stance`)
    /// raised 256 and 16 along his up, and lowered 32 (under the floor's
    /// surface: the ledge ray). The eye reports the nearest of the three.
    fn eye(&mut self, near: &Near, sim: &Sim, angle: i32, range: i32) -> i32 {
        let f = self.mover.frame(&self.polys, sim);
        let a = turns(self.mover.heading + angle);
        let dir = normalize(f.dir([a.cos(), a.sin(), 0.0]));
        let feet = sub(self.pos, scale(self.up, self.stance_height as f32));
        let up = self.up;
        let ray = |lift: f32| {
            let o = add(feet, scale(up, lift));
            (o, add(o, scale(dir, range as f32)))
        };
        let value = |d: i32| (256 - d * 256 / (range - 450)).clamp(0, 256);
        let wanted = |t: &Tri| t.flags & (SOLID | HAZARD) != 0;
        // (distance − 656, value); 10000 = nothing seen.
        let mut rays = [(10_000, 0); 3];
        for (k, lift) in [EYE_HIGH, EYE_MID].into_iter().enumerate() {
            let (o, e) = ray(lift);
            // `Creature_CastIrRay` @ 0x4190f0: the nearest polygon of the
            // mask, which may hide what lies behind it.
            if let Some(h) = near.segment(o, e, Faces::Both, wanted) {
                if is_seen(h.flags) {
                    let d = h.dist as i32 - EYE_NEAR;
                    rays[k] = (d, value(d));
                }
                self.notice(h.flags);
            }
        }
        rays[2] = self.ledge_ray(near, ray(EYE_LOW), value);
        // The nearest wins (0x419070 compares the three distances).
        let [hi, mid, low] = rays;
        if !(mid.0 < hi.0 || low.0 < hi.0) {
            hi.1
        } else if !(hi.0 < mid.0 || low.0 < mid.0) {
            mid.1
        } else {
            low.1
        }
    }

    /// Sound-cue side effects of a polygon an eye ray met.
    fn notice(&mut self, flags: u32) {
        self.seen_flags |= flags;
        self.seen_class |= flags & 0x38_0000 == 0x18_0000;
    }

    /// `Creature_CastIrLedgeRay` @ 0x4191f0: the low ray runs 32 below his
    /// feet, i.e. *inside* the floor, so the first things it meets are the
    /// edges of the slab he walks on. All its hits (nearest first, at most
    /// ten) are read in order. IR class 0x40 (the sides of floors, walls
    /// `0x340`): a lone one is "an edge"; if the next hit lies within 250
    /// it was just a seam between two slabs, otherwise the edge is a drop
    /// and is seen. Class 0x80 or a hazard: seen at once. Anything else
    /// (class 0xc0 or 0) ends the ray unseen. Under the shield
    /// (affliction 1), bit-22 polygons count as class 0xc0 (0x419290).
    fn ledge_ray(&mut self, near: &Near, (o, e): (V3, V3), value: impl Fn(i32) -> i32) -> (i32, i32) {
        let wanted = |t: &Tri| t.flags & (SOLID | HAZARD) != 0;
        let hits = near.segment_all(o, e, wanted);
        let dist: Vec<i32> = hits.iter().map(|h| h.dist as i32).collect();
        let report = |d: i32| (d - EYE_NEAR, value(d - EYE_NEAR));
        let mut out = (10_000, 0);
        let mut pending = false;
        for (i, h) in hits.iter().enumerate() {
            let mut fl = h.flags;
            if self.afflictions.kind == 1 && fl & SHIELDED != 0 {
                fl |= IR_VISIBLE;
            }
            self.notice(fl);
            let stop = match fl & IR_VISIBLE {
                0x40 if pending => {
                    if dist[i] - dist[i - 1] < LEDGE_GAP {
                        pending = false;
                        false
                    } else {
                        out = report(dist[i - 1]);
                        true
                    }
                }
                0x40 if i + 1 == hits.len() => {
                    out = report(dist[i]);
                    false
                }
                0x40 => {
                    pending = true;
                    false
                }
                c if c == 0x80 || fl & HAZARD != 0 => {
                    out = report(if pending { dist[i - 1] } else { dist[i] });
                    true
                }
                _ => true,
            };
            if stop {
                break;
            }
        }
        out
    }


    // ——— brain ———

    /// `Agent_Think` @ 0x425dc0: the three controllers read
    /// `(right eye, left eye, 0)` (`Nerm_ConnectInput` @ 0x427da0) and each
    /// runs its Markov chain 200 steps; the outputs become actuator targets.
    fn think(&mut self) {
        let inputs = [self.right, self.left, 0];
        for k in 0..3 {
            let n = &mut self.brain.nerms[k];
            let p = n.transition_probs(inputs);
            n.probs_cache = p;
            self.outputs[k] = n.run_chain(&p, &mut self.rng);
            self.act[k].set_target(self.outputs[k]);
        }
        // (n38, n3c) = (1, 2) without afflictions (`Creature_Think` @
        // 0x418970). Turning only happens while walking: its actuator's
        // time step is the walking speed.
        let (n38, n3c) = self.afflictions.actuator_steps();
        for _ in 0..n38 {
            self.act[0].step(0, 256);
        }
        for _ in 0..n3c {
            self.act[1].step(0, 256);
            let walking = (2 * self.act[1].value - 256).abs();
            self.act[2].step(0, walking);
        }
    }

    /// `Creature_ApplyMotorOutputs` @ 0x4189c0 → `CreatureBody_ComputeMotion`
    /// @ 0x426350 and the phase advance of `CreatureBody_SetPose` @ 0x426410.
    fn apply_motor_outputs(&mut self) {
        let turn_out = self.act[2].value;
        self.turn = (2 * turn_out - 256).abs();
        self.side = turn_out >= 128;
        let speed = 2 * self.act[1].value - 256;
        self.stance = self.act[0].value;
        let (turn, speed, gait) = (self.turn as i64, speed as i64, self.afflictions.gait() as i64);
        // 24-bit fractional accumulators keep the remainders between ticks.
        self.fwd_acc += (256 - turn) * speed * gait * 420;
        let fwd = self.fwd_acc >> 24;
        self.fwd_acc -= fwd << 24;
        self.turn_acc += turn * speed * gait * 245;
        let td = self.turn_acc >> 24;
        self.turn_acc -= td << 24;
        self.forward = fwd as i32;
        self.turn_d = if self.side { td as i32 } else { -td as i32 };
        let h = stance_height(self.stance);
        // A rising stance pushes his body up (a hop), grounded and not
        // falling (0x418a8b).
        if self.mover.grounded && self.fall_count == 0 {
            self.mover.hvel = 2 * (h - self.stance_height);
        }
        self.stance_height = h;
        // frac = 256: we render simulation states, not interpolations.
        self.phase = self.phase.wrapping_add((speed * gait) as u16);
    }

    /// `Creature_AccumulateCost` @ 0x419470 and `Agent_LearnIfDue` @ 0x425e80.
    fn accumulate_cost(&mut self) {
        let (l, r) = (self.left as i64, self.right as i64);
        let turn = 2 * self.outputs[2] as i64 - 256;
        let speed = 2 * self.act[1].value as i64 - 256;
        let w = (l + r) * 416 / 512 + 96;
        self.count[0] += w;
        self.count[2] += w;
        self.count[1] += 256;
        self.cost[1] += (l + r) / 16 - speed + 256;
        let d = if l > r {
            (turn + l).abs() * 512 / (l + 256)
        } else if l < r {
            (turn - r).abs() * 512 / (r + 256)
        } else {
            (turn.abs() - r).abs() * 512 / 256
        };
        self.cost[2] += w * d / 512;
        self.cost[0] = self.cost[2];
        for k in 0..3 {
            if self.count[k] > 65536 {
                let c = (self.cost[k] * 65536 / self.count[k]).clamp(0, i32::MAX as i64) as i32;
                let n = &mut self.brain.nerms[k];
                if n.individual as usize + 1 == galapagos_formats::creature::POPULATION {
                    self.stats.evolutions += 1;
                }
                n.report_fitness(c, &mut self.rng);
                self.cost[k] = 0;
                self.count[k] = 0;
            }
        }
    }


    /// `Creature_HandleSurfaceEvents` @ 0x419550: the shield, the fall
    /// limit, lethal surfaces and afflictions (the cues are [`Self::sound_cues`]).
    fn surface_events(&mut self, sim: &mut Sim) {
        if self.afflictions.kind == 1 {
            if self.surface & SHIELDED != 0 {
                self.surface &= !LETHAL;
            }
            if self.surface & UNAFFLICTING != 0 {
                self.surface &= !AFFLICTION_BITS;
            }
        }
        // Falling for too long is lethal (0x4195cf..0x41962a); the limit
        // comes from the floor he left (`+0x134`, the mover's `+0xe4`).
        if !self.mover.grounded {
            if self.fall_limit == 0 {
                self.fall_limit = if self.mover.floor_flags & 0x10 != 0 { 130 } else { 65 };
            } else {
                let n = self.fall_count;
                self.fall_count += 1;
                if self.fall_limit < n {
                    self.fall_count = 0;
                    self.surface = (self.surface & 0xffd8_1fff) | FELL_TOO_LONG;
                } else if self.surface & 0x10 != 0 {
                    self.fall_count = 0;
                }
            }
        } else {
            // Landing after more than 5 airborne ticks: event 4.
            self.landed = self.fall_count > 5;
            self.fall_count = 0;
            self.fall_limit = 0;
        }
        let f = self.surface;
        if f & AFFLICTION_BITS != 0 {
            self.afflictions.apply(((f >> 25) & 15) as u8, sim);
        }
        if f & LETHAL == 0 {
            if self.mover.grounded {
                self.delay = DELAY_TICKS;
            }
            return;
        }
        if f & DELAYED != 0 {
            self.delay = self.delay.saturating_sub(1);
            if self.delay > 0 {
                return;
            }
        }
        if f & HURTS != 0 {
            for c in &mut self.cost {
                *c += PAIN;
            }
        }
        // (f >> 19) & 7 picks what happens after the effect; on the CD the
        // lethal floors all say 3, "death". Teleport reasons (1, 2) need a
        // pending destination we don't have here, so they also respawn.
        self.fall_count = 0;
        self.stats.deaths += 1;
        // The effect of the floor: kind (f >> 12) & 15, variant (f >> 16) & 7.
        self.dematerialise([((f >> 12) & 15) as u16, ((f >> 16) & 7) as u16], sim);
        self.mode = Mode::Vanishing(0, After::Respawn);
    }


    // ——— Predators ———

    /// Mendel as `Pred` & co. see him: his position (`creature+0xa34`,
    /// `0x4183a0`), his affliction (`0x4198e0`) and his click hull.
    pub(crate) fn prey(&self) -> Option<Prey> {
        (self.mode != Mode::Absent).then(|| Prey {
            pos: self.pos.map(|c| c as i32),
            affliction: self.afflictions.kind,
            menc: self.menc,
        })
    }

    /// `Game_CreatureReact` @ 0x41cb80 → 0x4198b0: OR surface flags into
    /// `creature+0xae8`; he feels them with his next touch.
    pub(crate) fn react(&mut self, flags: u32) {
        self.injected |= flags;
    }


    /// A predator's probe (`0x4160d0`): `World_Cast` @ 0x4089e0 on the
    /// segment `a → b` against polygons whose flags meet `mask`, ignoring
    /// object `skip`. The cast takes the nearest polygon whichever way it
    /// faces, and the touch counts either way; only the push is one-sided
    /// (see [`Creature::push_out`], `shift` = the class's `+0x344`).
    pub(crate) fn probe(&self, sim: &Sim, a: V3, b: V3, mask: u32, skip: usize, shift: u32) -> Option<ProbeHit> {
        let len = length(sub(b, a));
        let near = Near::gather(&self.polys, sim, a, len, mask);
        let h = near.segment(a, b, Faces::Both, |t| t.obj != skip)?;
        Some(ProbeHit { obj: h.obj, push: self.push_out(&h, b, shift) })
    }

    /// The bounce response shared by the predators' probes (`0x4160d0`)
    /// and the camera's wall probes (`0x413cb0`):
    ///
    /// * `0x408c50` casts a 0x400-long segment from the probe's tip along
    ///   the hit polygon's normal (rotated into the world, `0x409880`),
    ///   against that polygon alone (`0x408670`, `0x407740`). If it
    ///   crosses (the tip is behind the polygon, at most 1,024 deep) it
    ///   returns `crossing − tip`: the perpendicular way back onto the
    ///   plane, [`walker::behind`]. Otherwise there is no push at all.
    /// * The caller adds the polygon's normal as stored in its record
    ///   (`puVar3[0xc]`: object-local, ×1024, *not* rotated) shifted right
    ///   by `shift`: 5 or 7 for predators, 7 for the camera (8 units).
    pub(crate) fn push_out(&self, hit: &walker::Hit, tip: V3, shift: u32) -> Option<[i32; 3]> {
        let back = walker::behind(hit, tip)?;
        let n = self.polys.get(hit.obj)?.get(hit.poly)?.normal;
        Some([0, 1, 2].map(|i| back[i] as i32 + ((n[i] * 1024.0) as i32 >> shift)))
    }

    // ——— MEnc and rendering ———

    /// `Creature_AttachTouchHull` @ 0x417d60: the section's `MEnc` (an
    /// invisible click hull) follows Mendel: it gets his position and his
    /// three render angles (`+0xa34`, copied from the mover's `+0xa8`
    /// block at 0x418056; `0x422a80` puts them in the object's `+0xe4`
    /// block, i.e. `+0xf0 = +0xb4`, `+0xf2 = +0xb6`, `+0xf4 = +0xb8`).
    /// Ours is hidden: we pick against Mendel's own mesh instead (see
    /// [`Creature::pick`]).
    fn attach_menc(&self, sim: &mut Sim, mesh: &mut WorldMesh) {
        let Some(i) = self.menc else { return };
        let Some(draw) = mesh.draw_of_object[i] else { return };
        let r = mesh.draws[draw];
        let o = r.first as usize * FLOATS_PER_VERTEX;
        if mesh.vertices.get(o + 5).is_some_and(|&l| l != -2.0) {
            for v in 0..r.count as usize {
                mesh.vertices[o + v * FLOATS_PER_VERTEX + 5] = -2.0;
            }
            sim.mark_dirty(r.first, r.first + r.count);
        }
        sim.set_pose(i, self.pos.map(|c| c as i32), self.mover.angles, mesh);
    }


    /// His mesh a fraction `alpha` of the way from the previous tick's to
    /// this tick's, positions only (the rest is this tick's).
    ///
    /// not in the original (it drew once per tick): the browser draws at
    /// the display's rate, and the world's objects blend the same way
    /// ([`Sim::transforms_at`]), so riding a platform he stays on it.
    /// inferred: a mesh of another size (the transition effect changing
    /// what is drawn) or a vertex that moved more than 4096 units in one
    /// tick (a teleport, a respawn) is a cut, drawn as it is now.
    pub fn vertices_at(&self, alpha: f32, out: &mut Vec<f32>) {
        out.clear();
        out.extend_from_slice(&self.vertices);
        if !(0.0..1.0).contains(&alpha) || self.prev_vertices.len() != self.vertices.len() {
            return;
        }
        let far = |a: &[f32], b: &[f32]| (0..3).any(|k| (a[k] - b[k]).abs() > 4096.0);
        let (prev, cur) = (self.prev_vertices.chunks_exact(FLOATS_PER_VERTEX), self.vertices.chunks_exact(FLOATS_PER_VERTEX));
        if prev.clone().zip(cur.clone()).any(|(a, b)| far(a, b)) {
            return;
        }
        for ((o, a), b) in out.chunks_exact_mut(FLOATS_PER_VERTEX).zip(prev).zip(cur) {
            for k in 0..3 {
                o[k] = a[k] + (b[k] - a[k]) * alpha;
            }
        }
    }

    /// The posed body in world space: shell plus both leg sides, real UVs,
    /// texture layers 16..19, lit like the world's polygons
    /// ([`world::light_level`] of his world-space normals), so the shader
    /// gets them pre-lit: `light = -1 - level`, the fog amount in `nx`.
    fn build_vertices(&mut self) {
        self.vertices.clear();
        let visible = match self.mode {
            Mode::Absent => false,
            // The transition effect draws him (`dress` below).
            Mode::Appearing(_) | Mode::Vanishing(..) => true,
            Mode::Alive => true,
        };
        if !visible {
            return;
        }
        let mesh = self.body.pose_turning((self.phase >> 8) as u8, self.stance, self.turn, self.side);
        let (mesh, look) = self.transition.dress(mesh, &self.afflictions.look);
        let r = self.body_rotation();
        let world: Vec<V3> = mesh.vertices.iter().map(|v| add(self.pos, walker::mul(&r, v.map(|c| c as f32)))).collect();
        let look = &look;
        for (k, f) in mesh.faces.iter().enumerate() {
            if f.indices.len() < 3 {
                continue;
            }
            let p = |k: usize| world[f.indices[k] as usize];
            // (v1 − v0) × (v2 − v0) is the exe's (v2 − v1) × (v0 − v1).
            let n = normalize(cross(sub(p(1), p(0)), sub(p(2), p(0))));
            // Each polygon has its own copy of its section material, which
            // the affliction manager restores every tick (0x405fc0). An
            // affliction's brightness `v` rewrites the copy's first two
            // terms (`0x406110`): below 0x8000 `m0 = 2v/65535, m1 = 0`,
            // above it `m0 = 1, m1 = (v − 0x7fff)/2 / 65535` (whiter).
            let mut m = world::material(self.materials.get(f.color_ramp as usize).copied().unwrap_or([0xffff, 0, 0, 0, 0, 0]));
            let b = *look.bright.get(k).unwrap_or(&NORMAL);
            if b != NORMAL {
                (m[0], m[1]) = if b < 0x8000 {
                    ((b as u32 * 2) as f32 / 65535.0, 0.0)
                } else {
                    (1.0, ((b as u32 - 0x7fff) / 2) as f32 / 65535.0)
                };
            }
            let level = world::light_level(m, &self.light, n);
            let (light, fog) = (-1.0 - level as f32, m[5]);
            let texture = look.texture.map_or(f.texture as f32, |t| t as f32);
            // The polygon's texel offset (+0x5c/+0x5e), added by the
            // Direct3D draw as `(offset & 0x1ff) / 128` (0x40e850).
            // Affliction 1 adds 8 a tick to `u` (`0x42b200`); camouflage
            // (4) changes only the texture and keeps his own offsets
            // (`0x407350` copies `+0x40` and the material, nothing else).
            let offset = [f.offset[0].wrapping_add(look.scroll), f.offset[1]];
            let [du, dv] = offset.map(|o| (o & 0x1ff) as f32);
            for k in 1..f.indices.len() - 1 {
                for j in [0, k, k + 1] {
                    let v = p(j);
                    let [u, t] = f.uv[j];
                    self.vertices.extend_from_slice(&[v[0], v[1], v[2], (u + du) / 128.0, (t + dv) / 128.0, texture, light, fog, 0.0, 0.0]);
                }
            }
        }
    }


    /// For `camera.rs` (`Camera_Collide` @ 0x413cb0): the nearest polygon
    /// with flags 0x102 crossing the segment `from → to`, either way, and
    /// the push it gives (shift 7, see [`Creature::push_out`]).
    pub fn camera_probe(&self, sim: &Sim, from: V3, to: V3) -> Option<[i32; 3]> {
        let near = Near::gather(&self.polys, sim, from, length(sub(to, from)), 0x102);
        let h = near.segment(from, to, Faces::Both, |_| true)?;
        self.push_out(&h, to, 7)
    }


    /// Ray against his posed mesh. Returns `Some(true)` for a hit on his
    /// left side, `Some(false)` for the right (the original maps the eight
    /// `MEnc` polygons to sides 1..4; we use the hit point instead).
    pub fn pick(&self, o: V3, d: V3) -> Option<bool> {
        let d = normalize(d);
        let mut best: Option<f32> = None;
        for tri in self.vertices.chunks_exact(3 * FLOATS_PER_VERTEX) {
            let v = |k: usize| [tri[k * FLOATS_PER_VERTEX], tri[k * FLOATS_PER_VERTEX + 1], tri[k * FLOATS_PER_VERTEX + 2]];
            let (a, b, c) = (v(0), v(1), v(2));
            let t = Tri { a, e1: sub(b, a), e2: sub(c, a), normal: [0.0; 3], obj: 0, poly: 0, flags: 0 };
            if let Some(dist) = ray_tri(o, d, &t) {
                if best.is_none_or(|b| dist < b) {
                    best = Some(dist);
                }
            }
        }
        let hit = add(o, scale(d, best?));
        Some(dot(sub(hit, self.pos), self.left_dir()) >= 0.0)
    }
}

fn stance_height(stance: i32) -> i32 {
    41 + stance * 166 / 256
}

// ——— Saved games (docs/re/saves.md) ——————————————————————————————————————

/// The mover as a saved game holds it. Version 3 stores the walker's own
/// state (`Mover_Write` @ 0x412620 keeps the same facts: surface object,
/// polygon, heading, local position, `+0x52`, `+0x24`, `+0xe4`); versions
/// 1 and 2 stored the old ray-cast controller's world frame and its
/// object-local contact, converted on load.
#[derive(Clone, Debug)]
enum SavedMover {
    Walker(Mover),
    Legacy { pos: V3, fwd: V3, up: V3, contact: Option<(usize, usize)>, air: V3 },
}

/// Everything about Mendel a saved game needs besides his brain: the
/// mover, the agent (actuators, running costs), the respawn point and the
/// counters. Parsed completely before it is applied, so a damaged file
/// leaves the running game untouched.
#[derive(Clone, Debug)]
pub struct SavedCreature {
    mode: Mode,
    mover: SavedMover,
    ints: [i32; 3], // forward, turn_d, stance_height
    surface: u32,
    delay: u16,
    left: i32,
    right: i32,
    touch: Option<(bool, u8)>,
    outputs: [i32; 3],
    act: [[i32; 7]; 3],
    cost: [i64; 3],
    count: [i64; 3],
    fwd_acc: i64,
    turn_acc: i64,
    phase: u16,
    stance: i32,
    turn: i32,
    side: bool,
    respawn: Option<Place>,
    stats: Stats,
    rng: u32,
    cue_quiet: [u16; 5],
    /// `injected`, the fall counter and limit, affliction 6's held noise
    /// (version 2 on; version 2 also had the last ground flags, now the
    /// mover's `floor_flags`).
    hazards: (u32, u16, u16, [i32; 2]),
}

impl Creature {
    pub(crate) fn saved(&self) -> SavedCreature {
        SavedCreature {
            mode: self.mode,
            mover: SavedMover::Walker(self.mover),
            ints: [self.forward, self.turn_d, self.stance_height],
            surface: self.surface,
            delay: self.delay,
            left: self.left,
            right: self.right,
            touch: self.touch,
            outputs: self.outputs,
            act: self.act.clone().map(|a| [a.rate, a.shift as i32, a.pos, a.vel, a.last_dt, a.target, a.value]),
            cost: self.cost,
            count: self.count,
            fwd_acc: self.fwd_acc,
            turn_acc: self.turn_acc,
            phase: self.phase,
            stance: self.stance,
            turn: self.turn,
            side: self.side,
            respawn: self.respawn,
            stats: self.stats,
            rng: self.rng.0,
            cue_quiet: self.cue_quiet,
            hazards: (self.injected, self.fall_count, self.fall_limit, self.noise),
        }
    }

    pub(crate) fn apply_saved(&mut self, s: SavedCreature, sim: &Sim) {
        let valid = |on: Option<(usize, usize)>| {
            on.filter(|&(o, p)| self.polys.get(o).and_then(|v| v.get(p)).is_some_and(|lp| lp.points.len() >= 3))
        };
        self.mode = s.mode;
        if let Mode::Vanishing(n, After::Teleport(p)) = self.mode {
            self.mode = Mode::Vanishing(n, After::Teleport(Place { on: valid(p.on), ..p }));
        }
        self.mover = match s.mover {
            SavedMover::Walker(m) => Mover { on: valid(m.on), grounded: m.grounded && valid(m.on).is_some(), ..m },
            SavedMover::Legacy { pos, fwd, up, contact, air } => self.legacy_mover(sim, pos, fwd, up, contact, air),
        };
        [self.forward, self.turn_d, self.stance_height] = s.ints;
        self.surface = s.surface;
        self.delay = s.delay;
        self.left = s.left;
        self.right = s.right;
        self.touch = s.touch;
        self.outputs = s.outputs;
        self.act = s.act.map(|[rate, shift, pos, vel, last_dt, target, value]| Actuator {
            rate,
            shift: shift.clamp(0, 31) as u32,
            pos,
            vel,
            last_dt: if last_dt == 0 { 256 } else { last_dt },
            target,
            value,
        });
        self.cost = s.cost;
        self.count = s.count;
        self.fwd_acc = s.fwd_acc;
        self.turn_acc = s.turn_acc;
        self.phase = s.phase;
        self.stance = s.stance;
        self.turn = s.turn;
        self.side = s.side;
        self.respawn = s.respawn.map(|p| Place { on: valid(p.on), ..p });
        self.stats = s.stats;
        self.rng = Lcg(s.rng);
        self.cue_quiet = s.cue_quiet;
        (self.injected, self.fall_count, self.fall_limit, self.noise) = s.hazards;
        // `Creature_Read` @ 0x419950 ends any affliction (0x405f10 at
        // 0x419967): afflictions are not part of a saved game.
        self.afflictions.end();
        // inferred: our saves do not keep the render angles; slot 6
        // recomputes them from where he is (in flight that drops the roll
        // his spin had added since he let go).
        self.mover.angles = walker::upright_angles(self.mover.heading);
        self.mover.refresh_angles(&self.polys, sim);
        self.sync(sim);
        self.build_vertices();
        self.prev_vertices.clear();
    }

    /// A version 1–2 mover (world frame + the contact in its object's local
    /// space) re-expressed in the walker's polygon coordinates.
    fn legacy_mover(&self, sim: &Sim, pos: V3, fwd: V3, up: V3, contact: Option<(usize, usize)>, air: V3) -> Mover {
        let on = contact.filter(|&(o, p)| self.polys.get(o).and_then(|v| v.get(p)).is_some_and(|lp| lp.points.len() >= 3));
        let f = Frame::of(&self.polys, sim, on);
        let hd = f.undir(fwd);
        let heading = ((hd[1].atan2(hd[0]) * 1024.0 / core::f32::consts::TAU).round() as i32) & 0x3ff;
        let local = f.unpoint(pos);
        Mover {
            on,
            grounded: on.is_some(),
            u: local[0],
            v: local[1],
            h: (local[2].round() as i32).max(0),
            heading,
            up: if on.is_some() { f.normal() } else { normalize(up) },
            pos,
            prev: pos,
            air,
            obj_rot: f.rotated.then_some(f.rot),
            floor_flags: on.map_or(0, |(o, p)| sim.frame_flags(o, p)),
            ..Mover::default()
        }
    }

    /// Part of `Game_TakeSnapshot` @ 0x41d170: his current spot becomes the
    /// respawn point, read off the mover (`Mover_GetSurface` @ 0x4124a0,
    /// `Mover_GetLocal` @ 0x4124f0): object, polygon, `(u, v)` with `h =
    /// 0`, heading. Airborne, that is the last floor and the last `(u, v)`.
    pub(crate) fn anchor_respawn(&mut self) {
        if self.mode != Mode::Alive {
            return;
        }
        let m = &self.mover;
        self.respawn = Some(match m.on {
            Some(on) => Place { on: Some(on), at: [m.u, m.v, 0.0], heading: m.heading },
            None => Place { on: None, at: m.pos, heading: m.heading },
        });
    }
}

fn write_v3(w: &mut Writer, v: V3) {
    v.iter().for_each(|&c| w.f32(c));
}
fn read_v3(r: &mut Reader) -> Result<V3, &'static str> {
    Ok([r.f32()?, r.f32()?, r.f32()?])
}

fn write_place(w: &mut Writer, p: Option<Place>) {
    let p = p.unwrap_or(Place { on: None, at: [0.0; 3], heading: 0 });
    let (has, (o, q)) = (p.on.is_some() as u8, p.on.unwrap_or((0, 0)));
    w.u8(has);
    w.u32(o as u32);
    w.u32(q as u32);
    write_v3(w, p.at);
    w.i32(p.heading);
}

fn read_place(r: &mut Reader) -> Result<Place, &'static str> {
    let has = r.u8()? != 0;
    let (o, q) = (r.u32()? as usize, r.u32()? as usize);
    Ok(Place { on: has.then_some((o, q)), at: read_v3(r)?, heading: r.i32()? })
}

impl SavedCreature {
    /// Version 3 layout (docs/re/saves.md).
    pub(crate) fn write(&self, w: &mut Writer) {
        let (kind, n, after) = match self.mode {
            Mode::Absent => (0, 0, None),
            Mode::Appearing(n) => (1, n, None),
            Mode::Alive => (2, 0, None),
            Mode::Vanishing(n, a) => (3, n, Some(a)),
        };
        w.u8(kind);
        w.u16(n);
        let teleport = match after {
            Some(After::Teleport(p)) => Some(p),
            _ => None,
        };
        w.u8(teleport.is_some() as u8);
        write_place(w, teleport);
        let m = match &self.mover {
            SavedMover::Walker(m) => *m,
            SavedMover::Legacy { .. } => Mover::default(), // never written: apply converts it
        };
        let (has, (o, q)) = (m.on.is_some() as u8, m.on.unwrap_or((0, 0)));
        w.u8(has);
        w.u32(o as u32);
        w.u32(q as u32);
        w.u8(m.grounded as u8);
        w.f32(m.u);
        w.f32(m.v);
        w.i32(m.h);
        w.i32(m.hvel);
        w.i32(m.heading);
        for v in [m.up, m.pos, m.prev, m.air] {
            write_v3(w, v);
        }
        w.i32(m.spin);
        w.u8(m.obj_rot.is_some() as u8);
        m.obj_rot.unwrap_or(IDENTITY).iter().for_each(|&c| w.f32(c));
        w.u32(m.floor_flags);
        self.ints.iter().for_each(|&i| w.i32(i));
        w.u32(self.surface);
        w.u16(self.delay);
        w.i32(self.left);
        w.i32(self.right);
        let (has, left, n) = self.touch.map_or((0, 0, 0), |(l, n)| (1, l as u8, n));
        w.u8(has);
        w.u8(left);
        w.u8(n);
        self.outputs.iter().for_each(|&i| w.i32(i));
        self.act.iter().flatten().for_each(|&i| w.i32(i));
        self.cost.iter().chain(&self.count).for_each(|&i| w.i64(i));
        w.i64(self.fwd_acc);
        w.i64(self.turn_acc);
        w.u16(self.phase);
        w.i32(self.stance);
        w.i32(self.turn);
        w.u8(self.side as u8);
        w.u8(self.respawn.is_some() as u8);
        write_place(w, self.respawn);
        let s = self.stats;
        w.u32(s.ticks_alive);
        w.f32(s.walked);
        [s.touches_posted, s.deaths, s.teleports, s.evolutions].iter().for_each(|&i| w.u32(i));
        w.u32(self.rng);
        self.cue_quiet.iter().for_each(|&q| w.u16(q));
        let (injected, count, limit, noise) = self.hazards;
        w.u32(injected);
        w.u16(count);
        w.u16(limit);
        noise.iter().for_each(|&n| w.i32(n));
    }

    /// `version` is the saved game's: 3 is the walker's layout; 1 and 2
    /// the ray-cast controller's (2 adds the hazard block).
    pub(crate) fn read(r: &mut Reader, version: u32) -> Result<Self, &'static str> {
        if version < 3 {
            return Self::read_legacy(r, version);
        }
        let kind = r.u8()?;
        let n = r.u16()?;
        let teleport = r.u8()? != 0;
        let place = read_place(r)?;
        let after = if teleport { After::Teleport(place) } else { After::Respawn };
        let mode = mode_of(kind, n, after)?;
        let has = r.u8()? != 0;
        let (o, q) = (r.u32()? as usize, r.u32()? as usize);
        let grounded = r.u8()? != 0;
        let (u, v, h, hvel, heading) = (r.f32()?, r.f32()?, r.i32()?, r.i32()?, r.i32()?);
        let (up, pos, prev, air) = (read_v3(r)?, read_v3(r)?, read_v3(r)?, read_v3(r)?);
        let spin = r.i32()?;
        let rotated = r.u8()? != 0;
        let mut rot = IDENTITY;
        for c in &mut rot {
            *c = r.f32()?;
        }
        let floor_flags = r.u32()?;
        let mover = Mover {
            on: has.then_some((o, q)),
            grounded,
            u,
            v,
            h,
            hvel,
            heading,
            up,
            pos,
            prev,
            air,
            spin,
            obj_rot: rotated.then_some(rot),
            floor_flags,
            angles: walker::upright_angles(heading),
        };
        let ints = [r.i32()?, r.i32()?, r.i32()?];
        let mut s = Self::read_agent(r, mode, SavedMover::Walker(mover), ints)?;
        let has_respawn = r.u8()? != 0;
        let respawn = read_place(r)?;
        s.respawn = has_respawn.then_some(respawn);
        s.read_tail(r)?;
        let (injected, count, limit) = (r.u32()?, r.u16()?, r.u16()?);
        s.hazards = (injected, count, limit, [r.i32()?, r.i32()?]);
        Ok(s)
    }

    fn read_legacy(r: &mut Reader, version: u32) -> Result<Self, &'static str> {
        let kind = r.u8()?;
        let n = r.u16()?;
        let teleport = r.u8()? != 0;
        let (at, h) = (read_v3(r)?, r.i32()?);
        let after = if teleport { After::Teleport(Place { on: None, at, heading: h }) } else { After::Respawn };
        let mode = mode_of(kind, n, after)?;
        let (pos, fwd, up) = (read_v3(r)?, read_v3(r)?, read_v3(r)?);
        let mut old = [0; 5]; // forward, turn_d, height, hvel, stance_height
        for i in &mut old {
            *i = r.i32()?;
        }
        let air = read_v3(r)?;
        let _air_ticks = r.u32()?;
        let grounded = r.u8()? != 0;
        let (o, q) = (r.u32()? as usize, r.u32()? as usize);
        // The old contact's platform-local position, forward and up: the
        // world ones are equivalent at save time.
        for _ in 0..3 {
            read_v3(r)?;
        }
        let contact = grounded.then_some((o, q));
        let mover = SavedMover::Legacy { pos, fwd, up, contact, air };
        let mut s = Self::read_agent(r, mode, mover, [old[0], old[1], old[4]])?;
        // Spawn, respawn (the save point), safe point: world positions.
        let mut points = [None; 3];
        for p in &mut points {
            let has = r.u8()? != 0;
            let (at, h) = (read_v3(r)?, r.i32()?);
            *p = has.then_some(Place { on: None, at, heading: h });
        }
        s.respawn = points[1].or(points[0]);
        s.read_tail(r)?;
        if version >= 2 {
            let (injected, count, limit, _ground) = (r.u32()?, r.u16()?, r.u16()?, r.u32()?);
            s.hazards = (injected, count, limit, [r.i32()?, r.i32()?]);
        }
        Ok(s)
    }

    /// The agent block, common to every version.
    fn read_agent(r: &mut Reader, mode: Mode, mover: SavedMover, ints: [i32; 3]) -> Result<Self, &'static str> {
        let surface = r.u32()?;
        let delay = r.u16()?;
        let (left, right) = (r.i32()?, r.i32()?);
        let (has, l, tn) = (r.u8()?, r.u8()?, r.u8()?);
        let mut outputs = [0; 3];
        for i in &mut outputs {
            *i = r.i32()?;
        }
        let mut act = [[0; 7]; 3];
        for i in act.iter_mut().flatten() {
            *i = r.i32()?;
        }
        let (mut cost, mut count) = ([0; 3], [0; 3]);
        for i in cost.iter_mut().chain(&mut count) {
            *i = r.i64()?;
        }
        let (fwd_acc, turn_acc) = (r.i64()?, r.i64()?);
        let phase = r.u16()?;
        let (stance, turn) = (r.i32()?, r.i32()?);
        let side = r.u8()? != 0;
        Ok(SavedCreature {
            mode,
            mover,
            ints,
            surface,
            delay,
            left,
            right,
            touch: (has != 0).then_some((l != 0, tn)),
            outputs,
            act,
            cost,
            count,
            fwd_acc,
            turn_acc,
            phase,
            stance,
            turn,
            side,
            respawn: None,
            stats: Stats::default(),
            rng: 0,
            cue_quiet: [0; 5],
            hazards: (0, 0, 0, [0; 2]),
        })
    }

    /// Stats, brain RNG and cue timers, common to every version.
    fn read_tail(&mut self, r: &mut Reader) -> Result<(), &'static str> {
        let ticks_alive = r.u32()?;
        let walked = r.f32()?;
        let (touches_posted, deaths, teleports, evolutions) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        self.stats = Stats { ticks_alive, walked, touches_posted, deaths, teleports, evolutions };
        self.rng = r.u32()?;
        for q in &mut self.cue_quiet {
            *q = r.u16()?;
        }
        Ok(())
    }
}

fn mode_of(kind: u8, n: u16, after: After) -> Result<Mode, &'static str> {
    Ok(match kind {
        0 => Mode::Absent,
        1 => Mode::Appearing(n.max(1)),
        2 => Mode::Alive,
        3 => Mode::Vanishing(n.max(1), after),
        _ => return Err("bad creature mode"),
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use galapagos_formats::creature::overlay_for_age;
    use galapagos_formats::worlds::{Archive, Object};

    pub(super) fn data() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds")).ok()
    }

    /// Section `s` with Mendel loaded (not spawned) and the level-3 brain.
    pub(super) fn world(d: &[u8], s: usize) -> (Sim, WorldMesh) {
        let a = Archive::parse(d).unwrap();
        let section = a.world(s).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mut mesh);
        let body = CreatureBody::parse(d, a.header.creature as usize).unwrap();
        let brain = Brain::parse(d, a.header.creature_overlays[overlay_for_age(128).unwrap()] as usize).unwrap();
        sim.creature = Some(Creature::new(&section, &mesh, body, brain));
        (sim, mesh)
    }

    /// Runs `secs` seconds; `skip_at` holds the Space key for 2 ticks then.
    fn run(sim: &mut Sim, mesh: &mut WorldMesh, secs: u32, skip_at: Option<u64>) -> Vec<bool> {
        let mut ever = vec![false; sim.visible().len()];
        for _ in 0..secs * sim::TICKS_PER_SECOND {
            sim.skip_held = skip_at.is_some_and(|t| (t..t + 2).contains(&sim.tick));
            sim.step(mesh);
            // Folders count from 2 s on, once the start-up flurry settled.
            if sim.tick > 2 * sim::TICKS_PER_SECOND as u64 {
                for (e, v) in ever.iter_mut().zip(sim.visible()) {
                    *e |= v;
                }
            }
        }
        ever
    }

    /// The real introduction, start to finish. The `Strt` portal #57
    /// spawns Mendel after ~14 s *on* its disc, the floor of the birth
    /// tube #54 (a polygon facing +z), standing upright. The disc hands
    /// its floor down its stack of four polygons, then the docked capsule
    /// #3 takes him, rolls him through the sky, and drops him on the
    /// The bounce response (`0x408c50` + stored normal >> shift): a probe
    /// tip 40 units under the start disc's top (#57, facing +z) comes back
    /// 40 units, plus 1024 >> 7 = 8 along the stored normal; a tip above
    /// the disc (crossing it from behind) gets no push at all.
    #[test]
    fn bounce_response_follows_0x408c50() {
        let Some(d) = data() else { return };
        let (sim, _mesh) = world(&d, 0);
        let c = sim.creature.as_ref().unwrap();
        let near = Near::gather(&c.polys, &sim, [0.0; 3], f32::MAX, FLOOR);
        let t = near.0.iter().find(|t| t.obj == 56 && t.normal[2] > 0.99).expect("a floor of #57 facing +z");
        let c0 = add(t.a, scale(add(t.e1, t.e2), 0.3));
        let hit = walker::Hit { dist: 0.0, point: c0, obj: t.obj, poly: t.poly, flags: t.flags, normal: t.normal, plane: t.a };
        assert_eq!(c.push_out(&hit, sub(c0, [0.0, 0.0, 40.0]), 7), Some([0, 0, 48]));
        assert_eq!(c.push_out(&hit, add(c0, [0.0, 0.0, 40.0]), 7), None);
    }

    /// arrival pad #83 after ~133 s. He never falls out of the world.
    #[test]
    fn mendel_is_born_in_section0() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        let (mut on_disc, mut on_capsule, mut on_pad) = (None, None, None);
        for t in 0..150 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            if c.mode != Mode::Alive {
                continue;
            }
            match c.ground().map(|g| g.0 + 1) {
                Some(57) if on_disc.is_none() => on_disc = Some((t, c.up, c.pos, c.stance_height)),
                Some(3) if on_capsule.is_none() => on_capsule = Some(t),
                Some(83) if on_pad.is_none() => on_pad = Some((t, c.up)),
                _ => {}
            }
        }
        let c = sim.creature.as_ref().unwrap();
        println!("intro: disc {on_disc:?}, capsule at {on_capsule:?}, pad {on_pad:?}; then {:?} at {:?}, {:?}", c.mode, c.pos.map(|v| v as i32), c.stats);
        let (_, up, pos, h) = on_disc.expect("born standing on the Strt disc");
        assert!(up[2] > 0.99, "upright on a floor facing +z: up {up:?}");
        assert!((pos[2] - (-284148.0 + h as f32)).abs() < 30.0, "his origin floats his stance height above the disc, z = {}", pos[2]);
        assert!(on_capsule.is_some(), "the capsule picks him up");
        let (t, up) = on_pad.expect("the capsule delivers him to the arrival pad");
        assert!(up[2] > 0.99, "upright on the pad: up {up:?}");
        assert!((120..145).contains(&(t / sim::TICKS_PER_SECOND)), "after a ride of about 133 s, not {t} ticks");
        assert_eq!(c.stats.deaths, 0, "never fell out of the world");
        assert!(c.stats.walked > 2000.0, "he walks");
        assert!(c.stats.touches_posted > 0);
    }

    /// The Space key (`STel`) skips the intro: Mendel is spawned over the
    /// arrival pad (#83/#92) of the main area. Then we drop him into the
    /// lift shaft (folder #75): the upper level (folder #145, with the gate
    /// #160) sleeps until #75 feels him, so his event 1 must wake it.
    #[test]
    fn mendel_wakes_the_world_in_section0() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        run(&mut sim, &mut mesh, 10, Some(5));
        let c = sim.creature.as_ref().unwrap();
        println!("skip: {:?} at {:?} on {:?} up {:?}", c.mode, c.pos.map(|v| v as i32), c.ground(), c.up);
        assert!(c.stats.ticks_alive > 8 * sim::TICKS_PER_SECOND, "spawned by the arrival pad");
        assert!(matches!(c.ground(), Some((o, _)) if o + 1 == 92 || o + 1 == 83), "standing on the pad");
        // On *top* of it, not clinging to a side: its floors are the top
        // (polygon 9, z = 5180) and two gentle ramps (3 and 6, normal
        // z = 0.98) that run down to z = 4440; its sides are walls (0x340).
        let (_, poly) = c.ground().unwrap();
        assert!([3, 6, 9].contains(&poly) && c.up[2] > 0.95 && c.pos[2] > 4440.0, "upright on top of the pad, poly {poly}, z = {}", c.pos[2]);
        assert!(!sim.visible()[160 - 1], "the upper level sleeps");

        sim.creature.as_mut().unwrap().drop_at([-1110.0, -19100.0, 3959.0], 0); // into the lift platform #77
        let mut woke = None;
        for t in 0..3 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if woke.is_none() && sim.visible()[160 - 1] {
                woke = Some(t);
            }
        }
        println!("upper level woke after {woke:?} ticks");
        assert!(woke.is_some(), "his touch event woke folder #145");

        // A minute of life afterwards: he walks, and stays in the level.
        run(&mut sim, &mut mesh, 60, None);
        let c = sim.creature.as_ref().unwrap();
        println!("then: {:?} at {:?}, {:?}", c.mode, c.pos.map(|v| v as i32), c.stats);
        assert!(c.stats.walked > 2000.0, "walked {}", c.stats.walked);
        assert!(c.pos[2].abs() < 40_000.0, "inside the level, z = {}", c.pos[2]);
    }

    /// Culling never hides the floor under him: in section 0 (after the
    /// skip, a minute of walking) and at section 2's upside-down start,
    /// the triangles the renderer draws for the polygon he stands on turn
    /// counter-clockwise seen from his side, i.e. he stands on their front
    /// (the side `0x402490` and Direct3D keep, world.rs `culls_glass`).
    #[test]
    fn mendel_stands_on_the_drawn_side_of_his_floors() {
        let Some(d) = data() else { return };
        let front = |sim: &Sim, mesh: &WorldMesh| -> Option<bool> {
            let c = sim.creature.as_ref().unwrap();
            let (o, p) = c.ground()?;
            let r = mesh.poly_ranges[mesh.draw_of_object[o]?][p];
            let m = sim::transform(sim.pose(o).0, sim.pose(o).1);
            let v = |i: u32| {
                let k = i as usize * FLOATS_PER_VERTEX;
                sim::apply(&m, [mesh.vertices[k], mesh.vertices[k + 1], mesh.vertices[k + 2]])
            };
            let (a, b, t) = (v(r.first), v(r.first + 1), v(r.first + 2));
            Some(dot(cross(sub(b, a), sub(t, a)), sub(c.pos, a)) > 0.0)
        };
        let (mut sim, mut mesh) = world(&d, 0);
        run(&mut sim, &mut mesh, 10, Some(5));
        let (mut grounded, mut on_front) = (0, 0);
        for _ in 0..60 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if let Some(f) = front(&sim, &mesh) {
                grounded += 1;
                on_front += f as u32;
            }
        }
        assert!(grounded > 50 * sim::TICKS_PER_SECOND);
        assert_eq!(on_front, grounded, "always on the drawn side");

        let (mut sim, mut mesh) = world(&d, 2);
        while sim.creature.as_ref().unwrap().mode == Mode::Absent {
            sim.step(&mut mesh);
        }
        assert!(sim.creature.as_ref().unwrap().up[2] < -0.999, "upside down");
        assert_eq!(front(&sim, &mesh), Some(true), "under a ceiling that faces him");
    }

    /// After the skip, a minute of walking around the main area: whatever
    /// he stands on is a floor polygon (bit 1) facing up, +z.
    #[test]
    fn mendel_walks_on_floors_facing_up_in_section0() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        run(&mut sim, &mut mesh, 10, Some(5));
        let (mut grounded, mut upright, mut floors) = (0, 0, 0);
        for _ in 0..60 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            let Some((o, p)) = c.ground() else { continue };
            grounded += 1;
            upright += (c.up[2] > 0.9) as u32;
            floors += (sim.frame_flags(o, p) & FLOOR != 0) as u32;
        }
        let c = sim.creature.as_ref().unwrap();
        println!("{grounded} grounded ticks, {upright} upright, {floors} on floor polygons; walked {}", c.stats.walked as i32);
        assert!(grounded > 50 * sim::TICKS_PER_SECOND, "mostly on the ground");
        assert_eq!(floors, grounded, "always on a floor polygon");
        assert!(upright * 10 >= grounded * 9, "upright (up ≈ +z) at least 90% of the time");
        assert_eq!(c.stats.deaths, 0);
    }

    #[test]
    fn actuator_reaches_its_target_smoothly() {
        let mut a = Actuator::new(20, 128);
        a.set_target(256);
        let mut last = a.value;
        for _ in 0..200 {
            a.step(0, 256);
            assert!(a.value >= last && a.value - last <= 20, "monotone and slew-limited");
            last = a.value;
        }
        assert!((250..=256).contains(&a.value), "settles near the target, got {}", a.value);
        // dt = 0 freezes it (turning while standing still).
        a.set_target(0);
        a.step(0, 0);
        assert_eq!(a.value, last);
    }

    /// One minute of section 0 after the skip: the world and Mendel make
    /// sounds, and every sound event names a real, non-empty sample.
    #[test]
    fn section0_sounds_point_at_real_samples() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        run(&mut sim, &mut mesh, 60, Some(5));
        let a = Archive::parse(&d).unwrap();
        let world_bank = crate::sound::Bank::decode(&d, &a.world(0).unwrap().sounds, crate::sound::BEST_QUALITY);
        let creature_set = galapagos_formats::worlds::SoundSet::parse(&d, a.header.creature_sounds as usize).unwrap();
        let creature_bank = crate::sound::Bank::decode(&d, &creature_set, crate::sound::BEST_QUALITY);
        let (mut world, mut cues) = (0, 0);
        for e in &sim.events {
            let bank = match e.kind {
                sim::EV_SOUND => { world += 1; &world_bank }
                sim::EV_CREATURE_SOUND => { cues += 1; &creature_bank }
                _ => continue,
            };
            let s = bank.sounds.get(e.a as usize).and_then(|s| s.as_ref());
            assert!(s.is_some_and(|s| !s.samples.is_empty()), "event {e:?} names an empty slot");
        }
        println!("section 0, 60 s: {world} world sounds, {cues} Mendel cues");
        assert!(world > 0 && cues > 0);
    }

    /// Save pads: every polygon whose frame has flag 0x20, in section 3
    /// (12 pads). Dropped onto one, Mendel reports `on_save_pad`, and a ray
    /// from above picks a 0x20 polygon: the two conditions of the Save
    /// dialog (click handler 0x41baa3..0x41bb07).
    #[test]
    fn standing_on_a_save_pad_when_data_present() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 3);
        sim.wake_all = true; // every pad pickable, wherever it is
        sim.step(&mut mesh);
        let a = Archive::parse(&d).unwrap();
        let section = a.world(3).unwrap();
        let mut tried = 0;
        let mut found = 0;
        for (i, o) in section.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let (Some(m), Some(st)) = (w.mesh.as_ref(), w.appearance.as_ref()) else { continue };
            for (p, poly) in m.polygons.iter().enumerate() {
                if sim.frame_flags(i, p) & SAVE_PAD == 0 || poly.len() < 3 || st.is_empty() {
                    continue;
                }
                let (pos, rot) = sim.pose(i);
                let t = sim::transform(pos, rot);
                let pts: Vec<V3> = poly.iter().map(|&k| sim::apply(&t, m.vertices[k as usize].map(|c| c as f32))).collect();
                let c = scale(pts.iter().fold([0.0; 3], |a, &p| add(a, p)), 1.0 / pts.len() as f32);
                tried += 1;
                let mendel = sim.creature.as_mut().unwrap();
                mendel.drop_at(add(c, scale(UP, 400.0)), 0);
                mendel.mode = Mode::Alive;
                let mut on = false;
                for _ in 0..24 {
                    sim.step(&mut mesh);
                    on |= sim.creature.as_ref().unwrap().on_save_pad;
                }
                // Aim at the panel along its normal, from either side.
                let n = normalize(cross(sub(pts[1], pts[0]), sub(pts[2], pts[0])));
                let hit = [1.0, -1.0].iter().find_map(|&s| sim.pick_hit(add(c, scale(n, 3000.0 * s)), scale(n, -s)).filter(|h| h.1 & SAVE_PAD != 0));

                println!("pad #{} poly {p}: flags {:#x} normal {n:?} visible {} on_save_pad {on}, pick {hit:?}, ground {:?}", i + 1, sim.frame_flags(i, p), sim.visible()[i], sim.creature.as_ref().unwrap().ground());
                if on && hit.is_some_and(|(_, f)| f & SAVE_PAD != 0) {
                    found += 1;
                }
            }
        }
        assert!(tried > 0);
        assert!(found > 0, "Mendel can stand on a save pad and click it");
    }
}

/// Afflictions and predators against the CD (docs/re/mendel.md §2.4,
/// docs/re/predators.md). They skip themselves without game data.
#[cfg(test)]
mod hazard_tests {
    use super::tests::{data, world};
    use galapagos_formats::worlds::Archive;
    use super::*;
    use crate::affliction::DURATION;

    /// Section 0 after the Space-key skip: Mendel alive on the arrival pad.
    fn alive_in_section0(d: &[u8]) -> (Sim, WorldMesh) {
        let (mut sim, mut mesh) = world(d, 0);
        for _ in 0..10 * sim::TICKS_PER_SECOND {
            sim.skip_held = (120..122).contains(&sim.tick);
            sim.step(&mut mesh);
        }
        sim.skip_held = false;
        assert_eq!(sim.creature.as_ref().unwrap().mode, Mode::Alive);
        (sim, mesh)
    }

    /// The speed affliction (3) of section 1's #166 / section 5's #176
    /// (frame flags 0x06000100): felt through his surface, it switches the
    /// gait to 53 and the muscles to (1, 6) steps, plays sound 39 and the
    /// strobe, and ends exactly 1440 ticks later.
    #[test]
    fn speed_affliction_changes_gait_and_ends_on_time() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = alive_in_section0(&d);
        sim.events.clear();
        sim.creature.as_mut().unwrap().react(0x0600_0100);
        sim.step(&mut mesh);
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(c.afflictions.kind, 3, "the surface afflicted him");
        assert_eq!((c.afflictions.gait(), c.afflictions.actuator_steps()), (53, (1, 6)));
        assert!(sim.events.iter().any(|e| e.kind == sim::EV_AFFLICTION_SOUND && e.a == 0x27 - 1), "sound 39");
        sim.step(&mut mesh);
        assert_eq!(sim.flash.remap, Some([255, 255, 255, 255, 255, 255]), "the screen strobes white");
        let start = sim.tick - 1;
        let mut ended = None;
        for _ in 0..2000 {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            if c.mode != Mode::Alive {
                break;
            }
            if c.afflictions.kind == 0 {
                ended = Some(sim.tick - start);
                break;
            }
        }
        println!("affliction 3 lasted {ended:?} ticks");
        assert_eq!(ended, Some(DURATION[3] as u64 + 1), "1440 ticks of effect, gone on the next");
        assert_eq!(sim.creature.as_ref().unwrap().afflictions.gait(), 24);
    }

    /// Afflictions 7/8/9 live on "delayed" lethal floors (e.g. section 3,
    /// 0x2ed93002: affliction 7, lethal kind 3 after 80 ticks): standing on
    /// one he is agitated (gait 40, cue sounds 1 and 2, flickers), then dies.
    #[test]
    fn agitating_floor_agitates_then_kills() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = alive_in_section0(&d);
        sim.events.clear();
        let mut agitated_ticks = 0;
        let mut died_at = None;
        for t in 0..200 {
            if let Some(c) = sim.creature.as_mut() {
                c.react(0x2ed9_3002);
            }
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            if c.afflictions.kind == 7 {
                agitated_ticks += 1;
                assert_eq!(c.afflictions.gait(), 40);
            }
            if matches!(c.mode, Mode::Vanishing(..)) {
                died_at = Some(t);
                break;
            }
        }
        let cues = sim.events.iter().filter(|e| e.kind == sim::EV_CREATURE_SOUND && (e.a == 0 || e.a == 1)).count();
        let crackles = sim.events.iter().filter(|e| e.kind == sim::EV_AFFLICTION_SOUND).count();
        println!("agitated {agitated_ticks} ticks, died at {died_at:?}, {cues} agitated cues, {crackles} crackles");
        assert!(agitated_ticks >= 79, "agitated while he stands there");
        assert_eq!(died_at, Some(79), "the delayed floor kills after 80 ticks of contact");
        assert!(cues >= 2, "the agitated cue (sounds 1, 2) plays");
        assert!(crackles >= 1, "crackles");
        // Death ends the affliction (respawn reason 3).
        for _ in 0..120 { // effect 3 v1: 60 ticks flattening, 40 held
            sim.step(&mut mesh);
        }
        assert_eq!(sim.creature.as_ref().unwrap().afflictions.kind, 0);
    }

    /// Under the shield (affliction 1) a lethal floor with bit 22 is
    /// harmless, and he sees it: the most common lethal floor on the CD,
    /// 0x01582380 (kind 2, pain), does nothing to him.
    #[test]
    fn shield_makes_lethal_floors_harmless() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = alive_in_section0(&d);
        sim.creature.as_mut().unwrap().react(0x0200_0100);
        sim.step(&mut mesh);
        assert_eq!(sim.creature.as_ref().unwrap().afflictions.kind, 1);
        for _ in 0..50 {
            sim.creature.as_mut().unwrap().react(0x0158_2380);
            sim.step(&mut mesh);
        }
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(c.mode, Mode::Alive, "still alive on a lethal floor");
        assert!(c.afflictions.look.texture.is_some(), "shimmering skin");
    }

    /// Affliction 1's skin slides: `+0x5c += 8` on every polygon each tick
    /// of its visual (0x42b200), so his texture coordinates move by 8/128
    /// in `u` (the vertex `u` minus his own offset), `v` never; touching
    /// the polygon again keeps it going; another affliction (here 3) and
    /// the end restore his records (`0x405f10`), offsets included.
    #[test]
    fn shield_scrolls_his_skin_along_u() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = alive_in_section0(&d);
        let uv = |sim: &Sim| {
            let v = &sim.creature.as_ref().unwrap().vertices;
            (v[3], v[4])
        };
        sim.creature.as_mut().unwrap().react(0x0200_0100);
        sim.step(&mut mesh);
        // Applied by the surface events, after this tick's effect tick.
        let c = sim.creature.as_ref().unwrap();
        assert_eq!((c.afflictions.kind, c.afflictions.look.scroll), (1, 0), "starts after the effects ran");
        for _ in 0..10 {
            sim.creature.as_mut().unwrap().react(0x0200_0100); // still touching it
            sim.step(&mut mesh);
        }
        assert_eq!(sim.creature.as_ref().unwrap().afflictions.look.scroll, 80, "re-arming keeps the offsets");
        // His first drawn vertex against his own offset plus 80.
        let c = sim.creature.as_ref().unwrap();
        let f = &c.body.pose_turning((c.phase >> 8) as u8, c.stance, c.turn, c.side).faces[0];
        let want_u = (f.uv[0][0] + ((f.offset[0].wrapping_add(80) & 0x1ff) as f32)) / 128.0;
        let want_v = (f.uv[0][1] + ((f.offset[1] & 0x1ff) as f32)) / 128.0;
        let (u1, v1) = uv(&sim);
        assert!((u1 - want_u).abs() < 1e-5 && (v1 - want_v).abs() < 1e-5, "({u1}, {v1}) vs ({want_u}, {want_v})");
        // Another affliction ends this one: records restored.
        sim.creature.as_mut().unwrap().react(0x0600_0100);
        sim.step(&mut mesh);
        let c = sim.creature.as_ref().unwrap();
        assert_eq!((c.afflictions.kind, c.afflictions.look.scroll, c.afflictions.look.texture), (3, 0, None));
        // And running out: 1440 ticks of visual, then his own skin again.
        sim.creature.as_mut().unwrap().react(0x0200_0100);
        sim.step(&mut mesh);
        for _ in 0..1440 {
            sim.step(&mut mesh);
        }
        let c = sim.creature.as_ref().unwrap();
        assert_eq!((c.afflictions.kind, c.afflictions.look.scroll), (1, 1440 * 8), "the last tick of its 1440");
        sim.step(&mut mesh);
        let c = sim.creature.as_ref().unwrap();
        assert_eq!((c.afflictions.kind, c.afflictions.look.scroll), (0, 0), "ended and restored");
    }

    /// The most common lethal floor, 0x01582380 (effect kind 2 v0): he
    /// bursts into pieces for 100 ticks (flash 6, sound 10), respawns, and
    /// the pieces fly back together for 60 (effect 2 v0x40), then he lives.
    /// He is drawn all along: no blinking.
    #[test]
    fn lethal_floor_bursts_him_and_he_reassembles() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = alive_in_section0(&d);
        sim.events.clear();
        sim.creature.as_mut().unwrap().react(0x0158_2380);
        let (mut dying, mut coming, mut alive_at) = (0, 0, None);
        let mut widest = 0.0f32;
        for t in 0..400 {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            match c.mode {
                Mode::Vanishing(..) => dying += 1,
                Mode::Appearing(_) => coming += 1,
                _ => {}
            }
            if c.mode != Mode::Alive {
                assert!(!c.vertices.is_empty(), "drawn on tick {t}");
                let far = c.vertices.chunks(FLOATS_PER_VERTEX).map(|v| length(sub([v[0], v[1], v[2]], c.pos))).fold(0.0, f32::max);
                widest = widest.max(far);
            } else if dying > 0 {
                alive_at = Some(t);
                break;
            }
        }
        println!("burst: {dying} ticks dying, {coming} coming back, alive at {alive_at:?}, pieces up to {widest:.0} away");
        assert_eq!(dying, 101, "the tick it starts, then 100 steps");
        assert_eq!(coming, 60);
        assert!(widest > 1000.0, "the pieces fly apart");
        assert_eq!(sim.creature.as_ref().unwrap().stats.deaths, 1);
        let sounds: Vec<i32> = sim.events.iter().filter(|e| e.kind == sim::EV_EFFECT_SOUND).map(|e| e.a + 1).collect();
        assert_eq!(sounds, [10, 18], "the burst's and the reassembly's");
    }

    /// A dimensional gate (portal kind 3, reaction 0x251000): he fades
    /// out polygon by polygon (effect 1 v5) while the screen bleaches to
    /// white (flash 4: 40 ticks, then 10 frames to white, 254 after the
    /// 8.8 lerp, which stays),
    /// and only then does the host get the section change. The footage
    /// shows the white-out ~1 s before the time screen (t = 594 s).
    #[test]
    fn a_gate_whites_out_before_the_section_changes() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = alive_in_section0(&d);
        sim.events.clear();
        let ev = sim::HostEvent { kind: sim::EV_SECTION, a: 1, b: 0, c: 0 };
        sim.requests.push(Request::LevelEnd(ev));
        let (mut white_at, mut sent_at) = (None, None);
        for t in 0..400 {
            sim.step(&mut mesh);
            if white_at.is_none() && sim.flash.current().iter().all(|&c| c >= 250) {
                white_at = Some(t);
            }
            if sim.events.iter().any(|e| *e == ev) {
                sent_at = Some(t);
                break;
            }
        }
        println!("gate: white at {white_at:?}, section change at {sent_at:?}");
        assert!(white_at.is_some_and(|t| (49..=52).contains(&t)), "40 held + 10 frames of the ramp");
        let sent = sent_at.expect("the section change is sent");
        assert!(sent > 60 && sent < 150, "after the effect, not at the touch: {sent}");
    }

    /// The transition effects the predators play: the ball's appearance
    /// (`shorts [2, 1, 1, 1]`: effect 1 v1 backwards = 0x41, every polygon
    /// fading in from black within 7 ticks) paints its polygons, and the
    /// paint is gone when it ends.
    #[test]
    fn the_ball_fades_in() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 2);
        sim.wake_all = true;
        for _ in 0..20 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().mode == Mode::Alive {
                break;
            }
        }
        let ball = 54 - 1;
        let draw = mesh.draw_of_object[ball].unwrap();
        let r = mesh.draws[draw];
        let range = r.first as usize * FLOATS_PER_VERTEX..(r.first + r.count) as usize * FLOATS_PER_VERTEX;
        let clean = mesh.vertices[range.clone()].to_vec();
        sim.debug_goto(ball, 3);
        let k = sim.predators.iter().position(|p| p.obj == ball).unwrap();
        let mut ticks = 0;
        let mut lit_seen = false;
        for _ in 0..40 {
            sim.step(&mut mesh);
            let v = &mesh.vertices[range.clone()];
            lit_seen |= v.chunks(FLOATS_PER_VERTEX).any(|v| v[6] < 0.0);
            if sim.predators[k].mode == 2 {
                ticks += 1;
            }
            if sim.predators[k].mode == 3 {
                break;
            }
        }
        assert_eq!(sim.predators[k].mode, 3, "active after its effect");
        assert!(lit_seen, "polygons brightened by the effect");
        assert!((7..=9).contains(&ticks), "{ticks} ticks appearing");
        let v = &mesh.vertices[range.clone()];
        for (a, b) in v.chunks(FLOATS_PER_VERTEX).zip(clean.chunks(FLOATS_PER_VERTEX)) {
            assert_eq!(a[3..], b[3..], "its own uv, layer, material and normal again");
        }
    }

    /// The affliction sounds are world-bank sounds (1-based), the same
    /// slots in every world. Those of the afflictions the CD uses (1, 3,
    /// and 7/8/9's crackles) and of 5 exist in all eight banks; those of
    /// the unused 4, 6, 10, 11, 12 (40, 42, 52..54) are empty slots
    /// everywhere: the designers never shipped them.
    #[test]
    fn affliction_sounds_exist_in_every_world() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        for s in 0..a.sections.len() {
            let bank = crate::sound::Bank::decode(&d, &a.world(s).unwrap().sounds, 1);
            let has = |n: usize| bank.sounds.get(n - 1).is_some_and(|x| x.is_some());
            for n in [0x25, 0x27, 0x29, 0x2b, 0x2c, 0x2d, 0x2e, 0x2f, 0x30] {
                assert!(has(n), "section {s}: sound {n}");
            }
            for n in [0x28, 0x2a, 0x34, 0x35, 0x36] {
                assert!(!has(n), "section {s}: sound {n} is an empty slot");
            }
        }
    }

    /// Section 2's ball `PBal` #54, activated as its state machine does
    /// (trigger 1 → `shorts [2, 1, 1, 1]`: appear at the `PPol` #55), chases
    /// Mendel and kills him on contact through his click hull `MEnc`.
    #[test]
    fn ball_appears_chases_and_bites_mendel() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 2);
        sim.wake_all = true;
        // Let the section start, spawn Mendel by its Strt.
        for _ in 0..20 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().mode == Mode::Alive {
                break;
            }
        }
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(c.mode, Mode::Alive, "spawned in section 2");
        let ball = 54 - 1;
        let dist = |sim: &Sim| {
            let p = sim.position_of(ball as u16 + 1).map(|v| v as f32);
            length(sub(p, sim.creature.as_ref().unwrap().pos))
        };
        // 1. Its state 3 is what the launcher #55 sends it to: entry
        // trigger 1 (`shorts [2, 1, 1, 1]`), two ticks later state 1, where
        // it hunts for 100 ticks, then state 2 (trigger 2: vanish).
        sim.debug_goto(ball, 3);
        sim.step(&mut mesh);
        let k = sim.predators.iter().position(|p| p.obj == ball).unwrap();
        assert!(matches!(sim.predators[k].mode, 2 | 3), "appearing");
        let mut first = None;
        let mut closest = f32::MAX;
        let mut parked = None;
        for t in 0..300 {
            sim.step(&mut mesh);
            let mode = sim.predators[k].mode;
            if first.is_none() && mode == 3 {
                first = Some(dist(&sim));
            }
            if mode == 3 {
                closest = closest.min(dist(&sim));
            }
            // Mid-chase, a saved game carries the ball's state (GLPS v2 on).
            if t == 40 {
                sim.take_snapshot(); // what the Save dialog does
                let bytes = sim.save_game();
                let (mut other, mut other_mesh) = world(&d, 2);
                other.load_game(&bytes, &mut other_mesh).unwrap();
                assert!(other.predators == sim.predators, "predators restored");
                assert!(other.save_game() == bytes, "load then save is the identity");
            }
            if first.is_some() && mode == 0 {
                parked = Some((t, sim.position_of(ball as u16 + 1)));
                break;
            }
        }
        let d0 = first.expect("the ball becomes active");
        println!("ball: active {d0:.0} away, came within {closest:.0}, vanished {parked:?}");
        assert!(closest < d0 - 15_000.0, "it chases him");
        assert_eq!(parked.map(|p| p.1), Some([100_000; 3]), "after 100 ticks it vanishes and is parked");

        // 2. Launched close to him (Mendel dropped 1,500 under the launcher),
        // it bites within a second or two, before a fall could kill him.
        let deaths = sim.creature.as_ref().unwrap().stats.deaths;
        let pole = sim.position_of(55).map(|v| v as f32);
        sim.events.clear();
        let c = sim.creature.as_mut().unwrap();
        c.drop_at(add(pole, [0.0, 0.0, -1500.0]), 0);
        c.mode = Mode::Alive;
        sim.debug_goto(ball, 3);
        let mut bite = None;
        for t in 0..60 {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().stats.deaths > deaths {
                bite = Some(t);
                break;
            }
        }
        let growls = sim.events.iter().filter(|e| e.kind == sim::EV_PREDATOR_SOUND).count();
        println!("bitten after {bite:?} ticks, {growls} predator sounds");
        assert!(bite.is_some_and(|t| t < 50), "the ball kills Mendel through MEnc");
        assert!(growls > 0, "it growls / bites (predator bank)");
    }
}

/// The polygon walker against the CD (docs/re/mendel.md §2.2): tilted
/// pads, the upside-down start of section 2, seams between polygons and
/// saved games with a tilted contact. They skip themselves without data.
#[cfg(test)]
mod walker_tests {
    use super::tests::{data, world};
    use super::*;
    use crate::walker::Mover;
    use galapagos_formats::logic::PortalAction;
    use galapagos_formats::worlds::{Archive, Object};

    /// Runs until Mendel is alive (the section's `Strt` spawned him).
    pub(super) fn until_alive(sim: &mut Sim, mesh: &mut WorldMesh, secs: u32) {
        for _ in 0..secs * sim::TICKS_PER_SECOND {
            sim.step(mesh);
            if sim.creature.as_ref().unwrap().mode == Mode::Alive {
                return;
            }
        }
        panic!("Mendel never came alive");
    }

    /// Every teleport of section `s` whose destination polygon does not
    /// face +z: (object index, params entry, polygon, local normal).
    pub(super) fn tilted_teleports(d: &[u8], s: usize) -> Vec<(usize, usize, usize, V3)> {
        let a = Archive::parse(d).unwrap();
        let section = a.world(s).unwrap();
        let polys = crate::walker::local_polys(&section);
        let mut out = Vec::new();
        for (i, o) in section.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            for (e, p) in w.logic.params.iter().enumerate() {
                let PortalAction::Teleport { mode, .. } = p.portal() else { continue };
                let Some(lp) = polys[i].get(mode.max(0) as usize).filter(|lp| lp.points.len() >= 3) else { continue };
                if lp.normal[2] < 0.99 && &w.kind == b"Tele" {
                    out.push((i, e, mode as usize, lp.normal));
                }
            }
        }
        out
    }

    /// Section 1's teleporters put Mendel on pads that face +y (the level
    /// stands on its side there). Each lands him on its polygon, standing
    /// in its frame (his up = its normal), and he walks off without
    /// falling: the polygon is his floor whatever way it faces.
    #[test]
    fn teleports_onto_tilted_pads_in_section1() {
        let Some(d) = data() else { return };
        let pads = tilted_teleports(&d, 1);
        assert!(pads.len() >= 18, "section 1 has its sideways pads ({})", pads.len());
        let (mut sim, mut mesh) = world(&d, 1);
        sim.wake_all = true;
        until_alive(&mut sim, &mut mesh, 30);
        let mut report = Vec::new();
        for &(i, entry, poly, normal) in &pads {
            // The pad's own trigger (portal kind 1/2/7): it queues the
            // teleport; Mendel vanishes, then rematerialises on the pad.
            let c = sim.creature.as_mut().unwrap();
            c.drop_at([0.0, 0.0, 1.0e6], 0); // far from every pad, so the request counts
            c.mode = Mode::Alive;
            sim.debug_trigger(i, entry, &mut mesh);
            let mut landed = None;
            for _ in 0..3 * sim::TICKS_PER_SECOND {
                sim.step(&mut mesh);
                let c = sim.creature.as_ref().unwrap();
                if c.mode == Mode::Alive {
                    landed = Some((c.ground(), c.up, c.mover.h, c.stance_height));
                    break;
                }
            }
            let (ground, up, h, stance) = landed.expect("he rematerialises");
            let want = sim::transform([0; 3], sim.pose(i).1);
            let wn = [
                want[0] * normal[0] + want[1] * normal[1] + want[2] * normal[2],
                want[3] * normal[0] + want[4] * normal[1] + want[5] * normal[2],
                want[6] * normal[0] + want[7] * normal[1] + want[8] * normal[2],
            ];
            assert_eq!(ground, Some((i, poly)), "pad #{} poly {poly}: attached to it", i + 1);
            assert!(dot(up, wn) > 0.999, "pad #{}: upright in its frame, up {up:?} vs normal {wn:?}", i + 1);
            assert!((h - stance).abs() <= 2, "pad #{}: at his stance height ({h} vs {stance})", i + 1);
            // Two seconds of his own walking: still on a floor at the end,
            // never a long fall.
            let (mut grounded, mut longest_fall) = (0, 0);
            for _ in 0..2 * sim::TICKS_PER_SECOND {
                sim.step(&mut mesh);
                let c = sim.creature.as_ref().unwrap();
                grounded += c.ground().is_some() as u32;
                longest_fall = longest_fall.max(c.fall_count);
            }
            let c = sim.creature.as_ref().unwrap();
            report.push((i + 1, poly, grounded, longest_fall, c.stats.walked as i32));
            assert!(grounded * 10 >= 2 * sim::TICKS_PER_SECOND * 9, "pad #{}: grounded {grounded} of 48 ticks", i + 1);
            assert_eq!(c.mode, Mode::Alive, "pad #{}: alive", i + 1);
        }
        println!("pads (id, poly, grounded ticks /48, longest fall, walked): {report:?}");
    }

    /// Section 2 begins upside down: its `Strt` #194 puts Mendel on a
    /// polygon facing −z, and the walker stands him on it, his up −z. No
    /// "wake every machine" needed: folder #193 (which holds #194) tests
    /// #178's state on its first tick, and the level load has already
    /// entered #178's initial state 1 (`World_Read` @ 0x4222d0), so the
    /// folder stays on and #194 counts its 100 ticks.
    #[test]
    fn section2_starts_on_its_downward_start_polygon() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 2);
        assert!(!sim.wake_all);
        assert_eq!(sim.state_of(178), 1, "#178 entered its initial state at load");
        for _ in 0..30 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().mode != Mode::Absent {
                break;
            }
        }
        let c = sim.creature.as_ref().unwrap();
        let (o, p) = c.ground().expect("standing from the first tick");
        println!("section 2 start: on #{} poly {p}, up {:?}, pos {:?}, stance {}", o + 1, c.up, c.pos, c.stance_height);
        assert_eq!(o + 1, 194, "on the Strt object");
        assert!(c.up[2] < -0.999, "upside down: up {:?}", c.up);
        // His origin hangs his stance height *below* the polygon. Spawned
        // without an effect (`ints[5] = 0`) he is alive at once (mode 2):
        // that first tick's muscles may already move his stance a little.
        let lp = &c.polys[o][p];
        let plane = sim::apply(&sim::transform(sim.pose(o).0, sim.pose(o).1), lp.points[0]);
        assert!((plane[2] - c.pos[2] - c.stance_height as f32).abs() < 4.0, "origin {} under the plane {}", c.pos[2], plane[2]);
    }

    /// Section 2's belt #197 (polygon 2, flags 0x6, facing −z next to the
    /// start #194) carries Mendel at the exe's speed: its flip-book slides
    /// the texture 8 texels a frame along t (`v`: 0, −8, −16 …), one frame
    /// a tick. Its texels (shape state 0, frame 0, the frame `0x41d9e0`
    /// puts on the mesh before `0x41d6d0` builds the basis) are (111.4, 0)
    /// (0, 0) (0, −602.8) at vertices 0, 1, 2: `t` runs along v1→v2, the
    /// belt's length, about 19.9 units a texel, stored × −256 as 5100;
    /// `0x41dc30` moves him `(4400 · −8 >> 8) >> 0 = −138` along the
    /// polygon's `u` every tick, along the belt and through the opening
    /// at its end. inferred: no footage of section 2. (With the
    /// initialiser's texels (0,127) (0,0) (127,0), which the port used
    /// before shape frames were ported, the flow ran *across* the belt.)
    #[test]
    fn section2_belt_carries_him_at_the_exes_speed() {
        let Some(d) = data() else { return };
        let belt = 197 - 1;
        {
            // Before its appearance is applied the scroll record is the
            // zeros 0x41d9e0 wrote: no flow. (The load pass of 0x4222d0
            // applies it, so only a section not yet entered shows this.)
            let a = Archive::parse(&d).unwrap();
            let section = a.world(2).unwrap();
            let sim = Sim::build(&section, &WorldMesh::build(&section));
            assert_eq!(sim.uv_step(belt, 2), (0, 0), "no frame applied yet");
        }
        let (mut sim, mut mesh) = world(&d, 2);
        sim.wake_all = true;
        for _ in 0..4 {
            sim.step(&mut mesh);
        }
        assert_eq!(sim.uv_step(belt, 2), (0, -8), "8 texels a frame along t");
        let c = sim.creature.take().unwrap();
        let m = c.polys[belt][2].texel.map(|x| (-256.0 * x) as i32);
        assert_eq!(m, [0, 4400, -5100, 0], "the exe's short basis");
        let mut w = Mover::default();
        w.place(&c.polys, &mut sim, Some((belt, 2)), [5000.0, -1800.0, 124.0], 0, 124);
        assert!(w.grounded && w.on == Some((belt, 2)), "standing on the belt: {:?}", w.on);
        let drive = Drive { forward: 0, turn: 0, stance: 124, gravity: -7, injected: 0 };
        let mut posted = Vec::new();
        let mut steps = Vec::new();
        for _ in 0..32 {
            let (u, v) = (w.u, w.v);
            let near = Near::gather(&c.polys, &sim, w.pos, 3200.0, SOLID | FLOOR | HAZARD);
            w.step(&c.polys, &mut sim, &near, &drive, &mut posted);
            assert!(w.on == Some((belt, 2)) && w.grounded, "still on the belt");
            assert_eq!(w.v, v, "nothing moves him across it");
            steps.push(w.u - u);
        }
        assert!(steps.iter().all(|&d| d == -138.0), "one conveyor step a tick: {steps:?}");
        // The belt runs into the wall #201 (polygon 7, x = −19980), but
        // through the opening that wall is drawn round (a keyhole polygon,
        // framed by polygon 8), onto the next belt #177 (polygon 2, flags
        // 0x6) on the far side. The opening's frame nudges him to the
        // middle; nothing holds him.
        let next = 177 - 1;
        let mut on = vec![w.on];
        for _ in 0..80 {
            let near = Near::gather(&c.polys, &sim, w.pos, 3200.0, SOLID | FLOOR | HAZARD);
            w.step(&c.polys, &mut sim, &near, &drive, &mut posted);
            assert!(w.grounded, "never falls: {:?}", w.pos);
            if on.last() != Some(&w.on) {
                on.push(w.on);
            }
        }
        assert!(on.contains(&Some((next, 2))) && w.pos[0] < -20500.0, "carried through the opening onto #177: {on:?}, at {:?}", w.pos);
    }

    /// Section 1's sideways pads are drawn on every tick. Their flip-books
    /// are one enabled frame and 31 disabled `-3` frames that only slide
    /// the texture (#49 polygon 0: `(8, e1), (-3, e0, +4/+4) × 31`); the
    /// exe's frame appliers (0x423730, 0x423810) never read `enabled`, so
    /// the polygon keeps texture 8 throughout.
    #[test]
    fn section1_pads_stay_drawn_through_their_flip_books() {
        let Some(d) = data() else { return };
        let pads = tilted_teleports(&d, 1);
        let (mut sim, mut mesh) = world(&d, 1);
        sim.wake_all = true;
        let layer = |mesh: &WorldMesh, i: usize, p: usize| {
            let draw = mesh.draw_of_object[i].unwrap();
            let r = mesh.poly_ranges[draw][p];
            (mesh.visible[draw], mesh.vertices[r.first as usize * FLOATS_PER_VERTEX + 5])
        };
        for t in 0..64 {
            sim.step(&mut mesh);
            for &(i, _, poly, _) in &pads {
                let (vis, l) = layer(&mesh, i, poly);
                assert!(vis == 1 && l >= 0.0, "tick {t}: pad #{} polygon {poly} drawn (visible {vis}, layer {l})", i + 1);
            }
        }
        assert_eq!(layer(&mesh, 49 - 1, 0).1, 8.0, "#49 keeps texture 8");
    }

    /// The intro capsule #3 flies its motion path through openings in the
    /// walls (footage 0:44..1:40): it enters the octagonal hall through
    /// the hexagonal hole of wall #101 (x = 14060, z ≈ 14000). Every tick
    /// of the ride, until it has dropped Mendel on the arrival pad #83,
    /// the segment its origin moves along crosses no drawn, solid polygon
    /// of another object; with the keyhole walls fanned it crossed #101.
    /// (Afterwards it rises straight through the hall's ceiling #97, a
    /// convex octagon: its motion path, not checked against footage.)
    #[test]
    fn intro_capsule_flies_through_openings_when_data_present() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        let capsule = 3 - 1;
        let polys = walker::local_polys(&Archive::parse(&d).unwrap().world(0).unwrap());
        let mut prev = sim.pose(capsule).0.map(|c| c as f32);
        let (mut moved, mut crossed) = (0.0, Vec::new());
        for _ in 0..140 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            if c.mode == Mode::Alive && c.ground().map(|g| g.0 + 1) == Some(83) {
                break;
            }
            let now = sim.pose(capsule).0.map(|c| c as f32);
            let len = length(sub(now, prev));
            if len > 0.0 {
                moved += len;
                let near = Near::gather(&polys, &sim, prev, len, SOLID);
                for h in near.segment_all(prev, now, |t| t.obj != capsule) {
                    let draw = mesh.draw_of_object[h.obj].unwrap();
                    let r = mesh.poly_ranges[draw][h.poly];
                    let drawn = mesh.visible[draw] == 1 && r.count > 0 && mesh.vertices[r.first as usize * world::FLOATS_PER_VERTEX + 5] >= 0.0;
                    if drawn {
                        crossed.push((sim.tick, h.obj + 1, h.poly, h.point.map(|c| c as i32)));
                    }
                }
            }
            prev = now;
        }
        println!("capsule flew {moved:.0} units until tick {}; crossed {crossed:?}", sim.tick);
        assert!(moved > 300_000.0 && sim.tick < 140 * sim::TICKS_PER_SECOND as u64, "the capsule flies and delivers him: {moved}");
        assert!(crossed.is_empty(), "through drawn solid polygons: {crossed:?}");
    }

    /// Section 0's walkway #94 (top polygon 4, z = 2960, from y = −370 to
    /// −15170) ends at the octagonal hall's wall #100, where a window
    /// (x −2220..0, z 2960..5920, framed by #100's grey polygon 0) opens
    /// on the corridor #79 (floor polygon 2, z = 2960, on to y = −22200),
    /// where #77 (texture 14, flags 0x15e2380: solid, with event bits)
    /// stands across it at y ≈ −17000..−18600. #100's wall
    /// polygon 2 (flags 0x340: solid) is one keyhole outline round that
    /// window; fanned, it bricked the window up and the hull stopped him
    /// 450 units short of it. Triangulated as the exe draws it (and as
    /// `World_Cast`'s even-odd test `0x407830` sees it) he walks through.
    #[test]
    fn section0_walkway_leads_through_the_window_when_data_present() {
        let Some(d) = data() else { return };
        let (mut sim, _mesh) = world(&d, 0);
        let c = sim.creature.take().unwrap();
        let (walkway, corridor, wall) = (94 - 1, 79 - 1, 100 - 1);
        let mut w = Mover::default();
        // Heading −y (768 = three quarters of a turn).
        w.place(&c.polys, &mut sim, None, [-1110.0, -12000.0, 3060.0], 768, 124);
        assert!(w.grounded && w.on == Some((walkway, 4)), "on the walkway: {:?}", w.on);
        let mut posted = Vec::new();
        let mut on = vec![w.on];
        for _ in 0..400 {
            let near = Near::gather(&c.polys, &sim, w.pos, 3200.0, SOLID | FLOOR | HAZARD);
            let drive = Drive { forward: 30, turn: 0, stance: 124, gravity: -7, injected: 0 };
            w.step(&c.polys, &mut sim, &near, &drive, &mut posted);
            assert!(w.grounded, "never falls: at {:?}", w.pos);
            if on.last() != Some(&w.on) {
                on.push(w.on);
            }
        }
        println!("walkway → corridor: polygons {on:?}, now at {:?}", w.pos);
        assert!(w.pos[1] < -16000.0, "through the window, into the corridor: {:?}", w.pos);
        assert!(on.contains(&Some((corridor, 2))), "on the corridor's floor: {on:?}");
        // The wall is still a wall beside the window: the same hull ray,
        // 2000 units to the side, is stopped by #100's polygon 2.
        let near = Near::gather(&c.polys, &sim, [-1110.0, -15170.0, 3500.0], 6000.0, SOLID);
        let ray = |x: f32| near.segment([x, -14500.0, 3500.0], [x, -15800.0, 3500.0], Faces::Both, |t| t.flags & SOLID != 0).map(|h| (h.obj, h.poly));
        assert_eq!(ray(-1110.0), None, "the window is open");
        assert_eq!(ray(-4000.0), Some((wall, 2)), "the wall beside it is solid");
        assert_eq!(ray(2500.0), Some((wall, 2)), "on both sides");
    }

    /// Walking across the seam between two polygons of one floor never
    /// lets go. Section 0's arrival pad (#83 over #82) is a flat top with
    /// two sloping ramps; driven straight from the top across a ramp, the
    /// mover goes from polygon to polygon (re-attaching at each seam,
    /// turning across the hinge) and is grounded every tick until he walks
    /// off the ramp's far end.
    #[test]
    fn crossing_polygon_seams_keeps_him_grounded() {
        let Some(d) = data() else { return };
        let (mut sim, _mesh) = world(&d, 0);
        let c = sim.creature.take().unwrap();
        let pad = 82 - 1;
        let (pos, rot) = sim.pose(pad);
        let m = sim::transform(pos, rot);
        let top = sim::apply(&m, c.polys[pad][2].centre);
        let mut crossed = 0;
        for ramp in [0usize, 1] {
            let rc = sim::apply(&m, c.polys[pad][ramp].centre);
            let dir = normalize([rc[0] - top[0], rc[1] - top[1], 0.0]);
            let heading = (dir[1].atan2(dir[0]) * 1024.0 / core::f32::consts::TAU).round() as i32;
            let mut w = Mover::default();
            w.place(&c.polys, &mut sim, None, add(top, [0.0, 0.0, 100.0]), heading, 124);
            // #83 is the same pad drawn 7 units higher (top = polygon 9,
            // ramps 3 and 6); its floors are set from the level load on
            // (0x4222d0 enters its initial state), so it is the one he
            // stands on.
            let shell = 83 - 1;
            assert!(w.grounded && w.on == Some((shell, 9)), "placed over the top, he stands on it: {:?}", w.on);
            let (ramp, m) = ([3usize, 6][ramp], sim::transform(sim.pose(shell).0, sim.pose(shell).1));
            let mut seq = vec![w.on];
            let mut posted = Vec::new();
            let mut let_go = None;
            for t in 0..300 {
                let near = Near::gather(&c.polys, &sim, w.pos, 3200.0, SOLID | FLOOR | HAZARD);
                let drive = Drive { forward: 30, turn: 0, stance: 124, gravity: -7, injected: 0 };
                w.step(&c.polys, &mut sim, &near, &drive, &mut posted);
                if !w.grounded {
                    let_go = Some((t, w.pos));
                    break;
                }
                if seq.last() != Some(&w.on) {
                    seq.push(w.on);
                }
            }
            println!("towards ramp {ramp}: polygons {seq:?}, let go {let_go:?}, now at {:?} (walked {} ticks)", w.pos, let_go.map_or(300, |l| l.0));
            if seq.iter().any(|&o| o == Some((shell, ramp))) {
                crossed += 1;
                // If he let go, it was past the ramp's lower end (or off
                // the side where it meets the ground), never on the seam.
                if let Some((_, at)) = let_go {
                    let lp = &c.polys[shell][ramp];
                    let lowest = lp.points.iter().map(|p| sim::apply(&m, *p)[2]).fold(f32::MAX, f32::min);
                    assert!(at[2] < lowest + 200.0, "let go at the ramp's lower end ({lowest}), not on a seam: {at:?}");
                }
            }
        }
        assert!(crossed >= 1, "he crossed from the top onto a ramp");
    }

    /// A saved game taken while Mendel stands on a tilted pad (section 1,
    /// facing +y) keeps the full contact: loading puts him back on that
    /// polygon in its frame, saving again gives the same bytes, and a
    /// death respawns him there (the respawn point is a polygon place).
    #[test]
    fn saved_game_keeps_a_tilted_contact() {
        let Some(d) = data() else { return };
        let pads = tilted_teleports(&d, 1);
        let (i, entry, poly, _) = pads[0];
        let (mut sim, mut mesh) = world(&d, 1);
        sim.wake_all = true;
        until_alive(&mut sim, &mut mesh, 30);
        let c = sim.creature.as_mut().unwrap();
        c.drop_at([0.0, 0.0, 1.0e6], 0);
        sim.debug_trigger(i, entry, &mut mesh);
        for _ in 0..3 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().mode == Mode::Alive {
                break;
            }
        }
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(c.ground(), Some((i, poly)));
        let (up, pos) = (c.up, c.pos);
        sim.take_snapshot();
        let bytes = sim.save_game();
        let (mut other, mut other_mesh) = world(&d, 1);
        other.load_game(&bytes, &mut other_mesh).unwrap();
        let o = other.creature.as_ref().unwrap();
        assert_eq!(o.ground(), Some((i, poly)), "loaded on the same polygon");
        assert!(dot(o.up, up) > 0.9999 && length(sub(o.pos, pos)) < 0.01, "same frame and spot");
        assert!(other.save_game() == bytes, "load then save is the identity");
        // Die: back on the tilted pad, in its frame.
        other.requests.push(Request::Respawn);
        for _ in 0..3 * sim::TICKS_PER_SECOND {
            other.step(&mut other_mesh);
            if other.creature.as_ref().unwrap().mode == Mode::Alive {
                break;
            }
        }
        let o = other.creature.as_ref().unwrap();
        println!("respawned on {:?} up {:?} ({} from the saved spot)", o.ground(), o.up, length(sub(o.pos, pos)));
        assert_eq!(o.ground(), Some((i, poly)), "respawned on the pad");
        assert!(dot(o.up, up) > 0.999, "in its frame");
    }

    /// Version 2 saved games still open: the old world-space contact is
    /// re-expressed in the walker's polygon coordinates.
    #[test]
    fn version2_creature_converts_to_the_walker() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        for _ in 0..10 * sim::TICKS_PER_SECOND {
            sim.skip_held = (120..122).contains(&sim.tick);
            sim.step(&mut mesh);
        }
        let c = sim.creature.as_ref().unwrap();
        let (on, pos, up, heading) = (c.ground().unwrap(), c.pos, c.up, c.heading());
        // The v2 layout, written by hand: mode, pos/fwd/up, five ints, air,
        // air ticks, the contact, then the agent block and the old points.
        let mut w = Writer::default();
        w.u8(2);
        w.u16(0);
        w.u8(0);
        [0.0f32; 3].iter().for_each(|&x| w.f32(x));
        w.i32(0);
        for v in [pos, c.fwd(), up] {
            v.iter().for_each(|&x| w.f32(x));
        }
        [0, 0, c.mover.h, 0, c.stance_height].iter().for_each(|&x| w.i32(x));
        [0.0f32; 3].iter().for_each(|&x| w.f32(x));
        w.u32(0);
        w.u8(1);
        w.u32(on.0 as u32);
        w.u32(on.1 as u32);
        [0.0f32; 9].iter().for_each(|&x| w.f32(x));
        let mut agent = Writer::default();
        c.saved().write(&mut agent);
        // In the v3 layout the agent block (surface .. side, 188 bytes)
        // follows the mode (29 bytes), the mover (123) and three ints (12).
        let start = 29 + 123 + 12;
        w.bytes(&agent.0[start..start + 188]);
        for _ in 0..3 {
            w.u8(1);
            pos.iter().for_each(|&x| w.f32(x));
            w.i32(heading);
        }
        let n = agent.0.len();
        w.bytes(&agent.0[n - 16 - 38..n - 16]); // stats, rng, cue timers
        w.u32(0);
        w.u16(0);
        w.u16(0);
        w.u32(0);
        w.i32(0);
        w.i32(0);
        let mut r = Reader { data: &w.0, at: 0 };
        let s = SavedCreature::read(&mut r, 2).unwrap();
        assert_eq!(r.at, w.0.len(), "the whole v2 record was read");
        let mut c = sim.creature.take().unwrap();
        c.apply_saved(s, &sim);
        sim.creature = Some(c);
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(c.ground(), Some(on), "the contact survives");
        assert!(length(sub(c.pos, pos)) < 1.0 && dot(c.up, up) > 0.999 && (c.heading() - heading).abs() <= 1);
    }
}

/// The level load (`World_Read` @ 0x4222d0) enters every initial state
/// before the first tick; without it, folders read their neighbours in
/// state 0 on tick 0 and some switch whole areas off for good.
#[cfg(test)]
mod level_load_tests {
    use super::tests::{data, world};
    use super::*;
    use galapagos_formats::worlds::{Archive, Object};

    /// After `Sim::new`, before any tick: every object is in its initial
    /// state (or the one its entry actions chain to, 0x42ce70).
    #[test]
    fn the_load_enters_every_initial_state() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        for s in 0..a.sections.len() {
            let section = a.world(s).unwrap();
            let (sim, _mesh) = world(&d, s);
            for (i, o) in section.objects.iter().enumerate() {
                let Object::World(w) = o else { continue };
                let Some(sm) = w.logic.states.as_ref() else { continue };
                let init = w.initial_state();
                let chained = sm.states.get(init as usize).and_then(|st| st.entry.goto_state).map(|g| g as u16);
                let now = sim.state_of(i as u16 + 1);
                assert!(now == init || Some(now) == chained, "section {s} #{}: state {now}, initial {init}", i + 1);
            }
        }
    }

    /// Every section that has a `Strt` brings Mendel to life with the
    /// folders gating as in the original (no "wake every machine").
    #[test]
    fn every_start_spawns_mendel_without_waking_all() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let mut report = Vec::new();
        for s in 0..a.sections.len() {
            let section = a.world(s).unwrap();
            if !section.objects.iter().any(|o| &o.kind() == b"Strt") {
                continue;
            }
            let (mut sim, mut mesh) = world(&d, s);
            assert!(!sim.wake_all);
            let mut born = None;
            for t in 0..60 * sim::TICKS_PER_SECOND {
                sim.step(&mut mesh);
                let c = sim.creature.as_ref().unwrap();
                if c.mode != Mode::Absent {
                    born = Some((t, c.ground().map(|(o, p)| (o + 1, p))));
                    break;
                }
            }
            report.push((s, born));
            // (Where he lands is the walker's business: in section 5 the
            // pad's spot rests on an `Objt`, in section 6 he drops in.)
            assert!(born.is_some(), "section {s}: Mendel never spawned");
        }
        println!("spawns (section, (tick, on)): {report:?}");
        assert!(!report.is_empty());
    }

    /// Section 2 specifically: alive on #194's downward polygon 1 a few
    /// seconds in (the `Strt` waits 100 ticks, then 11 of rematerialising).
    /// Polygon 1 is the hexagonal plate the portal names, set 74 units
    /// into the ceiling inside the ring polygon 6 (a keyhole outline round
    /// it): with the ring fanned, its filled hole caught the probe first.
    #[test]
    fn section2_spawns_mendel_on_194() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 2);
        for _ in 0..5 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
        }
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(c.mode, Mode::Alive);
        assert_eq!(c.ground().map(|(o, p)| (o + 1, p)), Some((194, 1)), "standing on the Strt's polygon 1");
    }
}

/// His render angles (the mover's `+0xb4..+0xb8`, slot 6 @ 0x412180)
/// against the frame-based orientation the port drew him by before.
#[cfg(test)]
mod render_angle_tests {
    use super::tests::{data, world};
    use super::*;
    use crate::walker::angle_tests::{angle_axes, degrees};

    /// The old way (`Creature::sync` before slot 6 was ported): forward =
    /// the heading in his polygon's current frame, squared to his up.
    fn old_axes(c: &Creature, sim: &Sim) -> [V3; 3] {
        let f = c.mover.frame(&c.polys, sim);
        let (fwd, _) = c.mover.axes(&f);
        let up = normalize(c.mover.up);
        let fwd = normalize(sub(fwd, scale(up, dot(fwd, up))));
        [fwd, up, cross(up, fwd)]
    }

    #[derive(Debug, Default)]
    struct Worst {
        flat: (f32, u32),
        tilted: (f32, u32),
        turned: (f32, u32),
        flying: (f32, u32),
    }

    impl Worst {
        fn see(&mut self, c: &Creature, sim: &Sim) {
            if c.mode != Mode::Alive {
                return;
            }
            let e = degrees(old_axes(c, sim), angle_axes(c.mover.angles));
            let turned = c.surface().is_some_and(|(o, _)| sim.pose(o).1 != [0; 3]);
            let slot = if c.ground().is_none() {
                &mut self.flying
            } else if turned {
                &mut self.turned
            } else if c.up[2] > 0.999 {
                &mut self.flat
            } else {
                &mut self.tilted
            };
            slot.0 = slot.0.max(e);
            slot.1 += 1;
        }
    }

    /// The intro (born on the disc, the capsule's ride and rolls, the drop
    /// onto pad #83) and his first steps: walking and turning on unturned
    /// floors, the angles are the old frame to within rounding; on the
    /// turned capsule his up is the same and only the roll differs, by the
    /// exe's `0x380` probe (walker.rs); in flight the exe freezes his
    /// pitch and yaw and only spins the roll.
    #[test]
    fn intro_render_angles_follow_the_exe() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        let mut w = Worst::default();
        let mut turned_up = 0.0f32;
        let mut headings = std::collections::BTreeSet::new();
        for _ in 0..150 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            w.see(c, &sim);
            if c.mode == Mode::Alive && c.ground().is_some_and(|(o, _)| sim.pose(o).1 != [0; 3]) {
                let up = |a: [V3; 3]| a[1];
                turned_up = turned_up.max(dot(up(old_axes(c, &sim)), up(angle_axes(c.mover.angles))).clamp(-1.0, 1.0).acos().to_degrees());
            }
            headings.insert(c.heading() / 64);
        }
        println!("intro, worst disagreement (degrees, ticks): {w:?}; on turned floors his up is off by {turned_up:.2}°");
        assert!(headings.len() >= 8, "he turns through the compass ({} sectors)", headings.len());
        assert!(w.flat.1 > 500 && w.flat.0 < 0.5, "walking upright: {:?}", w.flat);
        assert!(w.tilted.0 < 0.5, "on the disc stack's and the pad's slopes: {:?}", w.tilted);
        assert!(w.turned.1 > 1000 && w.turned.0 < 10.5, "riding the capsule: {:?}", w.turned);
        assert!(turned_up < 0.6, "his up on the capsule: {turned_up}°");
        // He lets go of the capsule (turned): what he falls with is the
        // capsule's last slot-6 roll, so the same kind of difference.
        assert!(w.flying.1 > 0 && w.flying.0 < 10.5, "falling onto the pad: {:?}", w.flying);
    }

    /// Section 1's sideways pads (up = +y, #49 ..) and section 2's start
    /// upside down on #194: unturned objects, so the angles are the frame.
    #[test]
    fn sideways_and_upside_down_render_angles() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 1);
        sim.wake_all = true;
        super::walker_tests::until_alive(&mut sim, &mut mesh, 30);
        let mut w = Worst::default();
        let mut sideways = 0;
        for (i, entry, _, _) in super::walker_tests::tilted_teleports(&d, 1).into_iter().take(6) {
            let c = sim.creature.as_mut().unwrap();
            c.drop_at([0.0, 0.0, 1.0e6], 0);
            c.mode = Mode::Alive;
            sim.debug_trigger(i, entry, &mut mesh);
            for _ in 0..4 * sim::TICKS_PER_SECOND {
                sim.step(&mut mesh);
                let c = sim.creature.as_ref().unwrap();
                if c.surface().is_some_and(|(o, _)| o == i) && c.ground().is_some() {
                    sideways += 1;
                    w.see(c, &sim);
                }
            }
        }
        println!("section 1 pads: {w:?}");
        assert!(sideways > 100 && w.tilted.1 > 100, "he stood on the pads: {sideways}");
        assert!(w.tilted.0 < 0.5, "sideways: {:?}", w.tilted);

        let (mut sim, mut mesh) = world(&d, 2);
        super::walker_tests::until_alive(&mut sim, &mut mesh, 30);
        let mut w = Worst::default();
        let mut under = 0;
        for _ in 0..2 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            under += (c.ground().is_some() && c.up[2] < -0.999) as u32;
            w.see(c, &sim);
        }
        println!("section 2 start: {w:?}");
        assert!(under > 20, "upside down under #194 ({under} ticks)");
        assert!(w.tilted.0 < 0.5, "upside down: {:?}", w.tilted);
    }
}

/// Vertex morphs (`ShapeAnim`, slot 0x08 `0x423320`, step `0x4235d0`) in
/// the birth room: the red flame curtains #70 around the start disc #57.
#[cfg(test)]
mod shape_tests {
    use super::tests::{data, world};
    use super::*;

    /// Height of drawn object `id`'s vertices (object-local z range) and
    /// whether it is drawn.
    fn drawn(mesh: &WorldMesh, id: usize) -> (f32, bool) {
        let d = mesh.draw_of_object[id - 1].unwrap();
        let r = mesh.draws[d];
        let z = (r.first..r.first + r.count).map(|i| mesh.vertices[i as usize * crate::world::FLOATS_PER_VERTEX + 2]);
        let (lo, hi) = z.fold((f32::MAX, f32::MIN), |(a, b), z| (a.min(z), b.max(z)));
        (hi - lo, mesh.visible[d] == 1)
    }

    /// #70 starts flat (shape 1, ±93) and hidden (appearance 3, a -5
    /// frame). When #57 goes to state 2 (Mendel is born) it waits; 60
    /// ticks later shape 2 (32 frames) starts raising the curtains, one
    /// frame a tick, and appearance 1 shows them red (texture 37); at 90
    /// ticks shape 3 holds them at full height (−93..2960), which is what
    /// the footage shows around him at 0:20. Before shape morphs were
    /// ported the curtains stayed flat: the low red wedges on the pad's
    /// sides.
    #[test]
    fn flame_curtains_rise_around_the_start_disc() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        assert_eq!(drawn(&mesh, 70), (185.0, false), "flat and hidden at load");
        let mut rising = Vec::new();
        let mut born = None;
        for t in 0..40 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            if born.is_none() && sim.state_of(57) == 2 {
                born = Some(t);
            }
            if sim.state_of(70) == 1 {
                rising.push(drawn(&mesh, 70));
            }
        }
        assert!(born.is_some(), "the start disc fires");
        assert_eq!(sim.state_of(70), 2, "curtains up");
        let (h, shown) = drawn(&mesh, 70);
        assert!(shown && h == 2960.0 + 93.0, "full height and drawn: {h}");
        // Rising: the 32-frame state plays one frame a tick from tick 60.
        let heights: Vec<f32> = rising.iter().filter(|r| r.1).map(|r| r.0).collect();
        assert!(heights.len() >= 25, "drawn while rising for most of 30 ticks: {}", heights.len());
        assert!(heights.windows(2).all(|w| w[1] >= w[0]) && heights.last() > heights.first(), "rising: {heights:?}");
        // Mendel's casts see the morphed polygons too.
        let c = sim.creature.as_ref().unwrap();
        let top = c.polys[70 - 1].iter().flat_map(|p| p.points.iter()).map(|p| p[2]).fold(f32::MIN, f32::max);
        assert_eq!(top, 2960.0, "his polygons of #70 follow its shape frame");
    }
}

#[cfg(test)]
mod blend_tests {
    use super::tests::{data, world};
    use super::*;

    /// A column-major 4×4 (WebGL) applied to a point, and its inverse
    /// for a rotation plus translation.
    fn to_world(m: &[f32], p: V3) -> V3 {
        core::array::from_fn(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r])
    }
    fn to_local(m: &[f32], p: V3) -> V3 {
        let d = [p[0] - m[12], p[1] - m[13], p[2] - m[14]];
        core::array::from_fn(|c| m[c * 4] * d[0] + m[c * 4 + 1] * d[1] + m[c * 4 + 2] * d[2])
    }
    fn centroid(v: &[f32]) -> V3 {
        let n = (v.len() / FLOATS_PER_VERTEX) as f32;
        v.chunks_exact(FLOATS_PER_VERTEX).fold([0.0; 3], |a, x| [a[0] + x[0] / n, a[1] + x[1] / n, a[2] + x[2] / n])
    }

    /// The intro's capsule #3 carries Mendel through the sky. Drawn
    /// between ticks (not in the original), the capsule glides: its
    /// in-between frames lie on the way from one tick's pose to the next;
    /// and Mendel, blended with the same `alpha`, stays where he stands
    /// in the capsule's frame (within a unit or two).
    #[test]
    fn the_capsule_glides_between_ticks_and_mendel_rides_it() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        let capsule = 3 - 1;
        let draw = mesh.draw_of_object[capsule].unwrap();
        let mut on = false;
        for _ in 0..60 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            if c.mode == Mode::Alive && c.ground().is_some_and(|g| g.0 == capsule) {
                on = true;
                break;
            }
        }
        assert!(on, "the capsule picks him up");
        // Let the ride get going.
        for _ in 0..10 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
        }
        let (mut out, mut verts) = (Vec::new(), Vec::new());
        let mut worst: f32 = 0.0;
        let mut moved: f32 = 0.0;
        let mut track = Vec::new();
        for _ in 0..48 {
            let m0 = mesh.transforms[draw * 16..draw * 16 + 16].to_vec();
            let v0 = sim.creature.as_ref().unwrap().vertices.clone();
            sim.step(&mut mesh);
            let c = sim.creature.as_ref().unwrap();
            if c.ground().map(|g| g.0) != Some(capsule) || v0.len() != c.vertices.len() {
                continue;
            }
            let m1 = mesh.transforms[draw * 16..draw * 16 + 16].to_vec();
            moved = moved.max(length(sub([m1[12], m1[13], m1[14]], [m0[12], m0[13], m0[14]])));
            // Where he is in the capsule's frame, at both ticks…
            let l0 = to_local(&m0, centroid(&v0));
            let l1 = to_local(&m1, centroid(&c.vertices));
            for a in [0.25f32, 0.5, 0.75] {
                sim.transforms_at(a, &mesh, &mut out);
                c.vertices_at(a, &mut verts);
                let m = &out[draw * 16..draw * 16 + 16];
                let want: V3 = core::array::from_fn(|k| m0[12 + k] + (m1[12 + k] - m0[12 + k]) * a);
                // The capsule's origin is on its way (rotation blended
                // through its angles, translation linearly).
                assert!(length(sub([m[12], m[13], m[14]], want)) < 1.0, "capsule origin at {a}");
                // …and in between: he has not slid on its floor.
                let l = to_local(m, centroid(&verts));
                let lerp: V3 = core::array::from_fn(|k| l0[k] + (l1[k] - l0[k]) * a);
                worst = worst.max(length(sub(l, lerp)));
                track.push((a, to_world(m, [0.0; 3]).map(|v| v as i32)));
            }
        }
        println!("capsule: up to {moved:.0} units a tick; Mendel off his spot in its frame by at most {worst:.2}");
        println!("capsule origin at quarter ticks: {:?}", &track[..12]);
        assert!(moved > 20.0, "the capsule moves ({moved})");
        assert!(worst < 3.0, "he stays glued to the capsule between ticks ({worst})");
    }

    /// A cut (a path restarting elsewhere, a predator parked at 100000, a
    /// teleport) is drawn as it is, never blended across.
    #[test]
    fn cuts_are_not_blended() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        sim.step(&mut mesh);
        let i = 6 - 1; // #6 sways on its path
        let draw = mesh.draw_of_object[i].unwrap();
        let (pos, rot) = sim.pose(i);
        sim.set_pose(i, [pos[0] + 50_000, pos[1], pos[2]], rot, &mut mesh);
        let mut out = Vec::new();
        sim.transforms_at(0.5, &mesh, &mut out);
        assert_eq!(&out[draw * 16..draw * 16 + 16], &mesh.transforms[draw * 16..draw * 16 + 16]);
        // After a cut (`cut_motion`, e.g. a snapshot coming back) nothing blends.
        sim.cut_motion();
        sim.transforms_at(0.5, &mesh, &mut out);
        assert_eq!(out, mesh.transforms);
    }
}

#[cfg(test)]
mod click_tests {
    use super::tests::{data, world};
    use super::*;

    /// A click lasts one tick (`ObjectRegistry_TickAll` @ 0x4284f0 clears
    /// the picked object after the update pass): section 0's lift #159
    /// (state 0 → 1 on `Event(1) ∧ Clicked`, a 156-tick ride) carries
    /// Mendel up exactly once per click, and never starts without one.
    #[test]
    fn one_click_one_ride() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        sim.wake_all = true; // folder #145 sleeps until Mendel is near
        let lift = 159 - 1;
        for _ in 0..4 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
        }
        // Drop him onto the lift's floor and let him land.
        let c = sim.creature.as_ref().unwrap();
        let m = sim::transform(sim.pose(lift).0, sim.pose(lift).1);
        let p = (0..c.polys[lift].len()).find(|&k| sim.frame_flags(lift, k) & FLOOR != 0 && c.polys[lift][k].normal[2] > 0.7).expect("a floor");
        let at = add(sim::apply(&m, c.polys[lift][p].centre), [0.0, 0.0, 300.0]);
        let c = sim.creature.as_mut().unwrap();
        c.drop_at(at, 0);
        c.mode = Mode::Alive;
        for _ in 0..2 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
        }
        assert_eq!(sim.creature.as_ref().unwrap().ground().map(|g| g.0), Some(lift), "standing on the lift");
        assert_eq!(sim.state_of(159), 0);
        // Without a click, nothing moves while he stands there.
        for _ in 0..2 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            assert_eq!(sim.state_of(159), 0, "no ride without a click");
        }
        // One click while he touches it: one ride.
        assert_eq!(sim.creature.as_ref().unwrap().ground().map(|g| g.0), Some(lift), "still on the lift");
        sim.click(159);
        let mut rides = 0;
        let mut was = 0;
        for _ in 0..30 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
            let s = sim.state_of(159);
            rides += (was == 0 && s == 1) as u32;
            was = s;
        }
        assert_eq!(rides, 1, "exactly one ride per click");
    }
}
