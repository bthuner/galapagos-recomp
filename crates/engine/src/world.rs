//! Turning a parsed world section into buffers a GPU can draw.
//!
//! The 1997 engine rasterised polygons itself, in 256 colours, or handed
//! them to Direct3D. We hand the same geometry to WebGL2 instead, so this
//! module flattens the object graph into one interleaved vertex array and
//! one stack of textures:
//!
//! ```text
//! vertex = x, y, z, u, v, layer, light, nx, ny, nz     (10 × f32)
//! ```
//!
//! * Polygons on disk are n-gons, mostly convex, which we fan into
//!   triangles; the few concave ones (rings drawn with a zero-width
//!   bridge) are ear-clipped as the exe does ([`triangulate`]).
//! * There are no texture coordinates in the mesh. The engine's polygon
//!   initialiser (0x42320d) gives the first three vertices the texel
//!   coordinates (0,127), (0,0), (127,0): v1 is the origin, u runs along
//!   v1→v2 and v runs along v1→v0. That defines an affine map over the
//!   polygon's plane; a quad's fourth vertex lands on (127,127). But
//!   every object's initial state selects a *shape* (vertex morph,
//!   [`WorldMesh::set_shape`]), whose frame carries the real texels of
//!   vertices 0, 1, 2 (`0x423530` copies them to polygon `+0x44`), so
//!   the defaults only last until the level-load pass.
//! * `layer` indexes a TEXTURE_2D_ARRAY; -1 means "untextured" (no
//!   appearance data) and -2 "not drawn in the current appearance state"
//!   (a polygon whose frames never gave it a texture: invisible collision
//!   geometry), kept in the buffer so a state change can reveal it by
//!   rewriting one float per vertex. -4 marks *glass* (a `-4` frame, see
//!   [`GLASS`]): drawn untextured and additively, after everything else.
//! * Vertices are *object-local*. Each object carries a 4×4 transform, so
//!   the simulation can move a platform by changing 16 floats instead of
//!   rebuilding geometry. The renderer draws one range per object.
//! * `light` is the polygon's *material* (a section colour ramp, the
//!   shade number of its appearance frame) and `n` its object-local unit
//!   normal. The exe relights every drawn polygon every frame
//!   (`0x402490` → [`light_level`]), with the normal turned by the
//!   object's current rotation; the vertex shader does the same with the
//!   model matrix and the material table [`WorldMesh::materials`], then
//!   turns the level into the Direct3D colours of [`d3d_colour`] and the
//!   per-vertex fog of [`fog_factor`]. A negative `light` is *pre-lit*
//!   (Mendel, creature.rs): level `-1 - light`, fog amount in `nx`.

use galapagos_formats::worlds::{Object, PolyFrame, Section, WorldObject};

pub const FLOATS_PER_VERTEX: usize = 10;
const TEX: f32 = 128.0;

/// The polygon initialiser's texels (`0x42320d`): `(u, v)` of vertices
/// 0, 1, 2 = (0,127), (0,0), (127,0).
pub const DEFAULT_TEXELS: [f32; 6] = [0.0, 127.0, 0.0, 0.0, 127.0, 0.0];

/// Texture coordinates (in textures, texels / 128) of every vertex of a
/// polygon whose vertices 0, 1, 2 carry the texels `t` = `(u0, v0, u1,
/// v1, u2, v2)`: the affine map on the polygon's plane that sends them
/// there, evaluated at each vertex, as the Direct3D renderer does
/// (`0x40b2f0`, `0x40b62e..0x40b6b1`).
pub fn polygon_uvs(pts: &[V3], t: [f32; 6]) -> Vec<(f32, f32)> {
    let eu = sub(pts[2], pts[1]);
    let ev = sub(pts[0], pts[1]);
    pts.iter()
        .map(|&p| {
            let (s, q) = affine_st(sub(p, pts[1]), eu, ev);
            ((t[2] + s * (t[4] - t[2]) + q * (t[0] - t[2])) / TEX, (t[3] + s * (t[5] - t[3]) + q * (t[1] - t[3])) / TEX)
        })
        .collect()
}

/// The slice of the vertex array belonging to one object.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct DrawRange {
    pub first: u32,
    pub count: u32,
}

/// The `layer` of a glass polygon. A `-4` frame sets the polygon's glass
/// flag (`0x423791`: flag = texture == -4, cleared by any other frame;
/// stored by `0x4023b0` at polygon `+0x62`). The Direct3D renderer draws
/// those polygons in a bucket of their own (bucket `0x27` of the draw loop
/// `0x40f000`) with depth writes off, alpha blending on, no texture and no
/// culling; the blend factors set at start-up (`0x40a640`) are `ONE, ONE`,
/// so glass *adds* its colour to whatever is behind it: the diffuse grey
/// of its light level, halved (`0x40c478`, see [`d3d_colour`]). The draw
/// loop skips glass whose level is 0. The software renderer (`0x404660`)
/// gets the same look its own way: glass never hides a span, it raises the
/// light level of the surface behind it. The intro capsule (#3 of section
/// 0) is glass: material 49 (base 0, level 0, invisible) while docked,
/// 17–19 on its ride, the grey veil of the footage.
pub const GLASS: f32 = -4.0;

/// Rows of the lighting table: `pow(2, 5)` (`0x40346f`, renderer
/// `+0x128680`).
pub const LEVELS: i32 = 32;

/// The fog distance of the Direct3D renderer (`0x45f394`): view depth is
/// clamped to it (`0x40c4d8`, `0x5dc0`), and a material with `m5 = 1` is
/// fully fogged there.
#[cfg_attr(not(test), allow(dead_code))]
pub const FOG_DEPTH: f32 = 24000.0;

/// A section material (a "colour ramp"), as the renderer keeps it: six
/// `u16 / 65535` (`Renderer_ReadColorRamps` @ 0x42ece0 → `0x405620`,
/// 24 bytes each at renderer `+0x1864`).
///
/// | | read by | meaning |
/// |---|---|---|
/// | `m[0]` | `0x401b70` | base light |
/// | `m[1]` | `0x401b70` | added after the clamp at the middle row: over-brightening |
/// | `m[2]` | `0x401b70` | ambient factor |
/// | `m[3]` | `0x401b70` | directional-light factor |
/// | `m[4]` | — | never read by the lighting or the Direct3D path |
/// | `m[5]` | `0x40c502` | fog amount (the software renderer reads it per span, `0x404660`) |
pub fn material(m: [u16; 6]) -> [f32; 6] {
    m.map(|x| x as f32 / 65535.0)
}

/// The section's lighting, from its environment record (`0x42e880`).
#[derive(Clone, Copy, Debug)]
pub struct Light {
    /// The directional light's unit vector, the direction light travels
    /// (`0x401470` normalises the three `i32`).
    pub dir: [f32; 3],
    /// `u16 / 65535` (`0x401510`, light `+0x38`).
    pub intensity: f32,
    /// Renderer `+0x128684`: `trunc(u16 / 65535)` (`0x42e940`, `_ftol`),
    /// so 0 unless the field is 65535; 0 in every section on the CD.
    pub ambient: f32,
}

impl Light {
    pub fn of(env: &galapagos_formats::worlds::Environment) -> Self {
        let l = env.light_direction;
        Light {
            dir: normalize([l[0] as f32, l[1] as f32, l[2] as f32]),
            intensity: env.light_intensity as f32 / 65535.0,
            ambient: (env.unknown_level as f32 / 65535.0).trunc(),
        }
    }
}

/// A polygon's light level, 0..=31: `0x401b70`, called by the mesh draw
/// `0x402490` for every polygon that survives clipping, every frame.
///
/// ```text
/// L     = m0 + m2·ambient
/// for each light: d = n·l;  if d < 0: L −= m3·intensity·d   (0x401bc3..0x401c0e)
/// t     = min(L / 2, 0.5) + m1, at most 1                     (0x401c10..0x401c53)
/// level = trunc(t · (rows − 1))                               (0x401c5b, _ftol)
/// ```
///
/// `n` is the polygon's unit normal `(v2 − v1) × (v0 − v1)` (`0x4019a0`,
/// object-local, normalised), turned into camera space with the object
/// matrix every frame (`0x401640`); `l` is the light vector turned into
/// camera space the same way (`0x401520`). Both rotations are the same,
/// so the dot product is the world-space one: an object that turns is
/// relit. Light travels along `l`, so a face is lit when it faces
/// against it (`d < 0`), never from behind.
pub fn light_level(m: [f32; 6], light: &Light, n: [f32; 3]) -> i32 {
    let mut l = m[0] + m[2] * light.ambient;
    let d = dot(n, light.dir);
    if d < 0.0 {
        l -= m[3] * light.intensity * d;
    }
    let t = ((l / 2.0).min(0.5) + m[1]).min(1.0);
    (t * (LEVELS - 1) as f32) as i32
}

