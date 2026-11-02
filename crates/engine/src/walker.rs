//! Mendel's mover: the original **polygon walker** (creature `+0x50`,
//! base class vtable 0x45f530, creature vtable 0x45fbb0), ported from the
//! exe. docs/re/mendel.md §2.2 is the spec.
//!
//! The whole idea fits in one sentence: *on the ground, Mendel lives in
//! the 2-D coordinates of the polygon he stands on, and every tick a short
//! probe along his own "down" decides which polygon that is.* No navmesh,
//! no adjacency table, no slope limit, no world "up": a floor is any
//! polygon with flag bit 1, whatever way it faces, and his up is its
//! normal.
//!
//! ```text
//! CreatureMover_Step (0x417f40 → base 0x411e10), once a tick:
//!   integrate      grounded: u += cos h·fwd, v += sin h·fwd, h turns;     0x418060
//!                  height above the plane falls back to the stance;
//!                  conveyor floors drag him (0x41dc30); world = frame(u,v,h)  0x412180
//!                  airborne: ballistic along −up, spinning
//!   collide        ten hull segments vs walls (bit 8): touch inputs,      0x418450
//!                  half the penetration pushed out, bounce in flight
//!   find surface   five probes along −up (mask 2): same polygon → stay;   0x411ec0
//!                  another → attach (re-project, turn the heading about
//!                  the shared hinge); none → let go (velocity = last step)
//! ```
//!
//! World geometry here is floating point (the exe uses 16.16-free
//! integers: normals × 0x400, frames × 0x1000); the algorithm and every
//! constant are the exe's.

use crate::sim::{self, Sim};
use galapagos_formats::worlds::{Object, Section};

pub(crate) type V3 = [f32; 3];

