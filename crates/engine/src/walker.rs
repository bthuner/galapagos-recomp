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

use crate::camera::{self, mat_mul, vec_mat};
use crate::sim::{self, Sim};
use crate::world;
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
    /// Its triangles ([`world::triangulate`]): a keyhole polygon, a wall
    /// with a doorway, keeps its doorway open (`World_Cast`'s even-odd
    /// test, `0x407830`).
    pub tris: Vec<[usize; 3]>,
    pub centre: V3,
    pub radius: f32,
    pub normal: V3,
    pub frame: M3,
    /// `∂(u, v)/∂(s, t)`: how far, in the polygon's frame, one texel of
    /// texture offset moves the image. Row-major 2×2.
    pub texel: [f32; 4],
    /// The exe's own integer copies, for the render angles (`0x412180`):
    /// the record's normal `+0x0` (× 1024, `0x409430`), its frame `+0x28`
    /// (4.12, polygon → object as `v · F`) and the normal's two angles
    /// `+0x4c` (azimuth − 90°) and `+0x4e` (elevation), built at 0x4092c5.
    pub normal_i: camera::I3,
    pub frame_i: camera::M3,
    pub angles: (i32, i32),
}

/// Every object's polygons, by object index (cameras have none), as the
/// exe's casts read them: the vertices of the object's current shape
/// frame (`0x423130`), which before the level-load pass is its state 0,
/// frame 0. The scrolling basis (`texel`) is built once, from that frame's
/// texels: `0x41d9e0` puts it on the mesh (slot 0x70) right before
/// `0x41d6d0` reads polygon `+0x44`, and nothing recomputes it.
pub(crate) fn local_polys(section: &Section) -> Vec<Vec<LocalPoly>> {
    section
        .objects
        .iter()
        .map(|o| match o {
            Object::World(w) => w.mesh.as_ref().map_or_else(Vec::new, |m| {
                let f0 = w.logic.shape.as_ref().and_then(|s| s.states.first()).and_then(|s| s.frames.first());
                let verts = f0.map_or(&m.vertices, |f| &f.vertices);
                m.polygons
                    .iter()
                    .enumerate()
                    .map(|(k, p)| {
                        let texels = f0.and_then(|f| f.polygon_mapping.get(k).copied()).unwrap_or(world::DEFAULT_TEXELS);
                        local_poly(polygon_points(p, verts), texels)
                    })
                    .collect()
            }),
            Object::Camera(_) => Vec::new(),
        })
        .collect()
}

/// A polygon's object-local points from a vertex list.
pub(crate) fn polygon_points(poly: &[u16], verts: &[[i16; 3]]) -> Vec<V3> {
    poly.iter().filter_map(|&k| verts.get(k as usize)).map(|v| v.map(|c| c as f32)).collect()
}

/// One polygon's mover data from its object-local points and the texels
/// `(u, v)` of its vertices 0, 1, 2 (`+0x44`).
pub(crate) fn local_poly(points: Vec<V3>, texels: [f32; 6]) -> LocalPoly {
    let n = points.len().max(1) as f32;
    let centre = scale(points.iter().fold([0.0; 3], |a, &p| add(a, p)), 1.0 / n);
    let radius = points.iter().map(|&p| length(sub(p, centre))).fold(0.0, f32::max);
    let ints: Vec<camera::I3> = points.iter().map(|p| p.map(|c| c as i32)).collect();
    let normal_i = if ints.len() >= 3 { camera::polygon_normal(ints[0], ints[1], ints[2]) } else { [0, 0, 1024] };
    let frame_i = camera::transpose(&camera::frame_from_normal(normal_i));
    let angles = normal_angles(normal_i);
    let (normal, frame, texel) = if points.len() >= 3 {
        let normal = normalize(cross(sub(points[1], points[0]), sub(points[2], points[0])));
        let frame = sim::frame_from_normal(normal);
        (normal, frame, texel_basis(&points, &frame, texels))
    } else {
        (UP, IDENTITY, [0.0; 4])
    };
    let tris = world::triangulate(&points);
    LocalPoly { points, tris, centre, radius, normal, frame, texel, normal_i, frame_i, angles }
}