/// The Direct3D colours of a light level (`0x40c3e2..0x40c4b5`): the
/// diffuse grey that modulates the texture, and the specular grey that
/// Direct3D adds on top, both 0..=255 per channel.
///
/// ```text
/// level <  16:  diffuse = min(trunc(level / 15 · 255), 255)   specular = 0
/// level >= 16:  diffuse = 255   specular = min(trunc((level − 16) / 16 · 255), 255)
/// glass:        diffuse = diffuse / 2                         specular = 0
/// then          specular = min(specular + DAT_0048b628, 255)
/// ```
///
/// So the lower half of the table darkens the texture and the upper half
/// washes it out towards white. `DAT_0048b628` is the black level of the
/// screen's red ramp (written by `0x434730` with ramp[0] whenever a
/// palette flash or fade is applied, `0x436e00`..`0x437690`): the
/// Direct3D stand-in for flashes the 8-bit renderer did with its palette.
/// It is 0 outside flashes; the web port draws flashes with flash.rs, so
/// it is left out here.
#[cfg_attr(not(test), allow(dead_code))]
pub fn d3d_colour(level: i32, glass: bool) -> (u8, u8) {
    let half = LEVELS / 2;
    let (mut diffuse, mut specular) = if level < half {
        (((level as f32 / (half - 1) as f32 * 255.0) as i32).min(255), 0)
    } else {
        (255, (((level - half) as f32 / half as f32 * 255.0) as i32).min(255))
    };
    if glass {
        diffuse /= 2;
        specular = 0;
    }
    (diffuse as u8, specular as u8)
}

/// Direct3D vertex fog (`0x40c4d3..0x40c5b9`), 0 (black) ..= 255 (clear):
/// the specular alpha, with `FOGENABLE` on (`0x40a640`, render state 28)
/// and the fog colour never set (Direct3D's default, black).
///
/// ```text
/// z   = min(trunc(1 / rhw), 24000)          the vertex's view depth
/// fog = trunc(255 − z / 24000 · m5 · 255)
/// ```
#[cfg_attr(not(test), allow(dead_code))]
pub fn fog_factor(m5: f32, depth: f32) -> u8 {
    let z = (depth.max(0.0) as u32).min(FOG_DEPTH as u32) as f32;
    (255.0 - z / FOG_DEPTH * m5 * 255.0) as i32 as u8
}

/// The grey a glass polygon adds, 0..1, for one material, unlit (glass
/// materials have no directional term on the CD).
#[cfg_attr(not(test), allow(dead_code))]
pub fn glass_grey(m: [u16; 6]) -> f32 {
    let light = Light { dir: [0.0, 0.0, -1.0], intensity: 0.0, ambient: 0.0 };
    let level = light_level(material(m), &light, [0.0, 0.0, 1.0]);
    if level <= 0 {
        return 0.0;
    }
    d3d_colour(level, true).0 as f32 / 255.0
}

/// Floats per material in [`WorldMesh::materials`].
pub const FLOATS_PER_MATERIAL: usize = 4;

pub struct WorldMesh {
    pub vertices: Vec<f32>,
    /// One range per drawn object, in section object order.
    pub draws: Vec<DrawRange>,
    /// For each section object (by list index): its draw index, if it has a mesh.
    pub draw_of_object: Vec<Option<usize>>,
    /// For each draw: the vertex range of every mesh polygon (count 0 if the
    /// polygon was degenerate), so appearance changes can patch in place.
    pub poly_ranges: Vec<Vec<DrawRange>>,
    /// For each draw and polygon: is its current frame's texture -5? One
    /// such polygon hides the whole object (see `apply_frame`).
    hides_object: Vec<Vec<bool>>,
    /// For each draw: hidden by its class (a predator blinking through
    /// its materialise effect, predator.rs), whatever its frames say.
    blinking: Vec<bool>,
    /// One byte per draw: 1 = drawn, 0 = hidden by a -5 frame.
    pub visible: Vec<u8>,
    /// One byte per draw: the mesh's backface flag (`+0x2a`, set by
    /// `0x4022f0`). 1 (the mesh constructor's default, `0x402150`) unless
    /// the frame last applied to the object's *last* polygon was glass
    /// (`0x4237e8`, `0x4239c0`). See [`culls_glass`].
    pub cull: Vec<u8>,
    /// For each draw and polygon: its current material (the last frame
    /// shade number `>= 0`; `0x402450` stores it at polygon `+0x60`).
    material: Vec<Vec<i16>>,
    /// The section's materials for the shader, [`FLOATS_PER_MATERIAL`]
    /// each: `m0 + m2·ambient`, `m1`, `m3·intensity`, `m5` (the terms of
    /// [`light_level`] that do not depend on the normal, and the fog).
    pub materials: Vec<f32>,
    /// The section's light ([`Light::of`]).
    pub light: Light,
    /// Texture coordinates before any per-frame offset, 2 per vertex.
    pub base_uv: Vec<f32>,
    /// For each vertex: which corner of its polygon it is (the polygon's
    /// triangles are [`triangulate`]'s, not always a fan from corner 0).
    pub corner: Vec<u16>,
    /// The appearance frame's texel offset of each vertex's polygon, in
    /// textures, 2 per vertex: drawn UV = `base_uv + uv_offset`, kept apart
    /// so a shape frame can replace `base_uv` exactly.
    uv_offset: Vec<f32>,
    /// One column-major 4×4 matrix per drawn object (16 f32 each).
    pub transforms: Vec<f32>,
    /// `textures.len() / (128*128*4)` RGBA layers.
    pub textures: Vec<u8>,
    pub texture_count: u32,
    /// min x, y, z, max x, y, z.
    pub bounds: [f32; 6],
    pub objects_drawn: u32,
    /// Section environment far clip, in world units.
    pub far_clip: f32,
    /// The fog and clear colour, 0 (black) or 1 (white): see [`fog_white`].
    pub fog: f32,
}

/// Which faces are drawn (observed). Two tests, both keeping the side the
/// polygon normal `(v2 − v1) × (v0 − v1)` (`0x4019a0`) points to:
///
/// * the mesh draw `0x402490` skips a polygon when the mesh's flag
///   `+0x2a` is set and `n · v0 >= 0` in camera space (eye at the
///   origin, `0x402508..0x4025c6`): the normal points away from the eye.
///   Both renderers go through it.
/// * Direct3D: the draw loop `0x40f000` sets `CULLMODE` (render state
///   `0x16`) to 3, counter-clockwise, for every texture bucket and to 1,
///   none, for the glass bucket `0x27`. It fans each polygon from its last
///   vertex backwards, `(n−1, k+1, k)` (`0x40f325..0x40f390`), and the
///   camera maps its z to screen-up with y down on screen (`0x401640`),
///   so a front face lands clockwise and only back faces are culled.
///
/// Every textured polygon therefore loses its back face. Glass is only
/// subject to the first test, which the frame appliers turn off for an
/// object whose last polygon is glass (`0x423730`, `0x423810`).
pub fn culls_glass(last_polygon_is_glass: bool) -> bool {
    !last_polygon_is_glass
}

/// The Direct3D fog colour, which is also the colour the screen is
/// cleared to. The environment's background colour goes to the
/// renderer's `+0xc` (`0x40aa80` in Direct3D mode, after `0x4039a0`
/// stores it at `+0x128678`), which sets render state `FOGCOLOR`
/// (`0x22`, sent by the frame start `0x40f6a6`) to white if the red
/// channel is non-zero, else black; the frame's clear (`0x40f5d7`) makes
/// the same choice. Section 1 is the only white one on the CD.
pub fn fog_white(env: &galapagos_formats::worlds::Environment) -> bool {
    env.background[0] != 0
}

type V3 = [f32; 3];
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn normalize(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l == 0.0 { a } else { [a[0] / l, a[1] / l, a[2] / l] }
}