pub(crate) fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub(crate) fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub(crate) fn scale(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub(crate) fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub(crate) fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub(crate) fn length(a: V3) -> f32 {
    dot(a, a).sqrt()
}
pub(crate) fn normalize(a: V3) -> V3 {
    let l = length(a);
    if l == 0.0 { a } else { scale(a, 1.0 / l) }
}

/// A 3×3 rotation, row-major.
pub(crate) type M3 = [f32; 9];
pub(crate) const IDENTITY: M3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

pub(crate) fn mul(m: &M3, v: V3) -> V3 {
    [m[0] * v[0] + m[1] * v[1] + m[2] * v[2], m[3] * v[0] + m[4] * v[1] + m[5] * v[2], m[6] * v[0] + m[7] * v[1] + m[8] * v[2]]
}
pub(crate) fn mul_t(m: &M3, v: V3) -> V3 {
    [m[0] * v[0] + m[3] * v[1] + m[6] * v[2], m[1] * v[0] + m[4] * v[1] + m[7] * v[2], m[2] * v[0] + m[5] * v[1] + m[8] * v[2]]
}
/// The world's up, +Z: what `CreatureMover_Init` @ 0x411b60 puts in the
/// contact normal (`+0x24 = (0, 0, 0x400)`, at 0x411bdd) before Mendel
/// ever touches a polygon.
pub(crate) const UP: V3 = [0.0, 0.0, 1.0];

// ——— Polygon flags (docs/re/objects.md §3) ————————————————————————————
/// Bit 1: a floor, what the surface probes look for (`0x4089e0(…, 2)`).
pub(crate) const FLOOR: u32 = 0x2;
/// Bit 8: what the hull collides with (mask 0x100 at 0x418595).
pub(crate) const SOLID: u32 = 0x100;
pub(crate) const HAZARD: u32 = 0x4000_0000;
/// Bits 2..3: a scrolling texture; on a floor, a conveyor (0x41dc30).
const SCROLL: u32 = 0xc;
/// Lethal and affliction bits: while the walls touched this tick carry
/// some, the floor's flags are not added (0x41215d).
const EVENT_BITS: u32 = 0x1e00_f000;
/// Affliction 1 (the shield): a touched polygon carrying it replaces the
/// surface flags instead of joining them (0x418848).
const SHIELD_BITS: u32 = 0x0200_0000;
const AFFLICTION_BITS: u32 = 0x1e00_0000;

// ——— Mover constants (`CreatureMover_Init` @ 0x411b60, `0x418230`) ————
/// The five surface probes: in-plane offsets (`+0x6c..+0x90`), a 14-unit
/// square around his origin, so a probe falling exactly on the edge
/// between two polygons still finds one of them.
const PROBES: [(f32, f32); 5] = [(0.0, 0.0), (7.0, 7.0), (7.0, -7.0), (-7.0, -7.0), (-7.0, 7.0)];
/// A probe starts `up/4` above his origin (`+0x24 >> 2`: up is × 0x400)…
const PROBE_UP: f32 = 256.0;
/// …and ends this far below his feet (`-32 - stance height`).
const PROBE_BELOW_FEET: f32 = 32.0;
/// A hit only counts if the probe's end lies behind the polygon's plane,
/// at most this deep (`0x408c50`: a 0x400-long segment along the normal).
const BEHIND_MAX: f32 = 1024.0;

/// The ten hull points (`creature+0xa60`, set in `Creature_ReadMaster` @
/// 0x417570) as raw `(forward, left, up)` triples in his polygon frame.
pub(crate) const HULL: [V3; 10] = [
    [400.0, 450.0, 225.0], // 0: front left  → touch input L
    [-450.0, -450.0, 225.0],
    [400.0, -450.0, 225.0], // 2: front right → touch input R
    [-450.0, 450.0, 225.0],
    [450.0, 0.0, 0.0],
    [-450.0, 0.0, 0.0],
    [0.0, 450.0, 0.0],
    [0.0, -450.0, 0.0],
    [0.0, 0.0, 450.0],
    [0.0, 0.0, -450.0], // 9: feet (its segment starts up/4 above him)
];

/// Bounce speed (units per tick) when he hits a wall in flight, by the
/// wall's `flags & 0xe00` (0x418791): the normal velocity becomes this.
fn bounce(flags: u32) -> f32 {
    let v = match flags & 0xe00 {
        0x000 => 0,
        0x200 => 0x1000,
        0x400 => 0x6400,
        0x600 => 0xfa00,
        0x800 => 0x15e00,
        0xa00 => 0x1c200,
        _ => 0,
    };
    v as f32 / 256.0
}

/// 1024ths of a turn → radians.
pub(crate) fn turns(a: i32) -> f32 {
    a as f32 * core::f32::consts::TAU / 1024.0
}

// ——— World geometry ————————————————————————————————————————————————

/// One mesh polygon in its object's local space, with what the mover
/// needs: its first vertex (the origin of its frame), unit normal, frame
/// (`Math_FrameFromNormal` @ 0x4312b0: polygon → object), and the texture
/// basis of a scrolling floor (`GameObject_ComputePolyTextureBasis` @
/// 0x41d6d0).
pub(crate) struct LocalPoly {
    pub points: Vec<V3>,
    pub centre: V3,
    pub radius: f32,
    pub normal: V3,
    pub frame: M3,
    /// `∂(u, v)/∂(s, t)`: how far, in the polygon's frame, one texel of
    /// texture offset moves the image. Row-major 2×2.
    pub texel: [f32; 4],
}

/// Every object's polygons, by object index (cameras have none).
pub(crate) fn local_polys(section: &Section) -> Vec<Vec<LocalPoly>> {
    section
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
                        let (normal, frame, texel) = if points.len() >= 3 {
                            let normal = normalize(cross(sub(points[1], points[0]), sub(points[2], points[0])));
                            let frame = sim::frame_from_normal(normal);
                            // Default texture coordinates (0x42320d): vertex 0
                            // = (s 0, t 127), vertex 1 = (0, 0), vertex 2 =
                            // (127, 0); so s runs along v1→v2, t along v1→v0.
                            let ds = mul_t(&frame, scale(sub(points[2], points[1]), 1.0 / 127.0));
                            let dt = mul_t(&frame, scale(sub(points[0], points[1]), 1.0 / 127.0));
                            (normal, frame, [ds[0], dt[0], ds[1], dt[1]])
                        } else {
                            (UP, IDENTITY, [0.0; 4])
                        };
                        LocalPoly { points, centre, radius, normal, frame, texel }
                    })
                    .collect()
            }),
            Object::Camera(_) => Vec::new(),
        })
        .collect()
}

