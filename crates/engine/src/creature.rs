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
//! mover      ride the platform, turn, walk, slide on walls, fall      0x417f40
//! touch      hull points that hit something → touch inputs, event 1  0x418450
//! senses     two infrared eyes, three rays each                       0x418b00
//! think      three NERM controllers → three slew-limited actuators    0x425dc0
//! motor      actuators → walking speed, turn rate, stance, pose       0x4189c0
//! learn      running costs; every 65,536 cost-ticks the GA steps      0x419470
//! surface    lethal floors, pain, delayed hazards                     0x419550
//! ```
//!
//! Coordinates: world space is the section's (Y points *down*). Mendel's
//! model space, after the load-time axis swap of `BodyPart::transform`, is
//! `x` forward, `y` down, `z` left; his origin floats `height` units above
//! the ground (41 + stance·166/256: exactly where the posed feet end,
//! checked on the CD data), so `world = pos + R(heading) · model`.
//!
//! The original mover walks in the plane of the polygon it stands on
//! (`0x41dc30`, a navmesh-like routine with surface-local coordinates) and
//! was not ported line by line. Ours is a plain ray-cast character
//! controller built from the same ingredients: the ten hull points, gravity
//! 7 units/tick², the stance height and the polygon flag masks. Each
//! approximation is marked `inferred:`.

use crate::sim::save::{Reader, Writer};
use crate::sim::{self, Request, Sim};
use crate::world::{WorldMesh, FLOATS_PER_VERTEX};
use galapagos_formats::creature::{Brain, CreatureBody, Lcg};
use galapagos_formats::worlds::{Object, Section};

type V3 = [f32; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn length(a: V3) -> f32 {
    dot(a, a).sqrt()
}
fn normalize(a: V3) -> V3 {
    let l = length(a);
    if l == 0.0 { a } else { scale(a, 1.0 / l) }
}

/// Mendel's "up" in world space: the world's Y axis points down.
const UP: V3 = [0.0, -1.0, 0.0];
const DOWN: V3 = [0.0, 1.0, 0.0];

/// Gravity, units per tick². Grounded, the mover adds −7 to the vertical
/// speed of the body above its feet; airborne it adds `up · −0x700 >> 10`
/// to a velocity kept in 1/256 units: −7 units/tick² both ways
/// (`CreatureMover_Integrate` @ 0x418060, verified in the listing).
const GRAVITY: i32 = 7;
/// Default gait (`body+0x69c`); afflictions 3 and 7..9 change it.
const GAIT: i32 = 24;
/// A right click is felt for this many ticks (`MEnc_PopTouchSide` @ 0x4261a0).
const TOUCH_TICKS: u8 = 4;
/// Ticks of contact before a "delayed" (0x800000) lethal surface fires.
const DELAY_TICKS: u16 = 80;
/// Pain: added to all three brain costs (`Creature_HandleSurfaceEvents` @ 0x419550).
const PAIN: i64 = 500 << 16;

/// inferred: length of the dematerialise / rematerialise effect (`+0x8ec`).
/// The real effect's duration lives in the effect code, not ported.
const VANISH_TICKS: u16 = 30;
const APPEAR_TICKS: u16 = 12;
/// inferred: how far above his origin the ground probe starts (lets him
/// step onto a platform that rose under him) and how far below his feet
/// it still counts as ground (walking down steps without falling).
const PROBE_UP: f32 = 150.0;
const STEP_DOWN: f32 = 120.0;
/// inferred: a polygon is ground, not wall, when its normal is within
/// 60° of vertical.
const FLOOR_NORMAL: f32 = 0.5;
/// inferred: where the low "ledge" ray of each eye looks for the floor.
const LEDGE_AHEAD: f32 = 700.0;
const LEDGE_DROP: f32 = 250.0;
/// inferred: falling this long means he left the world: respawn.
const MAX_FALL_TICKS: u32 = 20 * sim::TICKS_PER_SECOND;
/// Radius around Mendel inside which polygons are gathered each tick: the
/// longest ray (eye range 2,000 + 206) plus the hull, with margin.
const GATHER_RADIUS: f32 = 3200.0;
/// inferred: he steps off a ledge only if some ground lies within this
/// depth below; an edge over the void behaves like a wall. The original
/// mover walks from polygon to polygon (`0x411f55` attaches a surface,
/// `0x41213e` lets go) and was not ported; this keeps him out of the void
/// while still letting him drop into pits and onto lower floors.
const MAX_DROP: f32 = 40_000.0;

/// The ten hull points (`creature+0xa60`, set in `Creature_ReadMaster`
/// @ 0x417570), stored as `x, z, y` triples. Converted here to model space
/// `(forward, down, left)`; the spec's `y` is *up* (inferred: with `y`
/// down the two front "touch" points would sit under the floor).
const HULL: [[f32; 3]; 10] = {
    const fn p(x: f32, z: f32, y_up: f32) -> [f32; 3] {
        [x, -y_up, z]
    }
    [
        p(400.0, 450.0, 225.0), // 0: front left  → touch input L
        p(-450.0, -450.0, 225.0),
        p(400.0, -450.0, 225.0), // 2: front right → touch input R
        p(-450.0, 450.0, 225.0),
        p(450.0, 0.0, 0.0),
        p(-450.0, 0.0, 0.0),
        p(0.0, 450.0, 0.0),
        p(0.0, -450.0, 0.0),
        p(0.0, 0.0, 450.0),  // head
        p(0.0, 0.0, -450.0), // feet: the ground probe below replaces it
    ]
};

// ——— Polygon flags (docs/re/objects.md §3, docs/re/mendel.md §2.3) ———
const SOLID: u32 = 0x100;
const HAZARD: u32 = 0x4000_0000;
const IR_VISIBLE: u32 = 0xc0;
const LETHAL: u32 = 0xf000;
const DELAYED: u32 = 0x80_0000;
const HURTS: u32 = 0x100_0000;
/// Frame flag bit 5: a save pad ("blue, shimmering floor panel").
const SAVE_PAD: u32 = 0x20;

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

// ——— World geometry for ray casts ——————————————————————————————————————

/// One mesh polygon in its object's local space, with a bounding sphere so
/// the per-tick gather can skip far polygons cheaply.
struct LocalPoly {
    points: Vec<V3>,
    centre: V3,
    radius: f32,
}

/// A world-space triangle gathered for this tick's ray casts.
struct Tri {
    a: V3,
    e1: V3,
    e2: V3,
    normal: V3,
    obj: usize,
    poly: usize,
    flags: u32,
}

#[derive(Clone, Copy, Debug)]
struct Hit {
    dist: f32,
    obj: usize,
    poly: usize,
    flags: u32,
    normal: V3,
}

/// Möller–Trumbore, two-sided: distance along the unit ray `d`.
fn ray_tri(o: V3, d: V3, t: &Tri) -> Option<f32> {
    let p = cross(d, t.e2);
    let det = dot(t.e1, p);
    if det.abs() < 1e-6 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, t.a);
    let u = dot(s, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, t.e1);
    let v = dot(d, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let dist = dot(t.e2, q) * inv;
    (dist >= 0.0).then_some(dist)
}

/// The triangles near Mendel this tick, in world space, with each
/// polygon's *current* appearance-frame flags: a door that opens stops
/// being solid, a platform that moves carries its floor along.
struct Near(Vec<Tri>);

impl Near {
    /// Nearest hit within `len` among polygons whose flags satisfy `accept`.
    ///
    /// Polygons are one-sided: a ray only hits a polygon's front, the side
    /// its normal `(v1 - v0) × (v2 - v0)` points to. Verified on the data:
    /// floors face up, and section 0's spawn tube faces inwards, which is
    /// why Mendel, spawned exactly on its ceiling plane, falls *into* it
    /// (inferred: the original's collision culls back faces the same way).
    fn cast(&self, o: V3, d: V3, len: f32, accept: impl Fn(&Tri) -> bool) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for t in &self.0 {
            if dot(d, t.normal) >= 0.0 || !accept(t) {
                continue;
            }
            if let Some(dist) = ray_tri(o, d, t) {
                if dist <= len && best.is_none_or(|b| dist < b.dist) {
                    best = Some(Hit { dist, obj: t.obj, poly: t.poly, flags: t.flags, normal: t.normal });
                }
            }
        }
        best
    }

    /// Segment from `a` to `b`.
    fn segment(&self, a: V3, b: V3, accept: impl Fn(&Tri) -> bool) -> Option<Hit> {
        let d = sub(b, a);
        let len = length(d);
        if len == 0.0 {
            return None;
        }
        self.cast(a, scale(d, 1.0 / len), len, accept)
    }
}