/// The triangles the Direct3D renderer draws for one polygon, as
/// `[a, b, c]` indices into `pts`, wound like the polygon (the same side
/// faces the normal `(v2 − v1) × (v0 − v1)`).
///
/// Most polygons are convex and are fanned, `(0, k, k + 1)`. But the
/// artists also drew polygons **with holes**: section 0's save pad #87
/// polygon 5 is a hexagonal ring, the outer hexagon and the inner one
/// joined by a zero-width bridge (14 vertices, two of them repeated). A fan
/// from vertex 0 covers the hole, and with it the shimmering pool (polygon
/// 7, texture 21) 37 units below. The exe ear-clips such polygons
/// (`0x40be00`, called with the texel map `0x40b2f0` when the renderer
/// first meets a mesh, `0x40c130`); the result goes to the per-polygon
/// record `+0x168` (triangle count) / `+0x16c` (triangles), and `+0x168 ==
/// 0` means "fan it". Observed, step by step:
///
/// 1. Every vertex is moved by `−v1` and turned by the rotation that
///    sends the polygon normal to `+z` (`0x431ea0`, `0x430ff0`,
///    `0x431a60`); only `(x, y)` is used.
/// 2. A vertex is *convex* when `(n − c) ⨯ (p − c)`, with its neighbours
///    `p` and `n`, computed in `f32` as `(n.y − c.y)(p.x − c.x) − (n.x −
///    c.x)(p.y − c.y)`, is below `800` (`0x40bab0`): a tolerance that also
///    accepts flat and zero-area corners (the bridge).
/// 3. If every vertex is convex the polygon is fanned. Otherwise, walking
///    from vertex 0 and always on to the next remaining vertex, `cur` is
///    clipped as the ear `(prev, cur, next)` when it is convex and no other
///    remaining vertex lies in that triangle (`0x40bd10` → `0x40bb50`: an
///    inclusive bounding box, then a crossing count over the three edges,
///    each taken from its lower end, `lo.y <= p.y < hi.y`, crossing when
///    `ftol(x on the edge) <= ftol(p.x)`, the slope and the x held in
///    `f32`), until `n − 2` triangles are out. The exe stores `(next, cur,
///    prev)` and draws it as it draws fans, back to front; here `(prev,
///    cur, next)` keeps our fans' winding.
///
/// inferred: the exe loops forever if no ear can be found; we fan what
/// remains instead (no polygon of the CD needs it).
pub fn triangulate(pts: &[V3]) -> Vec<[usize; 3]> {
    let n = pts.len();
    let fan = || (1..n.saturating_sub(1)).map(|k| [0, k, k + 1]).collect::<Vec<_>>();
    if n < 4 {
        return fan();
    }
    let p = plane_xy(pts);
    let Some(p) = p else { return fan() };
    let convex = |a: usize, b: usize, c: usize| {
        let (pa, pb, pc) = (p[a], p[b], p[c]);
        let v = (pc.1 - pb.1) as f32 * (pa.0 - pb.0) as f32 - (pc.0 - pb.0) as f32 * (pa.1 - pb.1) as f32;
        v < 800.0
    };
    if (0..n).all(|i| convex((i + n - 1) % n, i, (i + 1) % n)) {
        return fan();
    }
    let mut removed = vec![false; n];
    let mut out = Vec::with_capacity(n - 2);
    let mut cur = 0;
    let mut misses = 0;
    while out.len() < n - 2 {
        let mut prev = cur;
        loop {
            prev = (prev + n - 1) % n;
            if !removed[prev] {
                break;
            }
        }
        let mut next = cur;
        loop {
            next = (next + 1) % n;
            if !removed[next] {
                break;
            }
        }
        let tri = [p[prev], p[cur], p[next]];
        let ear = convex(prev, cur, next) && (0..n).all(|s| removed[s] || s == prev || s == cur || s == next || !in_triangle(tri, p[s]));
        if ear {
            out.push([prev, cur, next]);
            removed[cur] = true;
            misses = 0;
        } else {
            misses += 1;
            if misses > n {
                // inferred: see above. Fan the remaining vertices.
                let rest: Vec<usize> = (0..n).filter(|&i| !removed[i]).collect();
                out.extend((1..rest.len() - 1).map(|k| [rest[0], rest[k], rest[k + 1]]));
                break;
            }
        }
        cur = next;
    }
    out
}

/// `(x, y)` of every vertex in the polygon's plane, as `0x40be00` sees
/// them: `v − v1` turned by the rotation `0x430ff0` builds from the normal
/// `(v2 − v1) × (v0 − v1)` (`0x431ea0`), applied by `0x431a60`. With `n̂ =
/// n / |n|`, `c = n̂.z`, `s = √(n̂.x² + n̂.y²)` and `(a, b)` the unit
/// horizontal direction of `n` (`(0, 1)` when `n` is vertical), the rows
/// it uses are `x = (c·a² + b², (c − 1)·a·b, −s·a)` and `y = ((c − 1)·a·b,
/// c·b² + a², −s·b)`. `None` for a degenerate first corner.
fn plane_xy(pts: &[V3]) -> Option<Vec<(f64, f64)>> {
    let d: Vec<[f64; 3]> = pts.iter().map(|q| [0, 1, 2].map(|k| q[k] as f64 - pts[1][k] as f64)).collect();
    let (v2, v0) = (d[2], d[0]);
    let nv = [v2[1] * v0[2] - v0[1] * v2[2], v0[0] * v2[2] - v2[0] * v0[2], v2[0] * v0[1] - v0[0] * v2[1]];
    let len = (nv[0] * nv[0] + nv[1] * nv[1] + nv[2] * nv[2]).sqrt();
    if len == 0.0 {
        return None;
    }
    let r = (nv[0] * nv[0] + nv[1] * nv[1]).sqrt();
    let (c, s) = (nv[2] / len, r / len);
    let (a, b) = if r == 0.0 { (0.0, 1.0) } else { (nv[0] / r, nv[1] / r) };
    let rx = [c * a * a + b * b, (c - 1.0) * a * b, -s * a];
    let ry = [(c - 1.0) * a * b, c * b * b + a * a, -s * b];
    Some(d.iter().map(|v| (v[0] * rx[0] + v[1] * rx[1] + v[2] * rx[2], v[0] * ry[0] + v[1] * ry[1] + v[2] * ry[2])).collect())
}

/// `0x40bb50`: is `q` inside the triangle `t` (see [`triangulate`])?
fn in_triangle(t: [(f64, f64); 3], q: (f64, f64)) -> bool {
    let (xs, ys) = (t.map(|p| p.0), t.map(|p| p.1));
    if !(ys.iter().any(|&y| y <= q.1) && ys.iter().any(|&y| q.1 <= y) && xs.iter().any(|&x| x <= q.0) && xs.iter().any(|&x| q.0 <= x)) {
        return false;
    }
    let mut odd = false;
    for i in 0..3 {
        let (u, w) = (t[i], t[(i + 1) % 3]);
        let (lo, hi) = if w.1 < u.1 { (w, u) } else { (u, w) };
        if lo.1 <= q.1 && q.1 < hi.1 {
            let slope = ((hi.0 - lo.0) / (hi.1 - lo.1)) as f32;
            let x = ((q.1 - lo.1) * slope as f64 + lo.0) as f32;
            // __ftol truncates towards zero.
            if (x as i32) <= (q.0 as i32) {
                odd = !odd;
            }
        }
    }
    odd
}

/// Coordinates (s, t) of `p` in the basis (e1, e2) anchored at the
/// polygon's first vertex, solved by least squares (2×2 normal equations).
fn affine_st(p: V3, e1: V3, e2: V3) -> (f32, f32) {
    let (a, b, c) = (dot(e1, e1), dot(e1, e2), dot(e2, e2));
    let (d, e) = (dot(p, e1), dot(p, e2));
    let det = a * c - b * b;
    if det.abs() < 1e-6 {
        return (0.0, 0.0);
    }
    ((d * c - e * b) / det, (e * a - d * b) / det)
}

impl WorldMesh {
    fn refresh_visible(&mut self, draw: usize) {
        self.visible[draw] = !(self.blinking[draw] || self.hides_object[draw].iter().any(|&x| x)) as u8;
    }

    /// Hide (or show again) a whole object for its class's own reasons.
    pub fn set_blinking(&mut self, draw: usize, hidden: bool) {
        if let Some(b) = self.blinking.get_mut(draw) {
            *b = hidden;
            self.refresh_visible(draw);
        }
    }