/// A world-space triangle gathered for this tick's casts.
pub(crate) struct Tri {
    pub a: V3,
    pub e1: V3,
    pub e2: V3,
    /// The polygon's unit normal; `a` is its first vertex (a point of its
    /// plane), whichever fan triangle this is.
    pub normal: V3,
    pub obj: usize,
    pub poly: usize,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Hit {
    pub dist: f32,
    pub point: V3,
    pub obj: usize,
    pub poly: usize,
    pub flags: u32,
    pub normal: V3,
    /// A point of the polygon's plane (its first vertex).
    pub plane: V3,
}

/// Möller–Trumbore, two-sided: distance along the unit ray `d`.
pub(crate) fn ray_tri(o: V3, d: V3, t: &Tri) -> Option<f32> {
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

/// Which faces a cast sees.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Faces {
    /// Both: `World_Cast` @ 0x4089e0 itself (`0x407740` accepts a crossing
    /// either way). The mover then rejects back faces with [`behind`].
    Both,
    /// Front faces only (the port's camera and predator probes).
    Front,
}

/// The triangles near Mendel this tick, in world space, with each
/// polygon's current flags (`Sim::frame_flags`): a door that opens stops
/// being solid, a platform that moves carries its floor along.
pub(crate) struct Near(pub Vec<Tri>);

impl Near {
    /// Collects every polygon within `radius` of `centre` whose flags meet
    /// `mask`, transformed by its object's *current* pose. All objects
    /// count, including those of sleeping folders: a folder only stops
    /// updates, its floor is still there.
    pub fn gather(polys: &[Vec<LocalPoly>], sim: &Sim, centre: V3, radius: f32, mask: u32) -> Near {
        let mut out = Vec::new();
        for (i, polys) in polys.iter().enumerate() {
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

    /// Nearest hit within `len` among polygons whose flags satisfy `accept`.
    pub fn cast(&self, o: V3, d: V3, len: f32, faces: Faces, accept: impl Fn(&Tri) -> bool) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for t in &self.0 {
            if (faces == Faces::Front && dot(d, t.normal) >= 0.0) || !accept(t) {
                continue;
            }
            if let Some(dist) = ray_tri(o, d, t) {
                if dist <= len && best.is_none_or(|b| dist < b.dist) {
                    let point = add(o, scale(d, dist));
                    best = Some(Hit { dist, point, obj: t.obj, poly: t.poly, flags: t.flags, normal: t.normal, plane: t.a });
                }
            }
        }
        best
    }

    /// Segment from `a` to `b`.
    pub fn segment(&self, a: V3, b: V3, faces: Faces, accept: impl Fn(&Tri) -> bool) -> Option<Hit> {
        let d = sub(b, a);
        let len = length(d);
        if len == 0.0 {
            return None;
        }
        self.cast(a, scale(d, 1.0 / len), len, faces, accept)
    }

    /// Every polygon the segment crosses (one hit per polygon), nearest
    /// first, at most ten: `World_CastAll` @ 0x408b20.
    pub fn segment_all(&self, a: V3, b: V3, accept: impl Fn(&Tri) -> bool) -> Vec<Hit> {
        let d = sub(b, a);
        let len = length(d);
        if len == 0.0 {
            return Vec::new();
        }
        let d = scale(d, 1.0 / len);
        let mut hits: Vec<Hit> = Vec::new();
        for t in &self.0 {
            if !accept(t) {
                continue;
            }
            if let Some(dist) = ray_tri(a, d, t).filter(|&x| x <= len) {
                if hits.iter().any(|h| h.obj == t.obj && h.poly == t.poly) {
                    continue;
                }
                hits.push(Hit { dist, point: add(a, scale(d, dist)), obj: t.obj, poly: t.poly, flags: t.flags, normal: t.normal, plane: t.a });
            }
        }
        hits.sort_by(|x, y| x.dist.total_cmp(&y.dist));
        hits.truncate(10);
        hits
    }
}

/// `0x408c50`: the mover's test on a hit. From the segment's end `end`,
/// a 0x400-long segment along the polygon's normal must cross its plane:
/// the end lies **behind** the polygon (on its back side), at most 1,024
/// deep. Returns the push from `end` to the plane (`depth · normal`).
/// This is what makes polygons one-sided for Mendel: a probe or a hull
/// segment that crosses a polygon from behind does not count.
pub(crate) fn behind(h: &Hit, end: V3) -> Option<V3> {
    let depth = dot(sub(h.plane, end), h.normal);
    (0.0..=BEHIND_MAX).contains(&depth).then(|| scale(h.normal, depth))
}

/// Where a polygon is this tick: its object's rotation and translation,
/// and the polygon's own frame and first vertex.
#[derive(Clone, Copy)]
pub(crate) struct Frame {
    /// Object rotation (`+0x16a`, local → world) and position (`+0xe4`).
    pub rot: M3,
    pub pos: V3,
    /// The polygon's frame (`+0x28`, polygon → object) and first vertex.
    pub poly: M3,
    pub v0: V3,
    /// Whether the object is turned at all (`0x409ae0`: any angle ≠ 0).
    pub rotated: bool,
}

impl Frame {
    /// The world's own frame, for a Mendel placed without a polygon.
    pub const WORLD: Frame = Frame { rot: IDENTITY, pos: [0.0; 3], poly: IDENTITY, v0: [0.0; 3], rotated: false };