/// `∂(u, v)/∂(s, t)` in the polygon's frame (`0x41d6d0`): the edges v1→v2
/// and v1→v0 carry the texel differences `T2 − T1` and `T0 − T1`, so the
/// basis is `E · T⁻¹`. With the initialiser's texels (`0x42320d`: (0,127)
/// (0,0) (127,0)) it is `(v2 − v1) / 127` along s and `(v0 − v1) / 127`
/// along t. inferred: the x87 tail of 0x41d6d0 is read as this inverse
/// (it is the only map consistent with both texel sets).
fn texel_basis(points: &[V3], frame: &M3, t: [f32; 6]) -> [f32; 4] {
    let es = mul_t(frame, sub(points[2], points[1]));
    let et = mul_t(frame, sub(points[0], points[1]));
    let (a, b, c, d) = (t[4] - t[2], t[0] - t[2], t[5] - t[3], t[1] - t[3]); // T = [[a, b], [c, d]]
    let det = a * d - b * c;
    if det.abs() < 1e-6 {
        return [0.0; 4];
    }
    let inv = [d / det, -b / det, -c / det, a / det];
    [es[0] * inv[0] + et[0] * inv[2], es[0] * inv[1] + et[0] * inv[3], es[1] * inv[0] + et[1] * inv[2], es[1] * inv[1] + et[1] * inv[3]]
}