    pub fn build(section: &Section) -> Self {
        let mut vertices = Vec::new();
        let mut draws = Vec::new();
        let mut transforms = Vec::new();
        let mut draw_of_object = Vec::new();
        let mut poly_ranges = Vec::new();
        let mut corner = Vec::new();
        let mut bounds = [f32::MAX, f32::MAX, f32::MAX, f32::MIN, f32::MIN, f32::MIN];
        let mut objects_drawn = 0;

        for object in &section.objects {
            let Object::World(obj) = object else {
                draw_of_object.push(None);
                continue;
            };
            draw_of_object.push(obj.mesh.as_ref().map(|_| draws.len()));
            if obj.mesh.is_some() {
                objects_drawn += 1;
                let first = (vertices.len() / FLOATS_PER_VERTEX) as u32;
                poly_ranges.push(emit_object(obj, &mut vertices, &mut corner, &mut bounds));
                let count = (vertices.len() / FLOATS_PER_VERTEX) as u32 - first;
                draws.push(DrawRange { first, count });
                let [x, y, z] = obj.position.map(|c| c as f32);
                transforms.extend_from_slice(&[1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., x, y, z, 1.]);
            }
        }

        let texture_count = section.textures.len() as u32;
        let mut textures = Vec::with_capacity(section.textures.len() * 128 * 128 * 4);
        for i in 0..section.textures.len() {
            for rgb in section.texture_rgb(i).chunks_exact(3) {
                textures.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        let far_clip = section.environment.far_clip as f32;
        let fog = if fog_white(&section.environment) { 1.0 } else { 0.0 };
        // emit_object wrote un-offset UVs; keep a copy so frame offsets can
        // be re-applied whenever a polygon's appearance frame changes.
        let base_uv = vertices.chunks_exact(FLOATS_PER_VERTEX).flat_map(|v| [v[3], v[4]]).collect();
        let uv_offset = vec![0.0; base_uv_len(&vertices)];
        let hides_object = poly_ranges.iter().map(|p| vec![false; p.len()]).collect();
        let visible = vec![1; draws.len()];
        let cull = vec![1; draws.len()];
        let blinking = vec![false; draws.len()];
        let material = poly_ranges.iter().map(|p| vec![0; p.len()]).collect();
        let light = Light::of(&section.environment);
        let materials = section
            .color_ramps
            .iter()
            .flat_map(|&m| {
                let m = self::material(m);
                [m[0] + m[2] * light.ambient, m[1], m[3] * light.intensity, m[5]]
            })
            .collect();
        let mut mesh = WorldMesh {
            vertices, draws, draw_of_object, poly_ranges, hides_object, blinking, visible, cull, material, materials, light,
            base_uv, corner, uv_offset, transforms, textures, texture_count, bounds, objects_drawn, far_clip, fog,
        };
        // Apply each polygon's initial appearance frame (layer, UV offset).
        for object in &section.objects {
            let Object::World(obj) = object else { continue };
            let Some(states) = &obj.appearance else { continue };
            let Some(state) = states.first() else { continue };
            let draw = mesh.draw_of_object[obj_index(section, obj)].unwrap_or(usize::MAX);
            if draw == usize::MAX {
                continue;
            }
            for (p, frames) in state.polygons.iter().enumerate() {
                mesh.apply_frame(draw, p, frames, 0);
            }
        }
        mesh
    }

    /// Writes a polygon's appearance frame into the vertex buffer: texture
    /// layer (or hidden / untextured) and the texel offset that makes
    /// surfaces scroll. Returns the patched vertex range.
    pub fn apply_frame(&mut self, draw: usize, poly: usize, frames: &[PolyFrame], index: usize) -> Option<DrawRange> {
        let r = *self.poly_ranges.get(draw)?.get(poly)?;
        let frame = frames.get(index).or(frames.first());
        // What a frame's texture number does, exactly as the exe's frame
        // applier (0x423730) reads it:
        //   >= 0   use that texture;
        //   -5     hide the *whole object* (a flag passed to 0x4022e0 after
        //          the polygon loop): start pad #57 and the birth tube #54
        //          carry it, so their stacks are never drawn;
        //   -4     glass (see `GLASS`): the texture is left alone, the
        //          polygon is drawn as an additive grey veil;
        //   -2/-3  change nothing: the polygon keeps its previous texture.
        // On the CD, -2/-3 frames sit inside flip-books (e.g. pad #83:
        // 9, -3, -3), so "previous" is the last earlier frame of the same
        // list with a real texture. inferred: resolving it from the list
        // rather than from render history keeps snapshots reproducible.
        // A list with none (section 1's pad #163, states 6 and 7: only
        // -2/-3 frames) keeps the texture the polygon shows now, as the
        // exe does; only a polygon that never had one stays hidden.
        //
        // The frame's `enabled` byte plays no part here: 0x423730 and the
        // flip-book step 0x423810 set texture, shade and texel offset of
        // every frame, and only the flag copies (0x42df50, 0x42e050) test
        // `enabled > 0`. Flip-books are mostly one enabled frame followed
        // by disabled `-2`/`-3` frames that just slide the offset (pad #49
        // of section 1: `(8, e1), (-3, e0, +4/+4) × 31`), so reading
        // "disabled" as "hidden" blanked them 31 ticks out of 32.
        let previous = frames[..index.min(frames.len())].iter().rev().find(|f| f.texture >= 0).map(|f| f.texture as f32);
        let layer = match frame {
            Some(f) if f.texture >= 0 => f.texture as f32,
            Some(f) if f.texture == -4 => GLASS,
            Some(_) => previous.unwrap_or_else(|| {
                let now = self.vertices.get(r.first as usize * FLOATS_PER_VERTEX + 5).copied().unwrap_or(-2.0);
                if r.count > 0 && now >= 0.0 { now } else { -2.0 }
            }),
            None => -1.0,
        };
        if let Some(h) = self.hides_object.get_mut(draw).and_then(|h| h.get_mut(poly)) {
            *h = frame.is_some_and(|f| f.texture == -5);
            self.refresh_visible(draw);
        }
        // The frame appliers keep the glass test of the polygon they set
        // last and hand it to the mesh's backface flag after the loop
        // (`0x4237e8`: `cull = glass == 0`), so only the last polygon
        // decides (see `culls_glass`).
        if poly + 1 == self.poly_ranges[draw].len() {
            self.cull[draw] = culls_glass(frame.is_some_and(|f| f.texture == -4)) as u8;
        }
        // A shade of -1 keeps the material (0x423730 calls 0x402450 only
        // for shade >= 0). Like -2/-3 textures, "kept" is resolved from the
        // list (the last earlier frame with a shade), so a restored
        // snapshot shows what the running world showed.
        let shade = frames[..index.min(frames.len())].iter().chain(frame).rev().find(|f| f.shade >= 0);
        if let (Some(f), Some(m)) = (shade, self.material.get_mut(draw).and_then(|m| m.get_mut(poly))) {
            *m = f.shade;
        }
        // The material goes to the shader, which lights the polygon with
        // its current normal (see `light_level`): a flip-book that changes
        // the shade (capsule #3's lamps: 35, 38, 27) blinks.
        let material = self.material[draw][poly] as f32;
        let (du, dv) = frame.map_or((0.0, 0.0), |f| (f.u as f32 / TEX, f.v as f32 / TEX));
        for v in r.first..r.first + r.count {
            let i = v as usize;
            let o = i * FLOATS_PER_VERTEX;
            self.uv_offset[i * 2] = du;
            self.uv_offset[i * 2 + 1] = dv;
            self.vertices[o + 3] = self.base_uv[i * 2] + du;
            self.vertices[o + 4] = self.base_uv[i * 2 + 1] + dv;
            self.vertices[o + 5] = layer;
            self.vertices[o + 6] = material;
        }
        Some(r)
    }
}

impl WorldMesh {
    /// Puts a shape frame on drawn object `draw` (slot 0x70 `0x423530`
    /// on a shape action, slot 0x74 `0x4235d0` per morph tick): the
    /// frame's vertices become the mesh's (`0x402280`) and its six floats
    /// per polygon the texels of vertices 0, 1, 2 (`+0x44..+0x5b`), from
    /// which the Direct3D renderer re-derives every vertex's `(u, v)`
    /// when the mesh is marked changed. Positions, normals and base UVs
    /// are rewritten in place; each polygon keeps its appearance frame's
    /// texel offset, layer and material. Returns the vertex range touched.
    pub fn set_shape(&mut self, draw: usize, polygons: &[Vec<u16>], vertices: &[[i16; 3]], texels: &[[f32; 6]]) -> Option<DrawRange> {
        let ranges = self.poly_ranges.get(draw)?;
        let (mut lo, mut hi) = (u32::MAX, 0);
        for (p, poly) in polygons.iter().enumerate() {
            let Some(&r) = ranges.get(p) else { break };
            let pts: Vec<V3> = poly.iter().filter_map(|&i| vertices.get(i as usize)).map(|v| v.map(|c| c as f32)).collect();
            if pts.len() < 3 || r.count == 0 {
                continue;
            }
            let n = normalize(cross(sub(pts[2], pts[1]), sub(pts[0], pts[1])));
            let uv = polygon_uvs(&pts, texels.get(p).copied().unwrap_or(DEFAULT_TEXELS));
            let mut v = r.first as usize;
            for tri in triangulate(&pts) {
                for i in tri {
                    if v >= (r.first + r.count) as usize {
                        break;
                    }
                    let o = v * FLOATS_PER_VERTEX;
                    // The appearance frame's offset rides on the new UVs.
                    self.base_uv[v * 2] = uv[i].0;
                    self.base_uv[v * 2 + 1] = uv[i].1;
                    self.vertices[o..o + 3].copy_from_slice(&pts[i]);
                    self.vertices[o + 3] = uv[i].0 + self.uv_offset[v * 2];
                    self.vertices[o + 4] = uv[i].1 + self.uv_offset[v * 2 + 1];
                    self.vertices[o + 7..o + 10].copy_from_slice(&n);
                    self.corner[v] = i as u16;
                    v += 1;
                }
            }
            lo = lo.min(r.first);
            hi = hi.max(r.first + r.count);
        }
        (lo < hi).then_some(DrawRange { first: lo, count: hi - lo })
    }
}

fn base_uv_len(vertices: &[f32]) -> usize {
    vertices.len() / FLOATS_PER_VERTEX * 2
}

fn obj_index(section: &Section, obj: &WorldObject) -> usize {
    section
        .objects
        .iter()
        .position(|o| matches!(o, Object::World(w) if core::ptr::eq(w, obj)))
        .unwrap()
}

fn emit_object(obj: &WorldObject, out: &mut Vec<f32>, corner: &mut Vec<u16>, bounds: &mut [f32; 6]) -> Vec<DrawRange> {
    let mut ranges = Vec::new();
    let mesh = obj.mesh.as_ref().unwrap();
    let origin = obj.position.map(|c| c as f32);
    let local: Vec<V3> = mesh.vertices.iter().map(|v| v.map(|c| c as f32)).collect();

    for poly in &mesh.polygons {
        let pts: Vec<V3> = poly.iter().filter_map(|&i| local.get(i as usize).copied()).collect();
        let first = (out.len() / FLOATS_PER_VERTEX) as u32;
        if pts.len() < 3 {
            ranges.push(DrawRange { first, count: 0 });
            continue;
        }
        // Layer and texel offset come from the appearance frame and are
        // written afterwards by `WorldMesh::apply_frame`; the texels are
        // the initialiser's until a shape frame replaces them.
        // (v2 − v1) × (v0 − v1), normalised: the exe's polygon normal
        // (0x4019a0). The material (`light`) is written by `apply_frame`.
        let [nx, ny, nz] = normalize(cross(sub(pts[2], pts[1]), sub(pts[0], pts[1])));
        let uv = polygon_uvs(&pts, DEFAULT_TEXELS);
        for tri in triangulate(&pts) {
            for i in tri {
                let p = pts[i];
                for a in 0..3 {
                    bounds[a] = bounds[a].min(origin[a] + p[a]);
                    bounds[a + 3] = bounds[a + 3].max(origin[a] + p[a]);
                }
                out.extend_from_slice(&[p[0], p[1], p[2], uv[i].0, uv[i].1, -1.0, 0.0, nx, ny, nz]);
                corner.push(i as u16);
            }
        }
        let count = (out.len() / FLOATS_PER_VERTEX) as u32 - first;
        ranges.push(DrawRange { first, count });
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use galapagos_formats::worlds::{Archive, PolyFrame};

    #[test]
    fn affine_maps_a_quad_to_unit_square() {
        // eu, ev span the polygon plane; the opposite corner is (1, 1).
        let (e1, e2) = ([10.0, 0.0, 0.0], [0.0, 0.0, 10.0]);
        assert_eq!(affine_st([10.0, 0.0, 10.0], e1, e2), (1.0, 1.0));
        assert_eq!(affine_st([0.0, 0.0, 10.0], e1, e2), (0.0, 1.0));
    }

    /// Prints per-section stats; run with `-- --nocapture` to see them.
    #[test]
    fn builds_every_section_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        for i in 0..archive.sections.len() {
            let m = WorldMesh::build(&archive.world(i).unwrap());
            let tris = m.vertices.len() / FLOATS_PER_VERTEX / 3;
            assert!(tris > 0, "section {i} has no geometry");
            println!(
                "section {i}: {} objects, {tris} triangles, {} textures, bounds {:?}",
                m.objects_drawn, m.texture_count, m.bounds
            );
        }
    }

    /// `-4` frames are glass (0x423791): the intro capsule #3 of section 0
    /// has twelve glass polygons (20..=31), drawn, not hidden.
    #[test]
    fn minus_four_is_glass_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        let m = WorldMesh::build(&archive.world(0).unwrap());
        let d = m.draw_of_object[3 - 1].unwrap();
        assert_eq!(m.visible[d], 1, "the capsule is drawn");
        let layer = |p: usize| m.vertices[m.poly_ranges[d][p].first as usize * FLOATS_PER_VERTEX + 5];
        for p in 20..=31 {
            assert_eq!(layer(p), GLASS, "capsule polygon {p} is glass");
        }
        assert!(layer(0) >= 0.0, "the capsule's frame stays textured");
        // Docked (material 49, base 0) the glass adds nothing; on the ride
        // (materials 17..=19) it is a grey veil.
        let material = |p: usize| m.vertices[m.poly_ranges[d][p].first as usize * FLOATS_PER_VERTEX + 6];
        assert_eq!(material(20), 49.0);
        let s = archive.world(0).unwrap();
        assert_eq!(glass_grey(s.color_ramps[49]), 0.0);
        let ride: Vec<f32> = (17..=19).map(|i| glass_grey(s.color_ramps[i])).collect();
        assert!(ride.iter().all(|&g| g > 0.1 && g < 0.25), "ride glass greys {ride:?}");
    }

    fn data() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds")).ok()
    }