/// A wall, for a creature whose up is `up`: solid and steeper than 60°.
fn is_wall(up: V3) -> impl Fn(&Tri) -> bool {
    move |t| t.flags & SOLID != 0 && dot(t.normal, up).abs() < FLOOR_NORMAL
}
/// Ground under a creature whose up is `up`. A *new* floor must also face
/// the world's up; the object he already stands on may have turned under
/// him (a rolling capsule) and still holds him. inferred: the original
/// mover sticks to its surface (`+0x24` is the surface's up vector).
fn is_ground(up: V3, riding: Option<usize>) -> impl Fn(&Tri) -> bool {
    move |t| {
        t.flags & SOLID != 0
            && dot(t.normal, up) >= FLOOR_NORMAL
            && (dot(t.normal, UP) >= FLOOR_NORMAL || riding == Some(t.obj))
    }
}
/// What an eye sees (`Creature_CastIrRay` @ 0x4190f0): mask 0x40000100 and
/// an IR-visible (0xc0) or hazard polygon.
fn is_seen(t: &Tri) -> bool {
    t.flags & (SOLID | HAZARD) != 0 && t.flags & (IR_VISIBLE | HAZARD) != 0
}

/// The rotation part of a `sim` transform, and its inverse (transpose).
fn rotate_by(m: &sim::M, v: V3) -> V3 {
    [m[0] * v[0] + m[1] * v[1] + m[2] * v[2], m[3] * v[0] + m[4] * v[1] + m[5] * v[2], m[6] * v[0] + m[7] * v[1] + m[8] * v[2]]
}
fn unrotate_by(m: &sim::M, v: V3) -> V3 {
    [m[0] * v[0] + m[3] * v[1] + m[6] * v[2], m[1] * v[0] + m[4] * v[1] + m[7] * v[2], m[2] * v[0] + m[5] * v[1] + m[8] * v[2]]
}

/// Heading (1024ths) → world direction on a level floor.
fn heading_dir(h: i32) -> V3 {
    let a = h as f32 * core::f32::consts::TAU / 1024.0;
    [a.cos(), 0.0, a.sin()]
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
    /// Respawn reason 3: back to the last save pad / spawn point.
    Respawn,
    /// Reasons 1/2: a teleporter's destination.
    Teleport(V3, i32),
}

/// The surface Mendel stands on, with his position and orientation
/// remembered in that object's local frame, so a moving, turning or
/// rolling platform carries him (inferred: the original keeps him in
/// surface-local coordinates, `mover+0x44/+0x48`).
#[derive(Clone, Copy, Debug)]
struct Contact {
    obj: usize,
    poly: usize,
    local: V3,
    fwd: V3,
    up: V3,
}

pub struct Creature {
    body: CreatureBody,
    pub brain: Brain,
    rng: Lcg,
    polys: Vec<Vec<LocalPoly>>,
    menc: Option<usize>,
    light: V3,
    floor_limit: f32,
    pub mode: Mode,

    // Mover (`creature+0x50`).
    pub pos: V3,
    /// His frame: unit forward and up vectors. On a level floor `up` is
    /// the world's up and `fwd = (cos, 0, sin)(heading)`; on a platform
    /// both follow the platform.
    pub fwd: V3,
    pub up: V3,
    /// Distance and turn of this tick (`creature+0x7d0`, `+0x7d2`).
    forward: i32,
    turn_d: i32,
    /// Body height above the feet, and its vertical speed (grounded).
    height: i32,
    hvel: i32,
    stance_height: i32,
    /// Airborne velocity, units per tick (world space).
    air: V3,
    air_ticks: u32,
    ground: Option<Contact>,
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

