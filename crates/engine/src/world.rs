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
//! * Polygons on disk are convex n-gons; we fan them into triangles.
//! * There are no texture coordinates on disk. The engine's polygon
//!   initialiser (0x42320d) gives the first three vertices the texel
//!   coordinates (0,127), (0,0), (127,0): v1 is the origin, u runs along
//!   v1→v2 and v runs along v1→v0. That defines an affine map over the
//!   polygon's plane; a quad's fourth vertex lands on (127,127).
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
                poly_ranges.push(emit_object(obj, &mut vertices, &mut bounds));
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
        let hides_object = poly_ranges.iter().map(|p| vec![false; p.len()]).collect();
        let visible = vec![1; draws.len()];
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
            vertices, draws, draw_of_object, poly_ranges, hides_object, blinking, visible, material, materials, light,
            base_uv, transforms, textures, texture_count, bounds, objects_drawn, far_clip, fog,
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
            self.vertices[o + 3] = self.base_uv[i * 2] + du;
            self.vertices[o + 4] = self.base_uv[i * 2 + 1] + dv;
            self.vertices[o + 5] = layer;
            self.vertices[o + 6] = material;
        }
        Some(r)
    }
}

fn obj_index(section: &Section, obj: &WorldObject) -> usize {
    section
        .objects
        .iter()
        .position(|o| matches!(o, Object::World(w) if core::ptr::eq(w, obj)))
        .unwrap()
}

fn emit_object(obj: &WorldObject, out: &mut Vec<f32>, bounds: &mut [f32; 6]) -> Vec<DrawRange> {
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
        // written afterwards by `WorldMesh::apply_frame`.
        // Texel axes from the first three vertices, as the original does.
        let eu = sub(pts[2], pts[1]);
        let ev = sub(pts[0], pts[1]);
        // (v2 − v1) × (v0 − v1), normalised: the exe's polygon normal
        // (0x4019a0). The material (`light`) is written by `apply_frame`.
        let [nx, ny, nz] = normalize(cross(eu, ev));

        const EDGE: f32 = 127.0 / TEX; // (0,127) is the last texel, not 128
        let uv: Vec<(f32, f32)> = pts
            .iter()
            .map(|&p| {
                let (s, t) = affine_st(sub(p, pts[1]), eu, ev);
                (s * EDGE, t * EDGE)
            })
            .collect();
        for k in 1..pts.len() - 1 {
            for &i in &[0, k, k + 1] {
                let p = pts[i];
                for a in 0..3 {
                    bounds[a] = bounds[a].min(origin[a] + p[a]);
                    bounds[a + 3] = bounds[a + 3].max(origin[a] + p[a]);
                }
                out.extend_from_slice(&[p[0], p[1], p[2], uv[i].0, uv[i].1, -1.0, 0.0, nx, ny, nz]);
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