    pub fn of(polys: &[Vec<LocalPoly>], sim: &Sim, on: Option<(usize, usize)>) -> Frame {
        let Some((o, p)) = on else { return Frame::WORLD };
        let Some(lp) = polys.get(o).and_then(|v| v.get(p)).filter(|lp| lp.points.len() >= 3) else { return Frame::WORLD };
        let (pos, rot) = sim.pose(o);
        let m = sim::transform(pos, rot);
        Frame {
            rot: [m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]],
            pos: [m[9], m[10], m[11]],
            poly: lp.frame,
            v0: lp.points[0],
            rotated: rot != [0; 3],
        }
    }

    /// Polygon-frame direction → world (`0x431ad0` with `+0x28`, then
    /// `0x409880`).
    pub fn dir(&self, d: V3) -> V3 {
        mul(&self.rot, mul(&self.poly, d))
    }

    /// World direction → polygon frame (`0x4098e0`, then `+0x3a`).
    pub fn undir(&self, d: V3) -> V3 {
        mul_t(&self.poly, mul_t(&self.rot, d))
    }

    /// Surface point `(u, v, h)` → world (`0x412180`).
    pub fn point(&self, p: V3) -> V3 {
        add(self.pos, mul(&self.rot, add(self.v0, mul(&self.poly, p))))
    }

    /// World point → `(u, v, h)`.
    pub fn unpoint(&self, w: V3) -> V3 {
        mul_t(&self.poly, sub(mul_t(&self.rot, sub(w, self.pos)), self.v0))
    }