    /// Twice the signed area of a triangle in the plane z = 0.
    fn area2(a: V3, b: V3, c: V3) -> f32 {
        (b[0] - a[0]) * (c[1] - a[1]) - (c[0] - a[0]) * (b[1] - a[1])
    }

    /// A square ring drawn as one polygon, the outer square and the inner
    /// one joined by a zero-width bridge, the way the artists drew section
    /// 0's save pad: a fan from vertex 0 covers the hole, `0x40be00`'s ear
    /// clipping leaves it open.
    #[test]
    fn rings_are_ear_clipped_not_fanned() {
        // Outer 0..1000 counter-clockwise from above, bridge at (0, 0) to
        // (300, 300), inner square clockwise.
        let pts: Vec<V3> = [
            (0., 0.), (1000., 0.), (1000., 1000.), (0., 1000.), (0., 0.),
            (300., 300.), (300., 700.), (700., 700.), (700., 300.), (300., 300.),
        ]
        .iter()
        .map(|&(x, y)| [x, y, 0.0])
        .collect();
        // Winding: the normal (v2 − v1) × (v0 − v1) points up (+z).
        assert!(cross(sub(pts[2], pts[1]), sub(pts[0], pts[1]))[2] > 0.0);
        let fan_covers_hole = (1..pts.len() - 1).any(|k| {
            let t = [pts[0], pts[k], pts[k + 1]];
            let c = [600.0, 400.0, 0.0];
            area2(t[0], t[1], c) > 0.0 && area2(t[1], t[2], c) > 0.0 && area2(t[2], t[0], c) > 0.0
        });
        assert!(fan_covers_hole, "the fan the port used to draw fills the hole");
        let tris = triangulate(&pts);
        assert_eq!(tris.len(), pts.len() - 2);
        let total: f32 = tris.iter().map(|t| area2(pts[t[0]], pts[t[1]], pts[t[2]])).sum::<f32>() / 2.0;
        assert_eq!(total, 1000.0 * 1000.0 - 400.0 * 400.0, "exactly the ring");
        for t in &tris {
            assert!(area2(pts[t[0]], pts[t[1]], pts[t[2]]) >= 0.0, "{t:?} keeps the polygon's winding");
        }
        // A convex polygon is still fanned.
        let quad: Vec<V3> = vec![[0., 0., 0.], [10., 0., 0.], [10., 10., 0.], [0., 10., 0.]];
        assert_eq!(triangulate(&quad), vec![[0, 1, 2], [0, 2, 3]]);
    }