    spawn_point: Option<(V3, i32)>,
    /// The respawn point (`Game+0x17efc4..0x17efd4`): where he stood when
    /// the last world snapshot was taken, or the latest spawn request.
    save_point: Option<(V3, i32)>,
    /// `creature+0xaf0`: the surface he touches this tick has frame flag
    /// bit 5 (0x20), i.e. he stands on a save pad (`0x419730`).
    pub on_save_pad: bool,
    /// not in the original: the last spot where he stood on still, harmless
    /// ground, a respawn fallback when the spawn point hangs over nothing
    /// (section 0's birth tube is gone once the capsule has left).
    safe_point: Option<(V3, i32)>,

    /// The posed mesh in world space, `world::FLOATS_PER_VERTEX` per vertex.
    pub vertices: Vec<f32>,
    /// Teaching/debug counters.
    pub stats: Stats,

    /// Sound cue inputs gathered during the tick: flags of every polygon an
    /// eye ray hit, and how long he had been falling if he landed.
    seen_flags: u32,
    landed_after: u32,
    /// Per-cue silence timers (ticks), see [`SOUND_CUES`].
    cue_quiet: [u16; 5],
}

/// Mendel's five sound cues (`Creature_ReadSoundCues` @ 0x417ab0, played by
/// `Creature_HandleSurfaceEvents` @ 0x419550): the 1-based sounds of the
/// creature bank one cue picks from, and its silence after playing as
/// `(min, random extra)` ticks. Order: landing, agitated, saw a hazard,
/// saw a `0x180000`-class surface, strong left/right asymmetry.
const SOUND_CUES: [(&[i16], u16, u16); 5] =
    [(&[5], 5, 0), (&[1, 2], 5, 32), (&[3, 4], 20, 32), (&[6, 7], 20, 32), (&[8, 9, 10, 11], 20, 32)];