    pub fn normal(&self) -> V3 {
        self.dir([0.0, 0.0, 1.0])
    }
}

/// Angle of `(x, y)` in 1024ths (`Math_Atan2` @ 0x430ab0).
fn angle(x: f32, y: f32) -> f32 {
    if x == 0.0 && y == 0.0 {
        return 0.0;
    }
    y.atan2(x) * 1024.0 / core::f32::consts::TAU
}

// ——— The mover ——————————————————————————————————————————————————————

/// What one mover step felt, for the creature (touch inputs, surface flags).
#[derive(Default)]
pub(crate) struct Felt {
    /// Hull point 0 / point 2 hit something (touch inputs L / R).
    pub left: bool,
    pub right: bool,
    /// `+0xe8`: the flags of what he touched (walls and floor).
    pub surface: u32,
}

/// The mover state (creature `+0x50`); field names are the exe's offsets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mover {
    /// `+0x8` / `+0x30`: the object and polygon whose frame he lives in.
    /// Set by a placement, changed when he attaches to another floor,
    /// **kept** while airborne (the camera keys off it).
    pub on: Option<(usize, usize)>,
    /// `+0x52`: standing on `on`.
    pub grounded: bool,
    /// `+0x44`, `+0x48`: his position in the polygon's plane.
    pub u: f32,
    pub v: f32,
    /// `+0x4c`: his origin's height above the plane (≥ the stance height).
    pub h: i32,
    /// `+0xa4`: the vertical speed of `h`.
    pub hvel: i32,
    /// `+0x50`: heading in the polygon's plane, 1024ths of a turn.
    pub heading: i32,
    /// `+0x24`: his up, the floor's normal in world space (unit).
    pub up: V3,
    /// `+0xa8`: world position of his origin.
    pub pos: V3,
    /// `+0xbc`: last tick's.
    pub prev: V3,
    /// `+0x94`: airborne velocity, units per tick.
    pub air: V3,
    /// `+0xa0`: airborne spin, 1024ths per tick.
    pub spin: i32,
    /// `+0x22` / `+0x10`: the surface object's rotation at the last
    /// surface test, if it was turned.
    pub obj_rot: Option<M3>,
    /// `+0xe4`: the flags of `on` (also `creature+0x134`).
    pub floor_flags: u32,
}

impl Default for Mover {
    fn default() -> Self {
        Mover {
            on: None,
            grounded: false,
            u: 0.0,
            v: 0.0,
            h: 0,
            hvel: 0,
            heading: 0,
            up: UP,
            pos: [0.0; 3],
            prev: [0.0; 3],
            air: [0.0; 3],
            spin: 0,
            obj_rot: None,
            floor_flags: 0,
        }
    }
}

/// The per-tick inputs the creature hands its mover.
pub(crate) struct Drive {
    /// `+0x780`: distance to walk this tick (the motor's last output).
    pub forward: i32,
    /// `+0x782`: heading change this tick.
    pub turn: i32,
    /// `+0x784`: stance height (41..207), where `h` rests.
    pub stance: i32,
    /// Gravity, units/tick² (−7; afflictions 10..12 change it).
    pub gravity: i32,
    /// `creature+0xae8`: flags the game handed him (a predator's bite);
    /// when set they *replace* what the hull felt (0x4188a8).
    pub injected: u32,
}

impl Mover {
    pub fn frame(&self, polys: &[Vec<LocalPoly>], sim: &Sim) -> Frame {
        Frame::of(polys, sim, self.on)
    }

    /// His forward and left in world space, from the heading in the
    /// polygon's plane.
    pub fn axes(&self, f: &Frame) -> (V3, V3) {
        let a = turns(self.heading);
        (f.dir([a.cos(), a.sin(), 0.0]), f.dir([-a.sin(), a.cos(), 0.0]))
    }

    /// `Mover_Place` @ 0x411bf0 (vtable slot 1, via `0x418310`): put him at
    /// `(u, v, h)` in the frame of polygon `on`, facing `heading`, not
    /// grounded, still; then look for a floor under him right away
    /// (slots 8 and 5). `on = None`: `at` is a world position (a portal
    /// whose polygon is missing, the debug drop).
    pub fn place(&mut self, polys: &[Vec<LocalPoly>], sim: &mut Sim, on: Option<(usize, usize)>, at: V3, heading: i32, stance: i32) {
        let on = on.filter(|&(o, p)| polys.get(o).and_then(|v| v.get(p)).is_some_and(|lp| lp.points.len() >= 3));
        self.on = on;
        self.grounded = false;
        self.heading = heading & 0x3ff;
        self.air = [0.0; 3];
        self.spin = 0;
        self.hvel = 0;
        let f = self.frame(polys, sim);
        if on.is_some() {
            [self.u, self.v] = [at[0], at[1]];
            self.h = at[2] as i32;
            self.pos = f.point(at);
        } else {
            let local = f.unpoint(at);
            [self.u, self.v] = [local[0], local[1]];
            self.h = local[2] as i32;
            self.pos = at;
        }
        self.up = f.normal();
        self.obj_rot = f.rotated.then_some(f.rot);
        let near = Near::gather(polys, sim, self.pos, 2000.0, FLOOR);
        let mut felt = Felt::default();
        self.find_surface(polys, sim, &near, stance, &mut felt);
        self.prev = self.pos;
    }