    /// The save pad of section 0 (#84, the invisible `0x22` hexagon) sits on
    /// #87: a ring (polygon 5, texture 1, the dark circuit board) around a
    /// pool 37 units lower (polygon 7, texture 21, the blue clouds) whose
    /// flip-book slides the texture by (+4, +4) texels a tick (32 frames,
    /// looping). The footage shows the blue pool shimmering inside its
    /// frame; fanned, the ring hid it under a grey lid. Ear-clipped, the
    /// ring leaves the pool's centre open and the pool scrolls.
    #[test]
    fn save_pad_ring_shows_the_shimmering_pool_when_data_present() {
        let Some(d) = data() else { return };
        let s = Archive::parse(&d).unwrap().world(0).unwrap();
        let mut m = WorldMesh::build(&s);
        let mut sim = crate::sim::Sim::new(&s, &mut m);
        let draw = m.draw_of_object[87 - 1].unwrap();
        let vert = |m: &WorldMesh, i: u32| -> [f32; 10] {
            let o = i as usize * FLOATS_PER_VERTEX;
            m.vertices[o..o + FLOATS_PER_VERTEX].try_into().unwrap()
        };
        let ring = m.poly_ranges[draw][5];
        assert_eq!(ring.count, 12 * 3, "14 vertices, 12 triangles");
        assert_eq!(vert(&m, ring.first)[5], 1.0, "the ring is texture 1");
        // The pool's centre, object-local.
        let c = [0.0, 185.0, 0.0];
        let mut area = 0.0;
        for t in 0..12 {
            let p: Vec<V3> = (0..3).map(|k| vert(&m, ring.first + t * 3 + k)).map(|v| [v[0], v[1], 0.0]).collect();
            let inside = area2(p[0], p[1], c) > 0.0 && area2(p[1], p[2], c) > 0.0 && area2(p[2], p[0], c) > 0.0;
            assert!(!inside, "ring triangle {t} covers the pool");
            area += area2(p[0], p[1], p[2]) / 2.0;
        }
        // Outer hexagon minus inner hexagon (shoelace over the mesh).
        let Object::World(w) = &s.objects[87 - 1] else { panic!() };
        let mesh = w.mesh.as_ref().unwrap();
        let shoelace = |poly: &[u16]| -> f32 {
            let q: Vec<[f32; 3]> = poly.iter().map(|&i| mesh.vertices[i as usize].map(|c| c as f32)).collect();
            (0..q.len()).map(|i| area2([0.0; 3], q[i], q[(i + 1) % q.len()])).sum::<f32>() / 2.0
        };
        assert!((area - shoelace(&mesh.polygons[5])).abs() < 1.0, "ring area {area}");
        assert!((area - (shoelace(&mesh.polygons[5][8..]).abs() - shoelace(&mesh.polygons[7]).abs())).abs() < 1.0, "outer hexagon minus the pool");
        // Still a save pad: a click straight down on it hits #84's `0x22`
        // hexagon (the pick, `World_Pick` @ 0x4089e0, is not the renderer's
        // triangulation).
        let hit = sim.pick_hit([-1110.0, 1110.0, 5000.0], [0.0, 0.0, -1.0]);
        assert!(hit.is_some_and(|(id, flags)| id == 84 && flags & 0x20 != 0), "{hit:?}");
        // The pool: texture 21, its offset sliding 4 texels a tick on both axes.
        let pool = m.poly_ranges[draw][7];
        assert_eq!(vert(&m, pool.first)[5], 21.0, "the pool is texture 21");
        let uv = |m: &WorldMesh| {
            let v = vert(m, pool.first);
            (v[3], v[4])
        };
        // The pad lives in folder #81 < #80 < #74 (97 objects, the second
        // room), which switches its subtree's updates off on tick 2 while
        // Mendel is elsewhere (`NoEvent(1)`, capsule #3 and #72 in state 0)
        // and back on as soon as he touches it: the flip-book only runs
        // while he is around, as every object's does. Keep it awake here.
        sim.wake_all = true;
        let mut prev = uv(&m);
        for _ in 0..40 {
            sim.step(&mut m);
            let now = uv(&m);
            assert_eq!(vert(&m, pool.first)[5], 21.0);
            // Frame 31 → 0 jumps back 124 texels: the same picture, as the
            // offset counts modulo one texture (128 texels).
            let step = (((now.0 - prev.0) * 128.0).rem_euclid(128.0), ((now.1 - prev.1) * 128.0).rem_euclid(128.0));
            assert!((step.0 - 4.0).abs() < 1e-2 && (step.1 - 4.0).abs() < 1e-2, "pool slides (+4, +4) a tick, not {step:?}");
            prev = now;
        }
    }