/// "Landing" means after more than this many airborne ticks.
const LANDING_AIR_TICKS: u32 = 5;
/// inferred: the threshold of the asymmetry cue is not in the spec; this is
/// "one eye sees something close, the other almost nothing".
const ASYMMETRY: i32 = 192;

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
        let polys = section
            .objects
            .iter()
            .map(|o| match o {
                Object::World(w) => w.mesh.as_ref().map_or_else(Vec::new, |m| {
                    m.polygons
                        .iter()
                        .map(|p| {
                            let points: Vec<V3> =
                                p.iter().filter_map(|&k| m.vertices.get(k as usize)).map(|v| v.map(|c| c as f32)).collect();
                            let n = points.len().max(1) as f32;
                            let centre = scale(points.iter().fold([0.0; 3], |a, &p| add(a, p)), 1.0 / n);
                            let radius = points.iter().map(|&p| length(sub(p, centre))).fold(0.0, f32::max);
                            LocalPoly { points, centre, radius }
                        })
                        .collect()
                }),
                Object::Camera(_) => Vec::new(),
            })
            .collect();
        let menc = section.objects.iter().position(|o| o.kind() == *b"MEnc");
        let l = section.environment.light_direction;
        // Engine's load-time pose: SetPose(128, 1, 128, 0, 0).
        let stance = 128;
        let mut c = Creature {
            body,
            brain,
            rng: Lcg(1), // inferred: the engine-wide LCG's seed at load is not known
            polys,
            menc,
            light: normalize([l[0] as f32, l[1] as f32, l[2] as f32]),
            floor_limit: mesh.bounds[4] + 20_000.0,
            mode: Mode::Absent,
            pos: [0.0; 3],
            fwd: heading_dir(0),
            up: UP,
            forward: 0,
            turn_d: 0,
            height: stance_height(stance),
            hvel: 0,
            stance_height: stance_height(stance),
            air: [0.0; 3],
            air_ticks: 0,
            ground: None,
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
            spawn_point: None,
            save_point: None,
            on_save_pad: false,
            safe_point: None,
            vertices: Vec::new(),
            stats: Stats::default(),
            seen_flags: 0,
            landed_after: 0,
            cue_quiet: [0; 5],
        };
        c.act[0] = Actuator::new(13, stance);
        c
    }

    /// A right click on his left (`true`) or right side: held for 4 ticks.
    pub fn touch(&mut self, left: bool) {
        self.touch = Some((left, TOUCH_TICKS));
    }

    pub fn ground(&self) -> Option<(usize, usize)> {
        self.ground.map(|g| (g.obj, g.poly))
    }

    pub fn outputs(&self) -> [i32; 3] {
        [self.act[0].value, self.act[1].value, self.act[2].value]
    }

    /// His heading in 1024ths of a turn, measured in the world's
    /// horizontal plane (`(cos, 0, sin)` convention of the mover).
    pub fn heading(&self) -> i32 {
        let a = self.fwd[2].atan2(self.fwd[0]) * 1024.0 / core::f32::consts::TAU;
        (a.round() as i32) & 0x3ff
    }

    /// His left: `up × forward` (model `+z`).
    fn left_dir(&self) -> V3 {
        cross(self.up, self.fwd)
    }

    /// Forward turned by `delta` 1024ths about his up axis (positive =
    /// towards his left, as the heading grows on a level floor).
    fn dir(&self, delta: i32) -> V3 {
        let a = delta as f32 * core::f32::consts::TAU / 1024.0;
        add(scale(self.fwd, a.cos()), scale(self.left_dir(), a.sin()))
    }

    /// Model space (forward, down, left) → world, without the translation.
    fn rotate(&self, m: V3) -> V3 {
        add(add(scale(self.fwd, m[0]), scale(self.up, -m[1])), scale(self.left_dir(), m[2]))
    }

    /// Lie flat on a surface with normal `n`, keeping the heading as well
    /// as possible.
    fn align(&mut self, n: V3) {
        let f = sub(self.fwd, scale(n, dot(self.fwd, n)));
        if length(f) > 1e-3 {
            self.up = n;
            self.fwd = normalize(f);
        }
    }

    // ——— the tick ———

    pub fn tick(&mut self, sim: &mut Sim, mesh: &mut WorldMesh) {
        for r in std::mem::take(&mut sim.requests) {
            self.request(r, sim);
        }
        self.on_save_pad = false;
        match self.mode {
            Mode::Absent => return,
            Mode::Appearing(n) => self.mode = if n <= 1 { Mode::Alive } else { Mode::Appearing(n - 1) },
            Mode::Vanishing(n, then) => {
                if n > 1 {
                    self.mode = Mode::Vanishing(n - 1, then);
                } else {
                    self.finish_vanishing(then, sim, mesh);
                }
            }
            Mode::Alive => self.live(sim),
        }
        self.attach_menc(sim, mesh);
        self.build_vertices();
    }

    /// Spawn, teleport and save requests queued by the portals this tick.
    fn request(&mut self, r: Request, sim: &mut Sim) {
        match r {
            Request::Spawn { at, heading } => {
                // inferred: a spawn while he is already alive only moves the
                // respawn point (section 0's arrival pad #83 does that).
                self.spawn_point = Some((at, heading));
                // `Game_SpawnCreature` @ 0x41c590 also overwrites the
                // respawn point, in both of its branches.
                self.save_point = Some((at, heading));
                if self.mode == Mode::Absent {
                    self.place(at, heading);
                    self.mode = Mode::Appearing(APPEAR_TICKS);
                }
            }
            Request::Teleport { at, heading, snap } => {
                // inferred: a request whose destination he is already close
                // to is ignored. Pads repeat their trigger every tick until
                // he touches them; without this he would never land.
                if self.mode == Mode::Alive && length(sub(at, self.pos)) > 1500.0 {
                    self.mode = Mode::Vanishing(VANISH_TICKS, After::Teleport(at, heading));
                    self.stats.teleports += 1;
                    if snap {
                        sim.events.push(sim::HostEvent { kind: sim::EV_CAMERA_SNAP, a: 0, b: 0, c: 0 });
                    }
                }
            }
            Request::Respawn => {
                if self.mode == Mode::Alive {
                    self.mode = Mode::Vanishing(VANISH_TICKS, After::Respawn);
                }
            }
        }
    }

    fn place(&mut self, at: V3, heading: i32) {
        self.pos = at;
        self.fwd = heading_dir(heading);
        self.up = UP;
        self.air = [0.0; 3];
        self.air_ticks = 0;
        self.ground = None;
        self.hvel = 0;
        self.height = self.stance_height;
        self.delay = DELAY_TICKS;
    }

    /// `Game_OnCreatureRespawn` @ 0x41c700 after the effect has run.
    /// Reason 3 first restores the world snapshot (level start, last save
    /// or open), then puts him back on the respawn point and cuts the
    /// camera. The brain is never restored: he keeps what he learnt.
    fn finish_vanishing(&mut self, then: After, sim: &mut Sim, mesh: &mut WorldMesh) {
        match then {
            After::Teleport(at, h) => self.place(at, h),
            After::Respawn => {
                if sim.restore_snapshot(mesh) {
                    sim.events.push(sim::HostEvent { kind: sim::EV_CAMERA_SNAP, a: 0, b: 0, c: 0 });
                }
                let candidates = [self.save_point, self.spawn_point, self.safe_point];
                let mut pick = candidates.iter().flatten().find(|&&(p, _)| self.ground_below(sim, p)).copied();
                pick = pick.or(self.spawn_point);
                if let Some((at, h)) = pick {
                    self.place(at, h);
                }
            }
        }
        self.mode = Mode::Appearing(APPEAR_TICKS);
    }

    fn live(&mut self, sim: &mut Sim) {
        self.stats.ticks_alive += 1;
        let near = self.gather(sim, self.pos, GATHER_RADIUS);
        self.step_mover(&near, sim);
        self.sense(&near);
        self.think();
        self.apply_motor_outputs();
        self.accumulate_cost();
        self.surface_events();
        self.on_save_pad = self.surface & SAVE_PAD != 0 || self.touches_save_pad(sim);
        self.sound_cues(sim);
    }

    /// Plays the cues whose trigger happened this tick, each respecting its
    /// silence timer. The agitated cue needs afflictions, which aren't
    /// ported, so it never fires yet.
    fn sound_cues(&mut self, sim: &mut Sim) {
        for q in &mut self.cue_quiet {
            *q = q.saturating_sub(1);
        }
        let triggered = [
            self.landed_after > LANDING_AIR_TICKS,
            false,
            self.seen_flags & 0x4000_0000 != 0,
            self.seen_flags & 0x18_0000 != 0,
            (self.left - self.right).abs() >= ASYMMETRY,
        ];
        self.landed_after = 0;
        self.seen_flags = 0;
        for (k, &fire) in triggered.iter().enumerate() {
            if !fire || self.cue_quiet[k] > 0 {
                continue;
            }
            let (sounds, quiet, extra) = SOUND_CUES[k];
            // The sound system's own randomness (the CRT rand), not the
            // brain's LCG: choosing a sound must not change his behaviour.
            let pick = sounds[sim.rand() as usize % sounds.len()];
            let jitter = if extra > 0 { (sim.rand() % extra as u32) as u16 } else { 0 };
            self.cue_quiet[k] = quiet + jitter;
            sim.events.push(sim::HostEvent { kind: sim::EV_CREATURE_SOUND, a: pick as i32 - 1, b: 0, c: 0 });
        }
    }

    /// Collects every polygon near Mendel, transformed by its object's
    /// *current* pose. All objects count, including those of sleeping
    /// folders: a folder only stops updates, its floor is still there.
    fn gather(&self, sim: &Sim, centre: V3, radius: f32) -> Near {
        self.gather_masked(sim, centre, radius, SOLID | HAZARD)
    }

    fn gather_masked(&self, sim: &Sim, centre: V3, radius: f32, mask: u32) -> Near {
        let mut out = Vec::new();
        for (i, polys) in self.polys.iter().enumerate() {
            if polys.is_empty() {
                continue;
            }
            let (pos, rot) = sim.pose(i);
            let m = sim::transform(pos, rot);
            for (p, lp) in polys.iter().enumerate() {
                if lp.points.len() < 3 || length(sub(sim::apply(&m, lp.centre), centre)) > radius + lp.radius {
                    continue;
                }
                let flags = sim.frame_flags(i, p);
                if flags & mask == 0 {
                    continue;
                }
                let w: Vec<V3> = lp.points.iter().map(|&v| sim::apply(&m, v)).collect();
                let normal = normalize(cross(sub(w[1], w[0]), sub(w[2], w[0])));
                for k in 1..w.len() - 1 {
                    out.push(Tri { a: w[0], e1: sub(w[k], w[0]), e2: sub(w[k + 1], w[0]), normal, obj: i, poly: p, flags });
                }
            }
        }
        Near(out)
    }

    // ——— mover ———

    /// `CreatureMover_Step` @ 0x417f40 → `_Integrate` @ 0x418060 →
    /// `_CollideAndTouch` @ 0x418450.
    fn step_mover(&mut self, near: &Near, sim: &mut Sim) {
        self.left = 0;
        self.right = 0;
        self.surface = 0;
        let mut still = true;
        if let Some(g) = self.ground {
            let before = self.pos;
            self.ride(g, sim);
            still = length(sub(self.pos, before)) < 1.0;
        }
        if self.ground.is_some() {
            self.walk(near, sim);
        } else {
            self.fall(near, sim);
        }
        // Touch: the hull, at the final position.
        let hits = self.hull_hits(near, self.pos);
        for (k, h) in hits.iter().enumerate() {
            let Some(h) = h else { continue };
            match k {
                0 => self.left = 256,
                2 => self.right = 256,
                _ => {}
            }
            self.surface |= h.flags;
            self.post(sim, h.obj);
        }
        // inferred: his feet touch what lies under the four hull corners
        // too (his legs span ±450), not only what is under his centre.
        if self.ground.is_some() {
            let down = scale(self.up, -1.0);
            let reach = self.height as f32 + STEP_DOWN;
            let mut felt: Vec<usize> = Vec::new();
            for &[x, _, z] in &HULL[..4] {
                let at = add(self.pos, self.rotate([x, 0.0, z]));
                if let Some(h) = near.cast(at, down, reach, is_ground(self.up, self.riding())) {
                    if Some(h.obj) != self.riding() && !felt.contains(&h.obj) {
                        felt.push(h.obj);
                        self.post(sim, h.obj);
                    }
                }
            }
        }
        if let Some(g) = self.ground {
            let flags = sim.frame_flags(g.obj, g.poly);
            self.surface |= flags;
            self.post(sim, g.obj);
            if still && flags & LETHAL == 0 && dot(self.up, UP) > 0.9 {
                self.safe_point = Some((add(self.pos, scale(UP, 200.0)), self.heading()));
            }
        }
        // The right click (`MEnc`): felt exactly like a bump on that side.
        if let Some((left, n)) = self.touch {
            if left { self.left = 256 } else { self.right = 256 }
            self.touch = (n > 1).then_some((left, n - 1));
        }
    }

    /// Event bit 1, "Mendel touches me", to the object and its ancestors
    /// (`0x4185e0` → `WorldObject_PostEvent` @ 0x4232f0).
    fn post(&mut self, sim: &mut Sim, obj: usize) {
        sim.post_event(obj, 1);
        self.stats.touches_posted += 1;
    }

    /// Moving platforms: re-express his position and frame from the
    /// platform's local frame with its new pose.
    fn ride(&mut self, g: Contact, sim: &Sim) {
        let (pos, rot) = sim.pose(g.obj);
        let m = sim::transform(pos, rot);
        self.pos = sim::apply(&m, g.local);
        self.fwd = normalize(rotate_by(&m, g.fwd));
        self.up = normalize(rotate_by(&m, g.up));
    }

    fn contact(&self, sim: &Sim, h: &Hit) -> Contact {
        let (pos, rot) = sim.pose(h.obj);
        let m = sim::transform(pos, rot);
        Contact {
            obj: h.obj,
            poly: h.poly,
            local: sim::unapply(&m, self.pos),
            fwd: unrotate_by(&m, self.fwd),
            up: unrotate_by(&m, self.up),
        }
    }

    fn riding(&self) -> Option<usize> {
        self.ground.map(|g| g.obj)
    }

    /// The hull: segments from his origin to each of the eight side points
    /// (rotated by the heading). Floor-like polygons are left to the ground
    /// probe, so slopes and steps don't stop him.
    fn hull_hits(&self, near: &Near, at: V3) -> [Option<Hit>; 8] {
        core::array::from_fn(|k| near.segment(at, add(at, self.rotate(HULL[k])), is_wall(self.up)))
    }

    /// How free a position is: the smallest hit fraction along the hull
    /// segments (1 = nothing touched), and the nearest hit.
    fn clearance(&self, near: &Near, at: V3) -> (f32, Option<Hit>) {
        let mut best = (1.0, None);
        for (k, h) in self.hull_hits(near, at).into_iter().enumerate() {
            if let Some(h) = h {
                let f = h.dist / length(HULL[k]);
                if f < best.0 {
                    best = (f, Some(h));
                }
            }
        }
        best
    }

    /// Grounded integration: turn, walk, slide along walls, follow the
    /// ground, or step off into the air.
    fn walk(&mut self, near: &Near, sim: &Sim) {
        self.fwd = self.dir(self.turn_d);
        let d = scale(self.fwd, self.forward as f32);
        // The body bobs above its feet: vertical speed, gravity, clamp.
        self.hvel -= GRAVITY;
        self.height += self.hvel;
        if self.height <= self.stance_height {
            self.height = self.stance_height;
            self.hvel = 0;
        }
        let next = self.slide(near, d);
        self.stats.walked += length(sub(next, self.pos));
        if self.stand(near, sim, next) {
            return;
        }
        if !self.ground_below(sim, next) {
            // An edge over nothing: stay on this side of it.
            let here = self.pos;
            if !self.stand(near, sim, here) {
                self.ground = None;
            }
            return;
        }
        // Walked off a ledge: keep the momentum, start falling.
        self.pos = next;
        self.air = add(d, scale(self.up, self.hvel as f32));
        self.air_ticks = 0;
        self.ground = None;
        self.level_out();
    }

    /// Ground probe along his down axis below `at`; on success he stands
    /// there, lying flat on the polygon.
    fn stand(&mut self, near: &Near, sim: &Sim, at: V3) -> bool {
        let from = add(at, scale(self.up, PROBE_UP));
        let reach = PROBE_UP + self.height as f32 + STEP_DOWN;
        let down = scale(self.up, -1.0);
        let Some(h) = near.cast(from, down, reach, is_ground(self.up, self.riding())) else { return false };
        self.align(h.normal);
        let foot = add(from, scale(down, h.dist));
        self.pos = add(foot, scale(self.up, self.height as f32));
        self.ground = Some(self.contact(sim, &h));
        true
    }

    /// Airborne, he turns back to the world's up (inferred).
    fn level_out(&mut self) {
        let mut f = [self.fwd[0], 0.0, self.fwd[2]];
        if length(f) < 1e-3 {
            f = [-self.up[0], 0.0, -self.up[2]];
        }
        self.fwd = if length(f) < 1e-3 { heading_dir(0) } else { normalize(f) };
        self.up = UP;
    }

    fn ground_below(&self, sim: &Sim, at: V3) -> bool {
        let deep = self.gather(sim, add(at, scale(DOWN, MAX_DROP / 2.0)), MAX_DROP / 2.0);
        deep.cast(at, DOWN, MAX_DROP, is_ground(UP, None)).is_some()
    }

    /// Accept a move unless it brings the hull closer to a wall; otherwise
    /// try sliding along that wall; otherwise stay put. Comparing with the
    /// current clearance (rather than demanding none) lets him back out of
    /// a wall that moved into him.
    fn slide(&self, near: &Near, d: V3) -> V3 {
        if length(d) == 0.0 {
            return self.pos;
        }
        let (now, _) = self.clearance(near, self.pos);
        let ok = |delta: V3| {
            let at = add(self.pos, delta);
            (self.clearance(near, at).0 >= now.min(1.0)).then_some(at)
        };
        if let Some(at) = ok(d) {
            return at;
        }
        if let (_, Some(h)) = self.clearance(near, add(self.pos, d)) {
            let n = normalize(sub(h.normal, scale(self.up, dot(h.normal, self.up))));
            if let Some(at) = ok(sub(d, scale(n, dot(d, n)))) {
                return at;
            }
        }
        self.pos
    }

    /// Airborne: ballistic flight (`0x41814e`), landing on the first ground
    /// polygon under the path. Walls stop the horizontal motion.
    fn fall(&mut self, near: &Near, sim: &Sim) {
        self.air = add(self.air, scale(DOWN, GRAVITY as f32));
        self.air_ticks += 1;
        let mut next = add(self.pos, self.air);
        if self.clearance(near, [next[0], self.pos[1], next[2]]).0 < 1.0 {
            next = [self.pos[0], next[1], self.pos[2]];
            self.air = [0.0, self.air[1], 0.0];
        }
        // Probe from where he was (height-wise) to below his feet at `next`.
        let from = [next[0], self.pos[1] - PROBE_UP, next[2]];
        let reach = (next[1] - from[1]).max(0.0) + self.stance_height as f32;
        if self.air[1] > 0.0 {
            if let Some(h) = near.cast(from, DOWN, reach, is_ground(UP, None)) {
                let foot = add(from, scale(DOWN, h.dist));
                self.height = self.stance_height;
                self.hvel = 0;
                self.align(h.normal);
                self.pos = add(foot, scale(self.up, self.height as f32));
                self.ground = Some(self.contact(sim, &h));
                self.landed_after = self.air_ticks;
                self.air_ticks = 0;
                return;
            }
        }
        self.pos = next;
    }

    // ——— senses ———

    /// `Creature_SenseInfrared` @ 0x418b00. Two eyes at heading ± `a`, each
    /// casting three rays; an eye already set by touch stays as it is.
    fn sense(&mut self, near: &Near) {
        let posture = self.act[0].value;
        let a = 100 - posture * 50 / 256;
        let range = 1500 + posture * 500 / 256;
        if self.left == 0 {
            self.left = self.eye(near, a, range);
        }
        if self.right == 0 {
            self.right = self.eye(near, -a, range);
        }
    }

    fn eye(&mut self, near: &Near, angle: i32, range: i32) -> i32 {
        let dir = self.dir(angle);
        let value = |dist: f32| (256 - (dist as i32 - 656) * 256 / (range - 450)).clamp(0, 256);
        let reach = (range + 206) as f32;
        let mut best = 0;
        // inferred: the high and middle rays start at the top of the shell
        // and at his origin (the stacking offsets are not in the spec).
        for lift in [150.0, 0.0] {
            if let Some(h) = near.cast(add(self.pos, scale(self.up, lift)), dir, reach, is_seen) {
                best = best.max(value(h.dist));
                self.seen_flags |= h.flags;
            }
        }
        // The low ray (`Creature_CastIrLedgeRay` @ 0x4191f0) reports drops.
        // inferred: we look for the floor just ahead along the eye and, if
        // it falls away by more than 250, see the ledge as an obstacle there.
        if self.ground.is_some() {
            let ahead = add(self.pos, scale(dir, LEDGE_AHEAD));
            let feet = self.height as f32;
            let down = scale(self.up, -1.0);
            if near.cast(ahead, down, feet + LEDGE_DROP, is_ground(self.up, self.riding())).is_none() {
                best = best.max(value(LEDGE_AHEAD));
            }
        }
        best
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
        // (n38, n3c) = (1, 2) without afflictions. Turning only happens
        // while walking: its actuator's time step is the walking speed.
        self.act[0].step(0, 256);
        for _ in 0..2 {
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
        let (turn, speed, gait) = (self.turn as i64, speed as i64, GAIT as i64);
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
        if self.ground.is_some() {
            self.hvel = 2 * (h - self.stance_height);
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

    /// `Creature_HandleSurfaceEvents` @ 0x419550 (sounds and afflictions
    /// not ported).
    fn surface_events(&mut self) {
        let f = self.surface;
        let fell_out = self.air_ticks > MAX_FALL_TICKS || self.pos[1] > self.floor_limit;
        if f & LETHAL == 0 && !fell_out {
            if self.ground.is_some() {
                self.delay = DELAY_TICKS;
            }
            return;
        }
        if f & DELAYED != 0 && !fell_out {
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
        self.stats.deaths += 1;
        self.mode = Mode::Vanishing(VANISH_TICKS, After::Respawn);
    }

    // ——— MEnc and rendering ———

    /// `Creature_AttachTouchHull` @ 0x417d60: the section's `MEnc` (an
    /// invisible click hull) follows Mendel. Ours is hidden: we pick
    /// against Mendel's own mesh instead (see [`Creature::pick`]).
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
        let rot = [0, -self.heading(), 0];
        sim.set_pose(i, self.pos.map(|c| c as i32), rot, mesh);
    }

    /// The posed body in world space: shell plus both leg sides, real UVs,
    /// texture layers 16..19, flat Lambert light like the world's.
    fn build_vertices(&mut self) {
        self.vertices.clear();
        let visible = match self.mode {
            Mode::Absent => false,
            // A crude stand-in for the materialise effect: blink.
            Mode::Appearing(n) | Mode::Vanishing(n, _) => n % 4 < 2,
            Mode::Alive => true,
        };
        if !visible {
            return;
        }
        let mesh = self.body.pose_turning((self.phase >> 8) as u8, self.stance, self.turn, self.side);
        let world: Vec<V3> = mesh.vertices.iter().map(|v| add(self.pos, self.rotate(v.map(|c| c as f32)))).collect();
        for f in &mesh.faces {
            if f.indices.len() < 3 {
                continue;
            }
            let p = |k: usize| world[f.indices[k] as usize];
            let n = normalize(cross(sub(p(1), p(0)), sub(p(2), p(0))));
            let light = 0.7 + 0.3 * dot(n, self.light).abs();
            for k in 1..f.indices.len() - 1 {
                for j in [0, k, k + 1] {
                    let v = p(j);
                    let [u, t] = f.uv[j];
                    self.vertices.extend_from_slice(&[v[0], v[1], v[2], u / 128.0, t / 128.0, f.texture as f32, light]);
                }
            }
        }
    }

    /// For the host's chase camera (not in the original, whose camera
    /// collides through `0x413d43` with the same mask): the clear fraction
    /// (0..=1) of the segment from `from` to `eye`, against polygons whose
    /// flags have 0x102, seen from either side.
    pub fn camera_clearance(&self, sim: &Sim, from: V3, eye: V3) -> f32 {
        let d = sub(eye, from);
        let len = length(d);
        if len < 1.0 {
            return 1.0;
        }
        let dir = scale(d, 1.0 / len);
        let near = self.gather_masked(sim, from, len, 0x102);
        let mut best = len;
        for t in &near.0 {
            if let Some(h) = ray_tri(from, dir, t) {
                best = best.min(h);
            }
        }
        best / len
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

/// Everything about Mendel a saved game needs besides his brain: the
/// mover, the agent (actuators, running costs), the respawn points and the
/// counters. Parsed completely before it is applied, so a damaged file
/// leaves the running game untouched.
#[derive(Clone, Debug)]
pub struct SavedCreature {
    mode: Mode,
    pos: V3,
    fwd: V3,
    up: V3,
    ints: [i32; 5], // forward, turn_d, height, hvel, stance_height
    air: V3,
    air_ticks: u32,
    ground: Option<Contact>,
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
    points: [Option<(V3, i32)>; 3], // spawn, respawn (save), safe
    stats: Stats,
    rng: u32,
    cue_quiet: [u16; 5],
}

impl Creature {
    pub(crate) fn saved(&self) -> SavedCreature {
        SavedCreature {
            mode: self.mode,
            pos: self.pos,
            fwd: self.fwd,
            up: self.up,
            ints: [self.forward, self.turn_d, self.height, self.hvel, self.stance_height],
            air: self.air,
            air_ticks: self.air_ticks,
            ground: self.ground,
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
            points: [self.spawn_point, self.save_point, self.safe_point],
            stats: self.stats,
            rng: self.rng.0,
            cue_quiet: self.cue_quiet,
        }
    }

    pub(crate) fn apply_saved(&mut self, s: SavedCreature) {
        self.mode = s.mode;
        self.pos = s.pos;
        self.fwd = s.fwd;
        self.up = s.up;
        [self.forward, self.turn_d, self.height, self.hvel, self.stance_height] = s.ints;
        self.air = s.air;
        self.air_ticks = s.air_ticks;
        self.ground = s.ground.filter(|g| g.obj < self.polys.len());
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
        [self.spawn_point, self.save_point, self.safe_point] = s.points;
        self.stats = s.stats;
        self.rng = Lcg(s.rng);
        self.cue_quiet = s.cue_quiet;
        self.build_vertices();
    }

    /// inferred: the exe only learns about save pads through its hull probe
    /// (mask 0x100, `0x418595`), yet every save-pad polygon on the CD is
    /// flagged 0x22, never solid. So we also test, both ways, whether his
    /// hull segments or a vertical segment through his body cross a 0x20
    /// polygon: "Mendel is walking on it" as the readme says.
    fn touches_save_pad(&self, sim: &Sim) -> bool {
        let near = self.gather_masked(sim, self.pos, 1200.0, SAVE_PAD);
        if near.0.is_empty() {
            return false;
        }
        let feet = self.height as f32 + STEP_DOWN;
        let mut segments: Vec<(V3, V3)> = HULL[..8].iter().map(|&h| (self.pos, add(self.pos, self.rotate(h)))).collect();
        segments.push((add(self.pos, scale(self.up, 450.0)), add(self.pos, scale(self.up, -feet))));
        segments.iter().any(|&(a, b)| {
            let d = sub(b, a);
            let len = length(d);
            let dir = scale(d, 1.0 / len.max(1e-3));
            near.0.iter().any(|t| ray_tri(a, dir, t).is_some_and(|h| h <= len))
        })
    }

    /// Part of `Game_TakeSnapshot` @ 0x41d170: his current spot becomes the
    /// respawn point (`0x4124a0`/`0x4124f0` read the mover's surface and
    /// surface-local position; the world is restored to this very instant,
    /// so the world position is equivalent).
    pub(crate) fn anchor_respawn(&mut self) {
        if self.mode == Mode::Alive {
            self.save_point = Some((self.pos, self.heading()));
        }
    }
}

impl SavedCreature {
    pub(crate) fn write(&self, w: &mut Writer) {
        let v3 = |w: &mut Writer, v: V3| v.iter().for_each(|&c| w.f32(c));
        let (kind, n, after) = match self.mode {
            Mode::Absent => (0, 0, None),
            Mode::Appearing(n) => (1, n, None),
            Mode::Alive => (2, 0, None),
            Mode::Vanishing(n, a) => (3, n, Some(a)),
        };
        w.u8(kind);
        w.u16(n);
        match after {
            Some(After::Teleport(at, h)) => {
                w.u8(1);
                v3(w, at);
                w.i32(h);
            }
            _ => {
                w.u8(0);
                v3(w, [0.0; 3]);
                w.i32(0);
            }
        }
        for v in [self.pos, self.fwd, self.up] {
            v3(w, v);
        }
        self.ints.iter().for_each(|&i| w.i32(i));
        v3(w, self.air);
        w.u32(self.air_ticks);
        let g = self.ground;
        w.u8(g.is_some() as u8);
        let g = g.unwrap_or(Contact { obj: 0, poly: 0, local: [0.0; 3], fwd: [0.0; 3], up: [0.0; 3] });
        w.u32(g.obj as u32);
        w.u32(g.poly as u32);
        for v in [g.local, g.fwd, g.up] {
            v3(w, v);
        }
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
        for p in self.points {
            w.u8(p.is_some() as u8);
            let (at, h) = p.unwrap_or(([0.0; 3], 0));
            v3(w, at);
            w.i32(h);
        }
        let s = self.stats;
        w.u32(s.ticks_alive);
        w.f32(s.walked);
        [s.touches_posted, s.deaths, s.teleports, s.evolutions].iter().for_each(|&i| w.u32(i));
        w.u32(self.rng);
        self.cue_quiet.iter().for_each(|&q| w.u16(q));
    }

    pub(crate) fn read(r: &mut Reader) -> Result<Self, &'static str> {
        fn v3(r: &mut Reader) -> Result<V3, &'static str> {
            Ok([r.f32()?, r.f32()?, r.f32()?])
        }
        let kind = r.u8()?;
        let n = r.u16()?;
        let teleport = r.u8()? != 0;
        let (at, h) = (v3(r)?, r.i32()?);
        let after = if teleport { After::Teleport(at, h) } else { After::Respawn };
        let mode = match kind {
            0 => Mode::Absent,
            1 => Mode::Appearing(n.max(1)),
            2 => Mode::Alive,
            3 => Mode::Vanishing(n.max(1), after),
            _ => return Err("bad creature mode"),
        };
        let (pos, fwd, up) = (v3(r)?, v3(r)?, v3(r)?);
        let mut ints = [0; 5];
        for i in &mut ints {
            *i = r.i32()?;
        }
        let air = v3(r)?;
        let air_ticks = r.u32()?;
        let grounded = r.u8()? != 0;
        let contact = Contact { obj: r.u32()? as usize, poly: r.u32()? as usize, local: v3(r)?, fwd: v3(r)?, up: v3(r)? };
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
        let mut points = [None; 3];
        for p in &mut points {
            let has = r.u8()? != 0;
            let (at, h) = (v3(r)?, r.i32()?);
            *p = has.then_some((at, h));
        }
        let ticks_alive = r.u32()?;
        let walked = r.f32()?;
        let (touches_posted, deaths, teleports, evolutions) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        let stats = Stats { ticks_alive, walked, touches_posted, deaths, teleports, evolutions };
        let rng = r.u32()?;
        let mut cue_quiet = [0; 5];
        for q in &mut cue_quiet {
            *q = r.u16()?;
        }
        Ok(SavedCreature {
            mode,
            pos,
            fwd,
            up,
            ints,
            air,
            air_ticks,
            ground: grounded.then_some(contact),
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
            points,
            stats,
            rng,
            cue_quiet,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use galapagos_formats::creature::overlay_for_age;
    use galapagos_formats::worlds::Archive;

    fn data() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds")).ok()
    }

    /// Section `s` with Mendel loaded (not spawned) and the level-3 brain.
    fn world(d: &[u8], s: usize) -> (Sim, WorldMesh) {
        let a = Archive::parse(d).unwrap();
        let section = a.world(s).unwrap();
        let mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mesh);
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

    /// The real introduction: the `Strt` portal spawns Mendel in a tube
    /// high above section 0 after ~14 s; he must land inside it (the tube
    /// faces inwards), walk, and not fall out of the world.
    #[test]
    fn mendel_is_born_in_section0() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        run(&mut sim, &mut mesh, 60, None);
        let c = sim.creature.as_ref().unwrap();
        println!("intro: {:?} at {:?}, walked {}, {:?}", c.mode, c.pos.map(|v| v as i32), c.stats.walked as i32, c.stats);
        assert!(c.stats.ticks_alive > 30 * sim::TICKS_PER_SECOND, "spawned by the Strt portal");
        assert_eq!(c.stats.deaths, 0, "never fell out of the world");
        // By now the capsule (#3) has docked around the tube and is flying
        // him towards the main area: he must be standing on one of the two.
        let (obj, _) = c.ground().expect("standing on something");
        assert!(obj + 1 == 54 || obj + 1 == 3, "on the tube or in the capsule, not on #{}", obj + 1);
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
        println!("skip: {:?} at {:?} on {:?}", c.mode, c.pos.map(|v| v as i32), c.ground());
        assert!(c.stats.ticks_alive > 8 * sim::TICKS_PER_SECOND, "spawned by the arrival pad");
        assert!(matches!(c.ground(), Some((o, _)) if o + 1 == 92 || o + 1 == 83), "standing on the pad");
        assert!(!sim.visible()[160 - 1], "the upper level sleeps");

        sim.creature.as_mut().unwrap().place([-1110.0, -19100.0, 3959.0], 0); // above platform #77
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
        assert!(c.pos[1].abs() < 40_000.0, "inside the level, y = {}", c.pos[1]);
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

    #[test]
    fn ray_hits_a_triangle() {
        let t = Tri { a: [0.0; 3], e1: [10.0, 0.0, 0.0], e2: [0.0, 0.0, 10.0], normal: DOWN, obj: 0, poly: 0, flags: SOLID };
        assert_eq!(ray_tri([1.0, -5.0, 1.0], DOWN, &t), Some(5.0));
        assert_eq!(ray_tri([11.0, -5.0, 1.0], DOWN, &t), None);
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
                mendel.place(add(c, scale(UP, 400.0)), 0);
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