    /// `CreatureMover_Step` @ 0x417f40 → `Mover_Step` @ 0x411e10.
    pub fn step(&mut self, polys: &[Vec<LocalPoly>], sim: &mut Sim, near: &Near, d: &Drive, posted: &mut Vec<usize>) -> Felt {
        self.prev = self.pos;
        let heading_before = self.heading;
        self.integrate(polys, sim, d);
        let mut felt = self.collide(polys, sim, near, d.injected, posted);
        let was = self.grounded;
        self.find_surface(polys, sim, near, d.stance, &mut felt);
        if was && !self.grounded {
            // Let go (0x411e5c): the last step becomes the flight velocity
            // (kept × 256 in the exe) and the last turn the spin.
            self.air = sub(self.pos, self.prev);
            self.spin = wrap(self.heading - heading_before);
        }
        felt
    }

    /// `CreatureMover_Integrate` @ 0x418060 (slot 4).
    fn integrate(&mut self, polys: &[Vec<LocalPoly>], sim: &Sim, d: &Drive) {
        if self.grounded {
            // Walk in the polygon's plane: tables 0x472858 = cos, 0x473058
            // = sin (× 16384), indexed by the heading *before* this turn.
            let a = turns(self.heading);
            self.u += a.cos() * d.forward as f32;
            self.v += a.sin() * d.forward as f32;
            self.heading = (self.heading + d.turn) & 0x3ff;
            // His body bobs along the normal: gravity on `h`, resting on
            // the stance height.
            self.hvel += d.gravity;
            self.h += self.hvel;
            if self.h <= d.stance {
                self.h = d.stance;
                self.hvel = 0;
            }
            self.conveyor(polys, sim);
            self.pos = self.frame(polys, sim).point([self.u, self.v, self.h as f32]);
        } else {
            // Ballistic along *his* down, the last floor's normal:
            // `vel += up · g` (0x41814e: `up × 0x700 >> 10` on a velocity
            // kept in 1/256 units, the same 7 units/tick²).
            self.air = add(self.air, scale(self.up, d.gravity as f32));
            self.pos = add(self.pos, self.air);
            self.heading = (self.heading + self.spin) & 0x3ff;
        }
    }

    /// `0x41dc30`: a floor whose flags have bits 2..3 drags him along with
    /// its scrolling texture. The texture offset step of this tick `(du,
    /// dv)`, through the polygon's texel basis (stored × −256 as shorts),
    /// moves `(u, v)` by `−basis · step >> 8 >> shift`, `shift = (flags >>
    /// 2 & 3) − 1`: full, half or quarter speed. He rides the visible flow.
    fn conveyor(&mut self, polys: &[Vec<LocalPoly>], sim: &Sim) {
        let Some((o, p)) = self.on else { return };
        let flags = sim.frame_flags(o, p);
        if flags & SCROLL == 0 {
            return;
        }
        let Some(lp) = polys.get(o).and_then(|v| v.get(p)) else { return };
        let (du, dv) = sim.uv_step(o, p);
        let shift = ((flags >> 2) & 3) - 1;
        let m = lp.texel.map(|x| (-256.0 * x) as i32); // __ftol truncates
        self.u += (((m[0] * du + m[1] * dv) >> 8) >> shift) as f32;
        self.v += (((m[2] * du + m[3] * dv) >> 8) >> shift) as f32;
    }