/// `0x431cf0`: the angle between two vectors, `acos` in 1024ths of a turn,
/// rounded half up; 0 if either is null.
fn angle_between(a: camera::I3, b: camera::I3) -> i32 {
    let [a, b] = [a, b].map(|v| v.map(|c| c as f64));
    let (la, lb) = ((a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt(), (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt());
    if la == 0.0 || lb == 0.0 {
        return 0;
    }
    let c = ((a[0] * b[0] + a[1] * b[1] + a[2] * b[2]) / (la * lb)).clamp(-1.0, 1.0);
    let t = c.acos() * 1024.0 / core::f64::consts::TAU;
    (if t - t.floor() < 0.5 { t.floor() } else { t.ceil() }) as i32
}

/// A polygon record's `+0x4c` and `+0x4e` (built at 0x4092c5..0x409304):
/// the angle from +y to the normal's horizontal part `h = (n.x, n.y, 0)`
/// (`0x400 −` it when `n.x > 0`), and from `h` to the normal (`0x400 −`
/// it when `n.z < 0`). So `+0x4c` = the azimuth − 90° and `+0x4e` = the
/// elevation, the same pair `Math_VectorAngles` (0x430c50) gives. A normal
/// along ±z has `h = (0, 0x400, 0)`.
pub(crate) fn normal_angles(n: camera::I3) -> (i32, i32) {
    let h = if n[0] == 0 && n[1] == 0 { [0, 0x400, 0] } else { [n[0], n[1], 0] };
    let mut a = angle_between([0, 0x400, 0], h);
    if n[0] > 0 {
        a = 0x400 - a;
    }
    let mut b = angle_between(n, h);
    if n[2] < 0 {
        b = 0x400 - b;
    }
    (a, b)
}

/// Mendel's render angles (mover `+0xb4/+0xb6/+0xb8`), the second half of
/// slot 6 (`0x412180`, from 0x41223c). They are returned in object-angle
/// order `[rx, ry, rz]` = `[+0xb6, +0xb4, +0xb8]`, because the creature
/// hands the block to his body mesh (`0x402370`, mesh `+0x3c..+0x40`) and
/// to `MEnc` (`0x422a80`, object `+0xf0..+0xf4`), whose matrix is
/// `0x430ce0(+0xf2, +0xf0, +0xf4)` = [`sim::transform`] of these.
///
/// * His floor's object not turned (its drawn angles `+0x70..+0x74` all
///   zero, `0x409b10`): from the polygon record alone (0x4123bd),
///   `+0xb6 = +0x4c`, `+0xb4 = +0x4e + 0x200`, `+0xb8 = heading − +0x4c`.
/// * Turned: the normal through the object's rotation (`0x409940`) gives
///   yaw and pitch (`0x430c50`); the roll is measured by pushing
///   `0x430ce0(0, 0x380, heading)` through the polygon frame `+0x28`, the
///   object matrix and `0x430e40(yaw, pitch + 0x200, 0)` (`0x431b50` ×
///   3), `roll = atan(m[6], m[8]) + 0x300`.
///
/// The `0x380` pitch is the exe's (0x4122a0): with it the roll is exact
/// only at multiples of 90° of heading and off by up to ~10° in between
/// (`0x300` would be exact); see the tests.
pub(crate) fn render_angles(lp: &LocalPoly, obj_rot: [i32; 3], heading: i32) -> [i32; 3] {
    render_angles_probed(lp, obj_rot, heading, 0x380)
}

/// [`render_angles`] with the probe's pitch as a parameter (the tests try
/// `0x300`, which removes the distortion).
fn render_angles_probed(lp: &LocalPoly, obj_rot: [i32; 3], heading: i32, probe: i32) -> [i32; 3] {
    let (a, b) = lp.angles;
    if obj_rot == [0; 3] {
        return [a, b + 0x200, heading - a].map(|x| x & 0x3ff);
    }
    let t = camera::trig();
    let o = t.rot(obj_rot[0], obj_rot[1], obj_rot[2]);
    let (yaw, pitch) = t.dir_angles(vec_mat(lp.normal_i, &o));
    let pitch = pitch + 0x200;
    let m = mat_mul(&mat_mul(&mat_mul(&t.rot(0, probe, heading), &lp.frame_i), &o), &t.rot_t(yaw, pitch, 0));
    [yaw, pitch, t.angle(m[6], m[8]) + 0x300].map(|x| x & 0x3ff)
}

/// `Mover_Place` @ 0x411bf0 (0x411d85..0x411dc4) sets the render angles
/// with a shortcut: the object's angles `+0xf0..+0xf4` *added* to the
/// polygon's, `+0xb6 = +0xf2 + +0x4c`, `+0xb4 = +0xf0 + +0x4e + 0x200`,
/// `+0xb8 = heading − +0xf4 − +0x4c`. They last until slot 6 runs (at once
/// if the placement's floor test attaches him).
pub(crate) fn placed_angles(lp: &LocalPoly, obj_rot: [i32; 3], heading: i32) -> [i32; 3] {
    let (a, b) = lp.angles;
    [obj_rot[0] + a, obj_rot[1] + b + 0x200, heading - obj_rot[2] - a].map(|x| x & 0x3ff)
}

/// Render angles of an upright Mendel facing `heading` on a floor facing
/// +z: `+0x4c = 0`, `+0x4e = 0x100`.
pub(crate) fn upright_angles(heading: i32) -> [i32; 3] {
    [0, 0x300, heading & 0x3ff]
}

/// A world-space triangle gathered for this tick's casts.
pub(crate) struct Tri {
    pub a: V3,
    pub e1: V3,
    pub e2: V3,
    /// The polygon's unit normal; `a` is a corner of the triangle, so a
    /// point of the polygon's plane.
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
                for &[a, b, c] in &lp.tris {
                    out.push(Tri { a: w[a], e1: sub(w[b], w[a]), e2: sub(w[c], w[a]), normal, obj: i, poly: p, flags });
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
    /// `+0xb6, +0xb4, +0xb8`: his render angles as object angles `[rx,
    /// ry, rz]` ([`render_angles`]): how his body and `MEnc` are turned.
    /// Not in our saved games (recomputed on load).
    pub angles: [i32; 3],
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
            // inferred: `Mover_Init` @ 0x411b60 does not set them; upright.
            angles: upright_angles(0),
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

    /// Slot 6's render angles for where he is now (see [`render_angles`]);
    /// unchanged without a polygon.
    pub fn refresh_angles(&mut self, polys: &[Vec<LocalPoly>], sim: &Sim) {
        let Some((o, p)) = self.on else { return };
        let Some(lp) = polys.get(o).and_then(|v| v.get(p)).filter(|lp| lp.points.len() >= 3) else { return };
        self.angles = render_angles(lp, sim.pose(o).1, self.heading);
    }

    /// His forward and left in world space, from the heading in the
    /// polygon's plane (what the port drew him by before it had the render
    /// angles; the tests compare the two).
    #[cfg(test)]
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
        if let Some((o, p)) = on {
            [self.u, self.v] = [at[0], at[1]];
            self.h = at[2] as i32;
            self.pos = f.point(at);
            self.angles = placed_angles(&polys[o][p], sim.pose(o).1, self.heading);
        } else {
            let local = f.unpoint(at);
            [self.u, self.v] = [local[0], local[1]];
            self.h = local[2] as i32;
            self.pos = at;
            self.angles = upright_angles(self.heading);
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
            // Slot 6 (0x412180): world position and render angles.
            self.pos = self.frame(polys, sim).point([self.u, self.v, self.h as f32]);
            self.refresh_angles(polys, sim);
        } else {
            // Ballistic along *his* down, the last floor's normal:
            // `vel += up · g` (0x41814e: `up × 0x700 >> 10` on a velocity
            // kept in 1/256 units, the same 7 units/tick²).
            self.air = add(self.air, scale(self.up, d.gravity as f32));
            self.pos = add(self.pos, self.air);
            self.heading = (self.heading + self.spin) & 0x3ff;
            // 0x418209: the spin turns his drawn roll too; the other two
            // angles stay frozen in flight.
            self.angles[2] = (self.angles[2] + self.spin) & 0x3ff;
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
        // Slot 6.
        self.pos = nf.point([self.u, self.v, self.h as f32]);
        self.refresh_angles(polys, sim);
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

#[cfg(test)]
pub(crate) mod angle_tests {
    use super::*;

    /// A triangle facing `n` (unit), its first vertex at the origin.
    pub(crate) fn facing(n: V3) -> LocalPoly {
        let n = normalize(n);
        let a = normalize(if n[2].abs() < 0.9 { cross([0.0, 0.0, 1.0], n) } else { cross([1.0, 0.0, 0.0], n) });
        let b = cross(n, a);
        local_poly(vec![[0.0; 3], scale(a, 3000.0).map(f32::round), scale(b, 3000.0).map(f32::round)], crate::world::DEFAULT_TEXELS)
    }

    /// (forward, up, left) the old way: the heading in the polygon's frame,
    /// up its normal (what `Creature::sync` used to draw him by).
    pub(crate) fn frame_axes(lp: &LocalPoly, obj_rot: [i32; 3], heading: i32) -> [V3; 3] {
        let m = sim::transform([0; 3], obj_rot);
        let f = Frame { rot: [m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]], pos: [0.0; 3], poly: lp.frame, v0: lp.points[0], rotated: obj_rot != [0; 3] };
        let mover = Mover { heading, ..Mover::default() };
        let (fwd, left) = mover.axes(&f);
        [fwd, f.normal(), left]
    }

    /// (forward, up, left) of the render angles: model +x, −y, +z.
    pub(crate) fn angle_axes(angles: [i32; 3]) -> [V3; 3] {
        let m = sim::transform([0; 3], angles);
        let r = [m[0], m[1], m[2], m[3], m[4], m[5], m[6], m[7], m[8]];
        [mul(&r, [1.0, 0.0, 0.0]), mul(&r, [0.0, -1.0, 0.0]), mul(&r, [0.0, 0.0, 1.0])]
    }

    /// Largest angle in degrees between corresponding axes.
    pub(crate) fn degrees(a: [V3; 3], b: [V3; 3]) -> f32 {
        (0..3).map(|k| dot(normalize(a[k]), normalize(b[k])).clamp(-1.0, 1.0).acos().to_degrees()).fold(0.0, f32::max)
    }

    const NORMALS: [V3; 7] =
        [[0.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.3, -0.5, 0.8], [-0.7, 0.2, -0.3]];

    /// Worst disagreement (degrees) over every 16th heading.
    fn worst(lp: &LocalPoly, rot: [i32; 3], probe: i32) -> f32 {
        (0..1024).step_by(16).map(|h| degrees(frame_axes(lp, rot, h), angle_axes(render_angles_probed(lp, rot, h, probe)))).fold(0.0, f32::max)
    }

    /// Upright on a floor facing +z: `[0, 0x300, heading]`, forward
    /// `(cos h, sin h, 0)`, his model's down (+y) along −z, left along +y.
    #[test]
    fn upright_render_angles() {
        let lp = facing([0.0, 0.0, 1.0]);
        assert_eq!(lp.angles, (0, 0x100), "+0x4c, +0x4e of a floor");
        for h in [0, 100, 256, 700] {
            assert_eq!(render_angles(&lp, [0; 3], h), [0, 0x300, h]);
            let [f, u, l] = angle_axes(render_angles(&lp, [0; 3], h));
            let a = turns(h);
            assert!(length(sub(f, [a.cos(), a.sin(), 0.0])) < 1e-5, "forward {f:?}");
            assert!(length(sub(u, UP)) < 1e-5 && length(sub(l, [-a.sin(), a.cos(), 0.0])) < 1e-5);
        }
    }

    /// The polygon record's angles are those of `Math_VectorAngles`.
    #[test]
    fn record_angles_match_vector_angles() {
        let t = camera::trig();
        for n in NORMALS {
            let lp = facing(n);
            let (yaw, pitch) = t.dir_angles(lp.normal_i);
            let d = |a: i32, b: i32| wrap(a - b).abs();
            assert!(d(lp.angles.0, yaw) <= 1 && d(lp.angles.1, pitch) <= 1, "{n:?}: {:?} vs {:?}", lp.angles, (yaw, pitch));
        }
    }

    /// Old vs new. On a polygon of an object that is not turned (walking in
    /// section 0, the sideways pads of section 1, upside down at section
    /// 2's start) the exe's shortcut `[+0x4c, +0x4e + 0x200, heading −
    /// +0x4c]` is the frame-based orientation to within rounding (< 0.5°).
    /// On a turned object (the capsule) the exe's roll probe, pitched by
    /// `0x380`, is off by up to ~10° between the right angles of heading;
    /// with `0x300` it would agree, which checks the rest of the port.
    #[test]
    fn render_angles_agree_with_the_frame() {
        for n in NORMALS {
            let lp = facing(n);
            let w = worst(&lp, [0; 3], 0x380);
            assert!(w < 0.5, "{n:?} unturned: {w}°");
            for rot in [[256, 0, 0], [0, 100, 0], [0, 0, 300], [100, 200, 300], [512, 0, 0]] {
                let exact = worst(&lp, rot, 0x300);
                assert!(exact < 0.6, "{n:?} {rot:?} with 0x300: {exact}°");
                let exe = worst(&lp, rot, 0x380);
                assert!((9.0..10.5).contains(&exe), "{n:?} {rot:?} the exe's: {exe}°");
                // His up is always right: only the roll about it suffers.
                for h in (0..1024).step_by(64) {
                    let up = |a: [V3; 3]| a[1];
                    let e = dot(up(frame_axes(&lp, rot, h)), up(angle_axes(render_angles(&lp, rot, h)))).clamp(-1.0, 1.0).acos().to_degrees();
                    assert!(e < 0.6, "{n:?} {rot:?} h {h}: up off by {e}°");
                }
            }
        }
        // At the right angles of heading the exe's roll is exact.
        let lp = facing([0.0, 0.0, 1.0]);
        for h in [0, 256, 512, 768] {
            let e = degrees(frame_axes(&lp, [100, 200, 300], h), angle_axes(render_angles(&lp, [100, 200, 300], h)));
            assert!(e < 0.6, "h {h}: {e}°");
        }
    }

    /// `Mover_Place`'s shortcut adds the object's angles to the polygon's:
    /// right for an unturned object, only roughly for a turned one (slot 6
    /// replaces it as soon as he attaches).
    #[test]
    fn placed_angles_match_slot_6_when_unturned() {
        for n in NORMALS {
            let lp = facing(n);
            for h in [0, 300, 900] {
                assert_eq!(placed_angles(&lp, [0; 3], h), render_angles(&lp, [0; 3], h));
            }
        }
    }
}