    /// Over the whole CD, the triangles cover each polygon exactly
    /// (their areas add up to the polygon's own, measured in its plane),
    /// and 816 polygons (in every section) need the ear clipper.
    #[test]
    fn triangles_tile_every_polygon_when_data_present() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let mut clipped = Vec::new();
        for si in 0..a.sections.len() {
            let s = a.world(si).unwrap();
            for (i, o) in s.objects.iter().enumerate() {
                let Object::World(w) = o else { continue };
                let Some(mesh) = &w.mesh else { continue };
                for (pi, poly) in mesh.polygons.iter().enumerate() {
                    let pts: Vec<V3> = poly.iter().filter_map(|&k| mesh.vertices.get(k as usize)).map(|v| v.map(|c| c as f32)).collect();
                    if pts.len() < 4 {
                        continue;
                    }
                    let tris = triangulate(&pts);
                    assert_eq!(tris.len(), pts.len() - 2);
                    if tris.iter().enumerate().any(|(k, t)| *t != [0, k + 1, k + 2]) {
                        let Some(xy) = plane_xy(&pts) else { continue };
                        let p: Vec<V3> = xy.iter().map(|&(x, y)| [x as f32, y as f32, 0.0]).collect();
                        let sum: f32 = tris.iter().map(|t| area2(p[t[0]], p[t[1]], p[t[2]]).abs()).sum::<f32>() / 2.0;
                        let own = (0..p.len()).map(|k| area2([0.0; 3], p[k], p[(k + 1) % p.len()])).sum::<f32>().abs() / 2.0;
                        assert!((sum - own).abs() <= own * 1e-3 + 1.0, "section {si} #{} polygon {pi}: {sum} vs {own}", i + 1);
                        clipped.push((si, i + 1, pi, pts.len()));
                    }
                }
            }
        }
        println!("ear-clipped polygons: {clipped:?}");
        assert!(clipped.contains(&(0, 87, 5, 14)), "section 0's save-pad ring");
    }

    /// Does one of `tris` (of the planar polygon `pts`) cover `p`?
    fn covers(pts: &[V3], tris: &[[usize; 3]], p: V3) -> bool {
        let n = normalize(cross(sub(pts[2], pts[1]), sub(pts[0], pts[1])));
        tris.iter().any(|&[a, b, c]| {
            let side = |u: V3, v: V3| dot(cross(sub(v, u), sub(p, u)), n);
            side(pts[a], pts[b]) >= 0.0 && side(pts[b], pts[c]) >= 0.0 && side(pts[c], pts[a]) >= 0.0
        })
    }

    /// Walls have holes too. Section 0's octagonal hall wall #100
    /// (polygon 2, texture 2, flags 0x340: drawn and solid) is one keyhole
    /// outline round a window, `[12, 13, 9, 0, 3, 2, 1, 0, 9, 8]`, and the
    /// window's grey frame (polygon 0, texture 27) is another round the
    /// same opening (x −2220..0, z 2960..5920 at y = −15170), where the
    /// walkway #94 meets the corridor #79. A fan bricked it up; the ear
    /// clipper leaves it open and keeps the wall around it.
    #[test]
    fn section0_hall_window_is_open_when_data_present() {
        let Some(d) = data() else { return };
        let ar = Archive::parse(&d).unwrap();
        let sec = ar.world(0).unwrap();
        let Object::World(w) = &sec.objects[100 - 1] else { panic!() };
        let m = w.mesh.as_ref().unwrap();
        let local = |p: [i32; 3]| [0, 1, 2].map(|a| (p[a] - w.position[a]) as f32);
        let pts = |p: usize| m.polygons[p].iter().map(|&i| m.vertices[i as usize].map(|c| c as f32)).collect::<Vec<V3>>();
        let (wall, frame) = (pts(2), pts(0));
        let window = local([-1110, -15170, 4440]);
        let fan: Vec<[usize; 3]> = (1..wall.len() - 1).map(|k| [0, k, k + 1]).collect();
        assert!(covers(&wall, &fan, window), "a fan fills the window");
        assert!(!covers(&wall, &triangulate(&wall), window), "the wall leaves the window open");
        assert!(!covers(&frame, &triangulate(&frame), window), "and so does its frame");
        assert!(covers(&wall, &triangulate(&wall), local([-5000, -15170, 0])), "the wall is still there");
        assert!(covers(&frame, &triangulate(&frame), local([-1800, -15170, 6300])), "the frame too");
    }

    /// The winding the renderer culls by: the top of section 0's arrival
    /// pad #83 (polygon 9, z = 5180, the floor Mendel lands on) has the
    /// exe's normal `(v2 − v1) × (v0 − v1)` pointing up, and every fan
    /// triangle we emit for it turns counter-clockwise seen from above,
    /// so WebGL's front faces are the exe's: floors stay drawn from above
    /// and vanish from below, as in the 1997 game.
    #[test]
    fn floors_face_up_and_wind_counter_clockwise_from_above() {
        let Some(d) = data() else { return };
        let m = WorldMesh::build(&Archive::parse(&d).unwrap().world(0).unwrap());
        let draw = m.draw_of_object[83 - 1].unwrap();
        let r = m.poly_ranges[draw][9];
        assert!(r.count >= 3);
        let v = |i: u32| {
            let o = i as usize * FLOATS_PER_VERTEX;
            [m.vertices[o], m.vertices[o + 1], m.vertices[o + 2]]
        };
        for t in (r.first..r.first + r.count).step_by(3) {
            let n = [m.vertices[t as usize * FLOATS_PER_VERTEX + 7], m.vertices[t as usize * FLOATS_PER_VERTEX + 8], m.vertices[t as usize * FLOATS_PER_VERTEX + 9]];
            assert!(n[2] > 0.99, "the pad's top faces +z: {n:?}");
            let w = normalize(cross(sub(v(t + 1), v(t)), sub(v(t + 2), v(t))));
            assert!(dot(w, n) > 0.99, "triangle {t} winds counter-clockwise around the normal: {w:?}");
        }
    }

    /// The white light panels around the birth tube (section 0, objects
    /// #60, #61, #63, #65): texture 9 (a white tile cut in four by a dark
    /// cross), material 41, texel offset (−66, −1224). Material 41 reaches
    /// the middle row whatever the light (level 15: the plain texture, no
    /// specular wash, `0x40c330`). The polygon initialiser's texels
    /// ((0,127), (0,0), (127,0) at the first three vertices, `0x42320d`)
    /// would put *one* copy on the whole wall; but each panel's initial
    /// state selects shape 1, whose frame gives the polygon its real
    /// texels (`0x423530` → polygon `+0x44`), and the wall then carries a
    /// grid of about 4 × 3 copies, some 25 units a texel: the grid of white
    /// squares the 1997 footage shows (0:10, 0:20).
    #[test]
    fn birth_room_light_panels_are_a_grid_of_texture_9_when_data_present() {
        let Some(d) = data() else { return };
        let s = Archive::parse(&d).unwrap().world(0).unwrap();
        let mut m = WorldMesh::build(&s);
        let span = |m: &WorldMesh, id: usize, poly: usize| -> ([f32; 2], f32) {
            let r = m.poly_ranges[m.draw_of_object[id - 1].unwrap()][poly];
            let v: Vec<[f32; 5]> = (r.first..r.first + r.count)
                .map(|i| {
                    let o = i as usize * FLOATS_PER_VERTEX;
                    [m.vertices[o], m.vertices[o + 1], m.vertices[o + 2], m.vertices[o + 3], m.vertices[o + 4]]
                })
                .collect();
            let range = |k: usize| v.iter().map(|t| t[k]).fold(f32::MIN, f32::max) - v.iter().map(|t| t[k]).fold(f32::MAX, f32::min);
            // World units per texel along the longest edge direction.
            let size = (0..3).map(range).fold(0.0, f32::max);
            let tex = range(3).max(range(4)) * 128.0;
            ([range(3), range(4)], size / tex)
        };
        let before: Vec<_> = [(60, 4), (61, 4), (63, 3), (65, 3)].iter().map(|&(id, p)| span(&m, id, p).0).collect();
        let _sim = crate::sim::Sim::new(&s, &mut m);
        for (k, (id, poly)) in [(60, 4), (61, 4), (63, 3), (65, 3)].into_iter().enumerate() {
            let Object::World(w) = &s.objects[id - 1] else { panic!("#{id} is a world object") };
            let f = w.appearance.as_ref().unwrap()[0].polygons[poly][0];
            assert_eq!((f.texture, f.shade, f.u, f.v), (9, 41, -66, -1224), "#{id}");
            let mat = material(s.color_ramps[41]);
            for n in [[0.0, 0.0, 1.0], [0.0, 0.0, -1.0], [1.0, 0.0, 0.0]] {
                assert_eq!(light_level(mat, &m.light, n), 15, "material 41 is the plain texture");
            }
            assert_eq!(d3d_colour(15, false), (255, 0), "full diffuse, no specular");
            for c in before[k] {
                assert!((c - 127.0 / 128.0).abs() < 1e-3, "#{id}: the initialiser's single copy before the load pass ({c})");
            }
            let (copies, units_per_texel) = span(&m, id, poly);
            println!("#{id}: {copies:?} textures across, {units_per_texel:.1} units a texel");
            assert!(copies.iter().all(|&c| c > 2.5), "#{id}: a grid of copies, not one: {copies:?}");
            assert!((20.0..30.0).contains(&units_per_texel), "#{id}: about 25 units a texel, not {units_per_texel}");
        }
    }

    /// The shape texels give the world one texel density. For every
    /// textured polygon of the CD, world units per texel =
    /// `sqrt(world area / texel area)` of the triangle of its first three
    /// vertices: with the initialiser's fixed 127-texel triangle (the
    /// port before shape frames) it is whatever size the polygon has: 966
    /// of 8,512 polygons above 64 units a texel, the 90th percentile at 73;
    /// with each object's shape frame, 98 that coarse, 35 % at 16..23 units
    /// and 90 % below 35: the scale the 1997 footage shows (about 16 units
    /// a texel, measured on its frames).
    #[test]
    fn shape_texels_give_one_texel_density_when_data_present() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let (mut fixed, mut shaped) = (Vec::new(), Vec::new());
        for i in 0..a.sections.len() {
            let s = a.world(i).unwrap();
            for o in &s.objects {
                let Object::World(w) = o else { continue };
                let (Some(m), Some(sh)) = (&w.mesh, &w.logic.shape) else { continue };
                let f = &sh.states[0].frames[0];
                for (p, poly) in m.polygons.iter().enumerate() {
                    if poly.len() < 3 || w.polygon_texture(p).is_none_or(|t| t < 0) {
                        continue;
                    }
                    let v = |k: usize| f.vertices[poly[k] as usize].map(|c| c as f32);
                    let area = |a: V3, b: V3| { let c = cross(a, b); dot(c, c).sqrt() };
                    let world = area(sub(v(2), v(1)), sub(v(0), v(1)));
                    let t = f.polygon_mapping[p];
                    let tex = ((t[4] - t[2]) * (t[1] - t[3]) - (t[5] - t[3]) * (t[0] - t[2])).abs();
                    if world > 0.0 && tex > 0.0 {
                        fixed.push((world / (127.0 * 127.0)).sqrt());
                        shaped.push((world / tex).sqrt());
                    }
                }
            }
        }
        let spread = |v: &mut Vec<f32>| {
            v.sort_by(f32::total_cmp);
            let q = |x: f32| v[((v.len() - 1) as f32 * x) as usize];
            (q(0.1), q(0.5), q(0.9))
        };
        let (f, s) = (spread(&mut fixed), spread(&mut shaped));
        println!("{} polygons; units per texel (10%, median, 90%): initialiser {f:?}, shape frames {s:?}", fixed.len());
        let coarse = |v: &[f32]| v.iter().filter(|&&x| x > 64.0).count();
        let peak = |v: &[f32]| v.iter().filter(|&&x| (16.0..23.0).contains(&x)).count() as f32 / v.len() as f32;
        println!("over 64 units a texel: {} vs {}; 16..23 units: {:.0} % vs {:.0} %", coarse(&fixed), coarse(&shaped), 100.0 * peak(&fixed), 100.0 * peak(&shaped));
        assert!(coarse(&fixed) > 500 && coarse(&shaped) * 5 < coarse(&fixed), "big polygons are no longer one stretched copy");
        assert!(f.2 / f.0 > 10.0 && s.2 / s.0 < 7.0, "the fixed texels follow the polygon's size: {f:?} vs {s:?}");
        assert!(s.2 < 40.0 && peak(&shaped) > 0.3, "the shape texels peak at 16..23 units: {s:?}");
        assert!((10.0..40.0).contains(&s.1), "about 16 to 25 units a texel: {s:?}");
    }

    /// The backface flag of the glass (`culls_glass`): the intro capsule
    /// #3 ends with glass (polygons 20..=31), so its veil is seen from
    /// inside and outside; the arrival pad #83 has none and is culled.
    #[test]
    fn glass_culling_follows_the_last_polygon() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let m = WorldMesh::build(&a.world(0).unwrap());
        assert_eq!(m.cull[m.draw_of_object[3 - 1].unwrap()], 0, "capsule: two-sided glass");
        assert_eq!(m.cull[m.draw_of_object[83 - 1].unwrap()], 1, "pad: culled");
        for s in 0..a.sections.len() {
            let m = WorldMesh::build(&a.world(s).unwrap());
            let two_sided = m.cull.iter().filter(|&&c| c == 0).count();
            println!("section {s}: {two_sided} of {} objects end with glass", m.cull.len());
        }
    }

    /// `0x401b70` on made-up materials: the directional term only counts
    /// for faces turned against the light, the middle row caps the base,
    /// `m1` pushes past it.
    #[test]
    fn light_level_follows_0x401b70() {
        let light = Light { dir: [0.0, 0.0, -1.0], intensity: 1.0, ambient: 0.0 };
        let (up, down) = ([0.0, 0.0, 1.0], [0.0, 0.0, -1.0]);
        let lit = [0.35, 0.0, 1.0, 1.0, 0.0, 1.0];
        assert_eq!(light_level(lit, &light, down), 5, "0.35 / 2 · 31, unlit from behind");
        assert_eq!(light_level(lit, &light, up), 15, "0.35 + 1 caps at the middle row");
        let side = normalize([1.0, 0.0, 1.0]);
        assert_eq!(light_level(lit, &light, side), 15, "(0.35 + 0.71) / 2 is past the middle row too");
        let whiter = [1.0, 0.23, 0.0, 0.0, 0.0, 0.0];
        assert_eq!(light_level(whiter, &light, up), 22, "(0.5 + 0.23) · 31");
        assert_eq!(light_level([1.0, 1.0, 0.0, 0.0, 0.0, 0.0], &light, up), 31, "t is at most 1");
        let ambient = Light { ambient: 1.0, ..light };
        assert_eq!(light_level([0.0, 0.0, 0.5, 0.0, 0.0, 0.0], &ambient, down), 7, "m2 · ambient");
    }

    /// `0x40c3e2`: the lower half of the rows darkens, the upper half adds
    /// white; glass is half the diffuse grey. And `0x40c4d3`'s fog.
    #[test]
    fn direct3d_colours_and_fog() {
        assert_eq!(d3d_colour(0, false), (0, 0));
        assert_eq!(d3d_colour(5, false), (85, 0));
        assert_eq!(d3d_colour(15, false), (255, 0), "row 15 is already full: level / 15");
        assert_eq!(d3d_colour(16, false), (255, 0));
        assert_eq!(d3d_colour(22, false), (255, 95));
        assert_eq!(d3d_colour(31, false), (255, 239));
        assert_eq!(d3d_colour(15, true), (127, 0));
        assert_eq!(d3d_colour(22, true), (127, 0), "glass gets no highlight");
        assert_eq!(fog_factor(1.0, 0.0), 255);
        assert_eq!(fog_factor(1.0, 12000.0), 127);
        assert_eq!(fog_factor(1.0, 99999.0), 0, "depth is clamped to 24000");
        assert_eq!(fog_factor(0.0, 99999.0), 255, "m5 = 0: no fog");
        assert_eq!(fog_factor(0.5, 24000.0), 127);
    }

    /// The section's lights and materials, as the shader gets them; run
    /// with `-- --nocapture` to see them.
    #[test]
    fn materials_when_data_present() {
        let Some(data) = data() else { return };
        let archive = Archive::parse(&data).unwrap();
        for i in 0..archive.sections.len() {
            let s = archive.world(i).unwrap();
            let m = WorldMesh::build(&s);
            let e = &s.environment;
            println!(
                "section {i}: light {:?} (×{:.3}), ambient {}, fog field {}, background {:?}, far {}",
                m.light.dir, m.light.intensity, m.light.ambient, e.fog_enabled, e.background, e.far_clip
            );
            assert_eq!(m.light.ambient, 0.0, "no section has an ambient light");
            assert_eq!(m.materials.len(), s.color_ramps.len() * FLOATS_PER_MATERIAL);
            if i == 0 {
                for (k, r) in s.color_ramps.iter().enumerate() {
                    let up = light_level(material(*r), &m.light, [0.0, 0.0, 1.0]);
                    let off = light_level(material(*r), &m.light, [0.0, 0.0, -1.0]);
                    println!("  material {k:2}: {:?}  level facing up {up:2}, turned away {off:2}", material(*r).map(|x| (x * 1000.0).round() / 1000.0));
                }
            }
        }
    }

    /// A flip-book that changes the shade changes the light: capsule #3's
    /// polygon 2 cycles materials 35, 38, 27 in its ride state; the vertex
    /// `light` follows, and so does the level the shader computes.
    #[test]
    fn flip_book_shades_animate_the_light_when_data_present() {
        let Some(data) = data() else { return };
        let archive = Archive::parse(&data).unwrap();
        let s = archive.world(0).unwrap();
        let mut m = WorldMesh::build(&s);
        let Some(Object::World(capsule)) = s.objects.get(3 - 1) else { panic!("object #3") };
        let d = m.draw_of_object[3 - 1].unwrap();
        let states = capsule.appearance.as_ref().unwrap();
        let (n, frames) = states
            .iter()
            .enumerate()
            .find_map(|(n, st)| st.polygons.get(2).filter(|f| f.len() >= 3).map(|f| (n, f.clone())))
            .expect("a flip-book on polygon 2");
        let shades: Vec<i16> = frames.iter().map(|f| f.shade).collect();
        println!("capsule polygon 2, state {n}: shades {shades:?}");
        let at = |m: &WorldMesh| m.poly_ranges[d][2].first as usize * FLOATS_PER_VERTEX;
        let o = at(&m);
        let normal = [m.vertices[o + 7], m.vertices[o + 8], m.vertices[o + 9]];
        let mut levels = Vec::new();
        for (k, f) in frames.iter().enumerate() {
            m.apply_frame(d, 2, &frames, k);
            if f.shade >= 0 {
                assert_eq!(m.vertices[o + 6], f.shade as f32, "frame {k}");
            }
            let mat = m.vertices[o + 6] as usize;
            levels.push(light_level(material(s.color_ramps[mat]), &m.light, normal));
        }
        println!("  levels {levels:?}");
        assert!(levels.iter().any(|&l| l != levels[0]), "the lamp blinks: {levels:?}");
        // `-1` keeps the current material (0x423730).
        let keep = [PolyFrame { shade: -1, ..frames[0] }];
        m.apply_frame(d, 2, &frames, 1);
        let before = m.vertices[o + 6];
        m.apply_frame(d, 2, &keep, 0);
        assert_eq!(m.vertices[o + 6], before);
    }

    /// The light of a few known surfaces of section 0, facing up and lit
    /// by the section's light (the start platform's floor and the capsule).
    #[test]
    fn known_levels_when_data_present() {
        let Some(data) = data() else { return };
        let archive = Archive::parse(&data).unwrap();
        let s = archive.world(0).unwrap();
        let m = WorldMesh::build(&s);
        let mut histogram = [0u32; LEVELS as usize];
        for (d, polys) in m.poly_ranges.iter().enumerate() {
            for r in polys.iter().filter(|r| r.count > 0) {
                let o = r.first as usize * FLOATS_PER_VERTEX;
                if m.vertices[o + 5] < -1.5 && m.vertices[o + 5] != GLASS {
                    continue;
                }
                let n = [m.vertices[o + 7], m.vertices[o + 8], m.vertices[o + 9]];
                let mat = m.vertices[o + 6] as usize;
                let level = light_level(material(s.color_ramps[mat]), &m.light, n);
                assert!((0..LEVELS).contains(&level), "draw {d}: level {level}");
                histogram[level as usize] += 1;
            }
        }
        println!("section 0 polygons per level: {histogram:?}");
        assert!(histogram[..15].iter().sum::<u32>() > 0, "some surfaces are darker than full");
    }

    /// A `-5` frame hides the whole object (0x423730): section 0's start pad
    /// #57 and birth tube #54 are never drawn, though Mendel stands in them.
    #[test]
    fn minus_five_hides_whole_objects_when_data_present() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        for s in 0..archive.sections.len() {
            let m = WorldMesh::build(&archive.world(s).unwrap());
            let hidden = m.visible.iter().filter(|&&v| v == 0).count();
            println!("section {s}: {hidden} of {} objects hidden by -5 frames", m.visible.len());
            if s == 0 {
                for id in [57usize, 54] {
                    let d = m.draw_of_object[id - 1].unwrap();
                    assert_eq!(m.visible[d], 0, "object #{id} must be hidden");
                }
            }
        }
    }
}