    /// `CreatureMover_CollideAndTouch` @ 0x418450 (slot 9): the ten hull
    /// segments, each from his (progressively pushed) origin to a hull
    /// point turned by his heading in the polygon frame, against walls.
    fn collide(&mut self, polys: &[Vec<LocalPoly>], sim: &Sim, near: &Near, injected: u32, posted: &mut Vec<usize>) -> Felt {
        let mut felt = Felt::default();
        let f = self.frame(polys, sim);
        let a = turns(self.heading);
        let (c, s) = (a.cos(), a.sin());
        let mut p = self.pos;
        for (k, hp) in HULL.iter().enumerate() {
            let start = if k == 9 { add(p, scale(self.up, PROBE_UP)) } else { p };
            let local = [hp[0] * c - hp[1] * s, hp[0] * s + hp[1] * c, hp[2]];
            let end = add(p, f.dir(local));
            let Some(hit) = near.segment(start, end, Faces::Both, |t| t.flags & SOLID != 0) else { continue };
            if let Some(push) = behind(&hit, end) {
                posted.push(hit.obj);
                p = add(p, scale(push, 0.5));
                if self.grounded {
                    // Only the in-plane part moves him; `h` stays.
                    let dl = f.undir(sub(p, self.pos));
                    self.u += dl[0];
                    self.v += dl[1];
                    self.pos = f.point([self.u, self.v, self.h as f32]);
                } else {
                    self.pos = p;
                    let hf = Frame::of(polys, sim, Some((hit.obj, hit.poly)));
                    let mut vl = hf.undir(self.air);
                    if vl[2] < 0.0 {
                        vl[2] = bounce(hit.flags);
                        self.air = hf.dir(vl);
                    }
                }
            }
            // Every hit is felt, pushed or not (0x418842).
            let fl = hit.flags;
            if fl & AFFLICTION_BITS == SHIELD_BITS {
                felt.surface = fl;
            } else if felt.surface & AFFLICTION_BITS != SHIELD_BITS {
                felt.surface |= fl;
            }
            match k {
                0 => felt.left = true,
                2 => felt.right = true,
                _ => {}
            }
        }
        if injected != 0 {
            felt.surface = injected;
        }
        // His polygon's object feels him every tick, in flight too
        // (0x41894a: `on.vtable[9]`).
        if let Some((o, _)) = self.on {
            posted.push(o);
        }
        felt
    }

    /// `Mover_FindSurface` @ 0x411ec0 (slot 5): up to five probes along
    /// his down (slot 7 @ 0x418230), the first that crosses a floor from
    /// its front decides. Same polygon: stay (refresh his up). Another:
    /// attach (0x411f55). None: let go (0x41213e).
    fn find_surface(&mut self, polys: &[Vec<LocalPoly>], sim: &Sim, near: &Near, stance: i32, felt: &mut Felt) {
        let f = self.frame(polys, sim);
        let start = add(self.pos, scale(self.up, PROBE_UP));
        let found = PROBES.iter().find_map(|&(x, y)| {
            let end = add(self.pos, f.dir([x, y, -PROBE_BELOW_FEET - stance as f32]));
            let hit = near.segment(start, end, Faces::Both, |t| t.flags & FLOOR != 0)?;
            behind(&hit, end).map(|_| hit)
        });
        match found {
            None => self.grounded = false,
            Some(hit) if self.grounded && self.on == Some((hit.obj, hit.poly)) => self.up = f.normal(),
            Some(hit) => self.attach(polys, sim, &f, hit, stance),
        }
        let nf = self.frame(polys, sim);
        self.obj_rot = nf.rotated.then_some(nf.rot);
        // `+0xe4` = the floor's flags; they join the surface flags unless
        // a wall already brought lethal or affliction bits (0x412151).
        self.floor_flags = self.on.map_or(0, |(o, p)| sim.frame_flags(o, p));
        if felt.surface & EVENT_BITS == 0 {
            felt.surface |= self.floor_flags;
        }
    }

    /// 0x411f55: a new floor. His up becomes its normal; the heading is
    /// carried across the **hinge** the two planes share (the axis
    /// `old up × new up`, expressed in each polygon's frame: the heading
    /// keeps its angle to that line); `(u, v)` is where the probe hit,
    /// `h` the stance height.
    fn attach(&mut self, polys: &[Vec<LocalPoly>], sim: &Sim, old: &Frame, hit: Hit, stance: i32) {
        let old_up = self.up;
        let old_rot = self.obj_rot;
        let old_normal = self.on.and_then(|(o, p)| polys.get(o)?.get(p)).map_or(UP, |lp| lp.normal);
        self.on = Some((hit.obj, hit.poly));
        self.grounded = true;
        let nf = self.frame(polys, sim);
        self.up = nf.normal();
        let new_normal = polys[hit.obj][hit.poly].normal;
        // Same orientation, nothing turned: the heading needs no change
        // (the exe compares the polygons' two normal angles, `+0x4c/+0x4e`).
        if length(sub(old_normal, new_normal)) > 1e-4 || nf.rotated || old_rot.is_some() {
            let c = cross(old_up, self.up);
            let axis = if length(c) > 1e-6 {
                normalize(c)
            } else if self.up[1] != 0.0 {
                [0.0, -self.up[2], self.up[1]]
            } else {
                [0.0, -self.up[2], self.up[0]]
            };
            let before = Frame { rot: old_rot.unwrap_or(IDENTITY), ..*old }.undir(axis);
            let after = nf.undir(axis);
            let d = angle(after[0], after[1]) - angle(before[0], before[1]);
            self.heading = (self.heading + d.round() as i32) & 0x3ff;
        }
        let local = nf.unpoint(hit.point);
        self.u = local[0];
        self.v = local[1];
        self.h = stance;
        self.hvel = 0;
        self.pos = nf.point([self.u, self.v, self.h as f32]);
    }
}

/// Folds an angle difference into −512..512.
pub(crate) fn wrap(a: i32) -> i32 {
    let a = a & 0x3ff;
    if a > 0x200 { a - 0x400 } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ray_hits_a_triangle_from_both_sides() {
        let t = Tri { a: [0.0; 3], e1: [10.0, 0.0, 0.0], e2: [0.0, 10.0, 0.0], normal: UP, obj: 0, poly: 0, flags: SOLID };
        assert_eq!(ray_tri([1.0, 1.0, 5.0], [0.0, 0.0, -1.0], &t), Some(5.0));
        assert_eq!(ray_tri([1.0, 1.0, -5.0], [0.0, 0.0, 1.0], &t), Some(5.0));
        assert_eq!(ray_tri([11.0, 1.0, 5.0], [0.0, 0.0, -1.0], &t), None);
    }

    #[test]
    fn behind_accepts_only_the_back_side() {
        let h = Hit { dist: 0.0, point: [0.0; 3], obj: 0, poly: 0, flags: FLOOR, normal: UP, plane: [0.0; 3] };
        assert_eq!(behind(&h, [0.0, 0.0, -40.0]), Some([0.0, 0.0, 40.0]));
        assert_eq!(behind(&h, [0.0, 0.0, 40.0]), None, "in front: crossed from behind");
        assert_eq!(behind(&h, [0.0, 0.0, -2000.0]), None, "too deep");
    }

    #[test]
    fn surface_frame_round_trips() {
        let n = normalize([0.3, -0.5, 0.8]);
        let poly = sim::frame_from_normal(n);
        let rot = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let f = Frame { rot, pos: [100.0, 200.0, 300.0], poly, v0: [5.0, 6.0, 7.0], rotated: true };
        let p = [12.0, -34.0, 56.0];
        let back = f.unpoint(f.point(p));
        assert!(length(sub(back, p)) < 1e-3, "{back:?}");
        assert!(length(sub(f.normal(), mul(&rot, n))) < 1e-5);
    }
}
