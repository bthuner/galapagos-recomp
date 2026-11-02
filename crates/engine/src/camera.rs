//! The original game camera: a port of `Camera_Update` @ 0x412d50 and the
//! three classes it is built from (docs/re/objects.md §6).
//!
//! The designers did not script camera paths. They gave **every polygon of
//! every object a camera key** (`Cmra` record): when Mendel steps onto a
//! polygon whose key is set, that key becomes the camera's destination.
//! A key is a position (absolute, or relative to the object or to Mendel,
//! depending on its *style*) and an orientation of which only the roll is
//! used: the camera always looks at Mendel.
//!
//! Getting there is a three-stage pipeline, ticked after the creature
//! (`Game_Tick` @ 0x41bc50: objects → creature → camera):
//!
//! 1. a **mover** (0x427780) travels from where the camera is to the key
//!    with a trapezoidal speed profile: accelerate by `accel` up to `max`
//!    units per tick, brake so as to stop on the key (style `limits`);
//! 2. the mover's point, made absolute, goes through a per-axis
//!    **alpha-beta filter** (0x406a50) whose single parameter `d` (style
//!    `damping[1]`) gives α = 1 − d², β = (1 − d)², a critically damped
//!    tracker: the sweeping, slightly lagging motion of the footage;
//! 3. the look-at point (Mendel) goes through a second filter (`damping[0]`).
//!
//! The orientation is then rebuilt from eye → target, with a roll measured
//! in the frame of the polygon Mendel stands on (z = its normal), so the
//! camera stays upright on floors and rolls with him on walls.
//!
//! Everything is integer, like the original: positions in world units,
//! angles in 1024ths of a turn, matrices in 4.12 fixed point.

use crate::creature::{After, Mode};
use crate::sim::{self, Sim};
use galapagos_formats::logic::CameraStyle;
use galapagos_formats::worlds::{CameraKey, Object, Section};

type I3 = [i32; 3];
/// A 3×3 matrix, row-major, 4096 = 1.0 (the engine's `short[9]`).
type M3 = [i32; 9];

// ——— the fixed-point math library (0x430ab0 .. 0x431b50) ———————————————

/// The engine's tables, filled at start-up by `Math_InitTables` @ 0x4308c0:
/// sine and cosine × 16384 over 1024 steps, and an arctangent table
/// (atan(i/1024) in 1024ths of a turn). Rounding is half-up.
pub(crate) struct Trig {
    pub(crate) sin: Vec<i32>,
    pub(crate) cos: Vec<i32>,
    atan: Vec<i32>,
}

fn round_half_up(x: f64) -> i32 {
    let f = x.floor();
    (if x - f < 0.5 { f } else { f + 1.0 }) as i32
}

impl Trig {
    pub(crate) fn new() -> Self {
        let tau = core::f64::consts::TAU;
        Trig {
            sin: (0..1024).map(|i| round_half_up((i as f64 / 1024.0 * tau).sin() * 16384.0)).collect(),
            cos: (0..1024).map(|i| round_half_up((i as f64 / 1024.0 * tau).cos() * 16384.0)).collect(),
            atan: (0..=1024).map(|i| round_half_up((i as f64 / 1024.0).atan() * 1024.0 / tau)).collect(),
        }
    }

    /// `Math_Angle2` @ 0x430ab0: the angle of (x, y), 0 along +x, 0x100
    /// along +y, in [0, 0x400]. (0, 0) gives 0.
    pub(crate) fn angle(&self, x: i32, y: i32) -> i32 {
        let t = |i: i32| self.atan[i.clamp(0, 1024) as usize];
        if x >= 1 && y >= 0 {
            return if y < x { t((y << 10) / x) } else { 0x100 - t((x << 10) / y) };
        }
        if x < 1 {
            if y > 0 {
                return if -x < y { t((x * -0x400) / y) + 0x100 } else { 0x200 - t(-((y << 10) / x)) };
            }
            if x < 0 {
                // here y <= 0
                return if y != x && -y <= -x { t((y << 10) / x) + 0x200 } else { 0x300 - t((x << 10) / y) };
            }
        }
        if y < 0 {
            return if -x != y && x <= -y { t(-((x << 10) / y)) + 0x300 } else { 0x400 - t((y * -0x400) / x) };
        }
        x
    }

    /// `Math_VectorAngles` @ 0x430c50: (yaw, pitch) of a direction.
    pub(crate) fn dir_angles(&self, v: I3) -> (i32, i32) {
        let (x, y) = if v[0] == 0 && v[1] == 0 { (0, 0x400) } else { (v[0], v[1]) };
        let yaw = self.angle(x, y) + 0x300;
        let u = (-(yaw & 0x3ff)) & 0x3ff;
        let h = ((self.sin[u as usize] as i64 * v[0] as i64 + self.cos[u as usize] as i64 * v[1] as i64) >> 14) as i32;
        let (z, h) = if v[2] == 0 && h == 0 { (0, 0x400) } else { (v[2], h) };
        (yaw, 0x500 - self.angle(z, h))
    }

    fn sc(&self, a: i32) -> (i32, i32) {
        let u = ((-(a & 0x3ff)) & 0x3ff) as usize;
        (self.sin[u], self.cos[u])
    }

    /// `Math_RotationMatrix` @ 0x430ce0: the view rotation for (yaw, pitch,
    /// roll). Rows are the camera's right, forward and up axes in world
    /// coordinates (world → camera when applied as `M · v`).
    fn rot(&self, yaw: i32, pitch: i32, roll: i32) -> M3 {
        let (s5, c7) = self.sc(yaw);
        let (s4, c2) = self.sc(pitch);
        let (s3, c6) = self.sc(roll);
        let h = |a: i32, b: i32| (a * b) >> 16;
        let t = |a: i32, b: i32, c: i32| (((a * b) >> 14) * c) >> 16;
        [
            h(c6, c7) - t(s4, s5, s3),
            -(t(s4, c7, s3) + h(c6, s5)),
            -h(s3, c2),
            h(c2, s5),
            h(c2, c7),
            -s4 >> 2,
            t(c6, s5, s4) + h(s3, c7),
            t(c6, c7, s4) - h(s3, s5),
            h(c6, c2),
        ]
    }

    /// `Math_RotationMatrixT` @ 0x430e40: the transpose of [`Trig::rot`].
    fn rot_t(&self, yaw: i32, pitch: i32, roll: i32) -> M3 {
        let (s2, c5) = self.sc(yaw);
        let (s7, c3) = self.sc(pitch);
        let (s4, c6) = self.sc(roll);
        let h = |a: i32, b: i32| (a * b) >> 16;
        let t = |a: i32, b: i32, c: i32| (((a * b) >> 14) * c) >> 16;
        [
            h(c6, c5) - t(s7, s2, s4),
            h(c3, s2),
            t(c6, s2, s7) + h(s4, c5),
            -(t(s7, c5, s4) + h(c6, s2)),
            h(c3, c5),
            t(c6, c5, s7) - h(s4, s2),
            -h(s4, c3),
            -s7 >> 2,
            h(c6, c3),
        ]
    }
}

/// `Math_MatMul` @ 0x431b50: A · B, results truncated to 16 bits.
fn mat_mul(a: &M3, b: &M3) -> M3 {
    let mut o = [0; 9];
    for i in 0..3 {
        for j in 0..3 {
            o[i * 3 + j] = ((a[i * 3] * b[j] + a[i * 3 + 1] * b[3 + j] + a[i * 3 + 2] * b[6 + j]) >> 12) as i16 as i32;
        }
    }
    o
}

/// `Math_VecMat` @ 0x431ad0: the row vector v · M (= Mᵀ · v).
fn vec_mat(v: I3, m: &M3) -> I3 {
    let c = |j: usize| ((v[0] as i64 * m[j] as i64 + v[1] as i64 * m[3 + j] as i64 + v[2] as i64 * m[6 + j] as i64) >> 12) as i32;
    [c(0), c(1), c(2)]
}

fn transpose(m: &M3) -> M3 {
    [m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8]]
}

/// `Math_ApproxLength` @ 0x430fa0: max + 3/8·(the other two) − max/16.
pub(crate) fn approx_len(d: I3) -> i32 {
    let (mut a, mut b, c) = (d[0].abs(), d[1].abs(), d[2].abs());
    if a < b {
        core::mem::swap(&mut a, &mut b);
    }
    let (hi, mid) = if a < c { (c, a) } else { (a, c) };
    ((b + mid) >> 2) + ((b + mid) >> 3) - (hi >> 4) + hi
}

/// `Math_FrameFromNormal` @ 0x4312b0: the rotation taking +z to the unit
/// normal `n` (Rodrigues, about z × n), rounded to 4.12. Applied with
/// [`vec_mat`] it turns a world vector into the surface's frame.
fn frame_from_normal(n: I3) -> M3 {
    let [x, y, z] = n.map(|v| v as f64);
    let len = (x * x + y * y + z * z).sqrt();
    let (c, s) = if len != 0.0 { (z / len, (x * x + y * y).sqrt() / len) } else { (1.0, 0.0) };
    let hl = (x * x + y * y).sqrt();
    let (ux, uy) = if hl == 0.0 { (0.0, 1.0) } else { (x / hl, y / hl) };
    [
        c * ux * ux + uy * uy,
        (c - 1.0) * ux * uy,
        s * ux,
        (c - 1.0) * ux * uy,
        c * uy * uy + ux * ux,
        s * uy,
        -s * ux,
        -s * uy,
        c,
    ]
    .map(|v| round_half_up(v * 4096.0))
}

fn add(a: I3, b: I3) -> I3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: I3, b: I3) -> I3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Wrap an angle difference into [-0x200, 0x200], as 16-bit shorts do.
fn wrap(d: i32) -> i32 {
    let d = d as i16 as i32;
    if d > 0x200 {
        d - 0x400
    } else if d < -0x200 {
        d + 0x400
    } else {
        d
    }
}

// ——— the three building blocks ——————————————————————————————————————

/// `Mover` (0x427780, `SetTarget` @ 0x427840, `Step` @ 0x4278c0): a point
/// that travels in a straight line with a trapezoidal speed profile.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(crate) struct Mover {
    pub(crate) pos: I3,
    pub(crate) delta: I3,
    pub(crate) brake: i32,
    pub(crate) stop_dist: i32,
    pub(crate) total: i32,
    pub(crate) remain: i32,
    pub(crate) max: i16,
    pub(crate) accel: i16,
    pub(crate) speed: i16,
    pub(crate) moving: bool,
}

impl Mover {
    /// 0x4277e0: `stop_dist` is the distance covered while braking from
    /// full speed: Σ 0, accel, 2·accel, … ≤ max.
    pub(crate) fn setup(&mut self, pos: I3, accel: i16, max: i16) {
        self.pos = pos;
        self.accel = accel;
        self.max = max;
        self.speed = 0;
        self.stop_dist = 0;
        if accel > 0 && max >= 0 {
            let mut s: i16 = 0;
            while s <= max {
                self.stop_dist += s as i32;
                s = s.wrapping_add(accel);
            }
        }
        self.moving = false;
    }

    pub(crate) fn set_target(&mut self, t: I3) {
        self.delta = sub(t, self.pos);
        self.moving = true;
        self.speed = 0;
        self.total = approx_len(self.delta);
        self.remain = self.total;
        if self.total == 0 {
            self.moving = false;
        } else if self.total < self.stop_dist * 2 {
            self.brake = self.total >> 1;
        } else {
            self.brake = self.stop_dist;
        }
    }

    pub(crate) fn step(&mut self) {
        if !self.moving {
            return;
        }
        if self.brake < self.remain {
            if self.speed < self.max {
                self.speed = self.speed.wrapping_add(self.accel).min(self.max);
            }
        } else if self.speed > 0 {
            self.speed = (self.speed - self.accel).max(0);
        }
        if self.speed == 0 {
            self.moving = false;
        }
        let v = self.speed as i32;
        self.remain -= v;
        if self.total != 0 {
            for i in 0..3 {
                self.pos[i] += self.delta[i] * v / self.total;
            }
        }
    }
}

/// One axis of `Tracker` (0x406a50): an alpha-beta filter in 16.16.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(crate) struct Filter {
    pub(crate) d: i32,
    pub(crate) alpha: i32,
    pub(crate) beta: i32,
    pub(crate) vel: i32,
    pub(crate) out: i32,
    pub(crate) primed: bool,
}

impl Filter {
    /// 0x406b60: α = 1 − d², β = (1 − d)² (16.16).
    pub(crate) fn set_d(&mut self, d: i32) {
        self.d = d;
        self.alpha = 0x10000 - ((d as u32).wrapping_mul(d as u32) >> 16) as i32;
        self.beta = ((0x10000 - d) * (0x10000 - d)) >> 16;
    }
    /// 0x406af0: same, and forget the state.
    pub(crate) fn reset(&mut self, d: i32) {
        self.set_d(d);
        self.vel = 0;
        self.primed = false;
    }
    /// 0x406b90: predict, then correct by the error.
    pub(crate) fn step(&mut self, x: i32) -> i32 {
        if !self.primed {
            self.out = x;
            self.primed = true;
            return x;
        }
        let pred = (self.vel >> 16) + self.out;
        let err = x - pred;
        self.out = (self.alpha.wrapping_mul(err) >> 16) + pred;
        self.vel = self.vel.wrapping_add(self.beta.wrapping_mul(err));
        self.out
    }
}

pub(crate) fn filter3(f: &mut [Filter; 3], v: I3) -> I3 {
    [f[0].step(v[0]), f[1].step(v[1]), f[2].step(v[2])]
}

// ——— the camera ——————————————————————————————————————————————————————

/// The polygon a key belongs to: its surface frame (`+0x204`).
#[derive(Clone, Copy)]
struct Surface {
    frame: M3,
    upright: bool,
}

/// What the renderer gets (`View_SetCamera` @ 0x403870).
#[derive(Clone, Copy, Default)]
struct View {
    eye: I3,
    yaw: i32,
    pitch: i32,
    roll: i32,
}

/// The collision probes of `Camera_Collide` @ 0x413cb0 (constructor
/// 0x412700): ±700 along each axis.
const PROBES: [I3; 6] = [[700, 0, 0], [-700, 0, 0], [0, 700, 0], [0, -700, 0], [0, 0, 700], [0, 0, -700]];

/// Floats per camera sample handed to JavaScript, see [`Camera::sample`].
pub const SAMPLE: usize = 16;

pub struct Camera {
    trig: Trig,
    keys: Vec<Vec<CameraKey>>,
    default: CameraKey,
    styles: Vec<CameraStyle>,
    /// Per object, per polygon: the object-local normal, length 1024
    /// (`(v2 − v1) × (v0 − v1)`, as the polygon records of 0x409430).
    normals: Vec<Vec<I3>>,

    // `+0x200 ..`: the key in use and the polygon it came from.
    key: Option<CameraKey>,
    obj: Option<usize>,
    poly: usize,
    surface: Surface,
    /// `+0x21a`: 0 = upright surface, 1 = tilted surface, 2 = also in the
    /// object's rotating frame (modes 4, 5, 6, 10, 11).
    frame_mode: u8,
    // `+0x220 ..`: the key, decoded with its style.
    key_pos: I3,
    key_roll: i32,
    accel: i16,
    max: i16,
    d_look: i32,
    d_eye: i32,
    look_in: I3,

    mover: Mover,
    eye_world: I3,
    eye_f: [Filter; 3],
    eye: I3,
    look_f: [Filter; 3],
    look: I3,
    // `+0x78 ..`: the roll angle travels with the mover.
    angle: i32,
    angle_out: i32,
    angle_target: i32,
    angle_diff: i32,
    view: View,
    last_eye: I3,

    // Host side (see `tick`): Mendel's contact polygon, his mode last tick
    // and a pending camera cut.
    last_ground: Option<(usize, usize)>,
    prev_mode: Option<Mode>,
    snap_wanted: bool,
    /// The last two ticks' samples, for interpolation in the browser.
    pub samples: [[f32; SAMPLE]; 2],
}

impl Camera {
    pub fn new(section: &Section) -> Self {
        let cam = section.objects.iter().find_map(|o| match o {
            Object::Camera(c) => Some(c),
            _ => None,
        });
        let empty = CameraKey { a: 0, b: 0, position: [0; 3], c: [0; 3] };
        let normals = section
            .objects
            .iter()
            .map(|o| match o {
                Object::World(w) => w.mesh.as_ref().map_or_else(Vec::new, |m| {
                    m.polygons
                        .iter()
                        .map(|p| {
                            let v = |k: usize| p.get(k).and_then(|&i| m.vertices.get(i as usize)).map_or([0; 3], |v| v.map(|c| c as i32));
                            polygon_normal(v(0), v(1), v(2))
                        })
                        .collect()
                }),
                Object::Camera(_) => Vec::new(),
            })
            .collect();
        Camera {
            trig: Trig::new(),
            keys: cam.map_or_else(Vec::new, |c| c.paths.clone()),
            default: cam.map_or(empty, |c| c.default),
            styles: section.camera_styles.clone(),
            normals,
            key: None,
            obj: None,
            poly: 0,
            surface: Surface { frame: frame_from_normal([0, 0, 1024]), upright: true },
            frame_mode: 0,
            key_pos: [0; 3],
            key_roll: 0,
            accel: 1,
            max: 0,
            d_look: 0,
            d_eye: 0,
            look_in: [0; 3],
            mover: Mover::default(),
            eye_world: [0; 3],
            eye_f: [Filter::default(); 3],
            eye: [0; 3],
            look_f: [Filter::default(); 3],
            look: [0; 3],
            angle: 0,
            angle_out: 0,
            angle_target: 0,
            angle_diff: 0,
            view: View::default(),
            last_eye: [0; 3],
            last_ground: None,
            prev_mode: None,
            snap_wanted: false,
            samples: [[0.0; SAMPLE]; 2],
        }
    }

    fn style(&self, b: u8) -> Option<CameraStyle> {
        // `b` is read as a signed char: section 0 and 5 each have one key
        // with b = 0xff, i.e. index −1, which reads before the table in the
        // original (undefined). We ignore such keys.
        self.styles.get(b as usize).copied()
    }

    fn key_at(&self, obj: usize, poly: usize) -> Option<CameraKey> {
        self.keys.get(obj)?.get(poly).copied().filter(|k| k.a != 0 && self.style(k.b).is_some())
    }

    fn surface_of(&self, obj: usize, poly: usize) -> Surface {
        let n = self.normals.get(obj).and_then(|p| p.get(poly)).copied().unwrap_or([0, 0, 1024]);
        Surface { frame: frame_from_normal(n), upright: n[2] == 0x400 }
    }

    /// The key object's world → local rotation (4.12), if it is rotated.
    /// inferred: we take it from our own object transform (sim.rs), the
    /// original from the object's angles via 0x4097a0.
    fn obj_rot(&self, sim: &Sim) -> Option<M3> {
        let (pos, rot) = sim.pose(self.obj?);
        if rot == [0; 3] {
            return None;
        }
        let m = sim::transform(pos, rot);
        Some([m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8]].map(|v| (v * 4096.0).round() as i32))
    }
    /// 0x409990: world → object-local.
    fn to_local(&self, sim: &Sim, v: I3) -> I3 {
        self.obj_rot(sim).map_or(v, |o| vec_mat(v, &transpose(&o)))
    }
    /// 0x409940 / 0x409880: object-local → world.
    fn to_world(&self, sim: &Sim, v: I3) -> I3 {
        self.obj_rot(sim).map_or(v, |o| vec_mat(v, &o))
    }
    fn obj_pos(&self, sim: &Sim) -> I3 {
        self.obj.map_or([0; 3], |o| sim.pose(o).0)
    }

    fn mode(&self) -> Option<i16> {
        self.key.and_then(|k| self.style(k.b)).map(|s| s.mode)
    }

    /// The key object's rotation, in the modes that use it.
    fn frame_rot(&self, sim: &Sim) -> Option<M3> {
        if self.frame_mode == 2 {
            self.obj_rot(sim)
        } else {
            None
        }
    }

    fn key_roll(&self, sim: &Sim, pitch: i32, yaw: i32, roll: i32) -> i32 {
        roll_in_frame(&self.trig, &self.surface.frame, self.frame_rot(sim).as_ref(), pitch, yaw, roll)
    }

    /// `Camera_Orient` @ 0x4134e0.
    fn orient(&mut self, sim: &Sim) {
        let o = self.frame_rot(sim);
        let (yaw, pitch, roll) =
            orient_in_frame(&self.trig, &self.surface.frame, self.frame_mode, o.as_ref(), self.eye, self.look, self.angle_out);
        self.view.yaw = yaw;
        self.view.pitch = pitch;
        self.view.roll = roll;
    }

    /// `Camera_ApplyPolygonKey` @ 0x412f60: decode the key, re-express the
    /// camera's current point in the key's reference frame.
    fn apply_key(&mut self, sim: &Sim, mendel: I3) {
        let Some(k) = self.key else { return };
        let Some(style) = self.style(k.b) else { return };
        self.key_pos = k.position;
        self.frame_mode = 1;
        self.key_roll = self.key_roll(sim, k.c[1] as i32, k.c[0] as i32, k.c[2] as i32);
        let tilted = !self.surface.upright as u8;
        match style.mode {
            0 | 7 => {
                self.mover.pos = self.eye_world;
                self.frame_mode = tilted;
            }
            1 | 8 => {
                self.mover.pos = sub(self.eye_world, self.obj_pos(sim));
                self.frame_mode = tilted;
            }
            2 | 3 | 9 => {
                self.mover.pos = sub(self.eye_world, mendel);
                self.frame_mode = tilted;
            }
            4 | 5 | 6 | 10 | 11 => {
                let o = self.obj_pos(sim);
                let local = match style.mode {
                    4 => sub(self.eye_world, o),
                    5 | 6 => sub(self.eye_world, mendel),
                    _ => [self.eye_world[0] - o[0], self.eye_world[1] - o[1], self.eye_world[2] - mendel[2]],
                };
                self.mover.pos = self.to_local(sim, local);
                self.frame_mode = 2;
            }
            _ => {}
        }
        // The roll the camera has now, in the new frame: where the roll
        // starts from on its way to the key's.
        self.angle = self.key_roll(sim, self.view.pitch, self.view.yaw, self.view.roll);
        self.accel = style.limits[0];
        self.max = style.limits[1];
        self.d_look = style.damping[0] as i32;
        self.d_eye = style.damping[1] as i32;
        self.look_in = mendel;
    }

    /// `Camera_Reference` (vtable +0x14 @ 0x4136d0): the mover's point made
    /// absolute, according to the key's mode.
    fn reference(&mut self, sim: &Sim, mendel: I3, on_ground: bool) {
        let p = self.mover.pos;
        let Some(mode) = self.mode() else {
            self.eye_world = p;
            return;
        };
        if matches!(mode, 7..=9) {
            self.collide(sim);
        }
        let p = self.mover.pos;
        // Modes 3, 6 and 11 hold still while Mendel is off the ground
        // (0x412400; the contact flag `+0x52` is not ported).
        if matches!(mode, 3 | 6 | 11) && !on_ground {
            return;
        }
        self.eye_world = match mode {
            0 | 7 => p,
            1 | 8 => add(p, self.obj_pos(sim)),
            2 | 3 | 9 => add(p, mendel),
            4 => add(self.obj_pos(sim), self.to_world(sim, p)),
            5 | 6 => add(mendel, self.to_world(sim, p)),
            10 | 11 => {
                let (o, r) = (self.obj_pos(sim), self.to_world(sim, p));
                [o[0] + r[0], o[1] + r[1], mendel[2] + r[2]]
            }
            _ => return,
        };
    }

    /// `Camera_Collide` @ 0x413cb0 (modes 7, 8, 9): six probes of 700
    /// units from the last eye; a wall (flags 0x102) crossing one pushes
    /// the camera out so that the probe's tip lies 8 units in front of it.
    fn collide(&mut self, sim: &Sim) {
        let Some(c) = sim.creature.as_ref() else { return };
        let from = self.last_eye;
        for d in PROBES {
            let to = add(from, d);
            let f = |v: I3| v.map(|c| c as f32);
            let Some((a, n)) = c.camera_probe(sim, f(from), f(to)) else { continue };
            let depth = (a[0] - to[0] as f32) * n[0] + (a[1] - to[1] as f32) * n[1] + (a[2] - to[2] as f32) * n[2];
            let push = n.map(|c| c * (depth + 8.0)).map(|v| v.round() as i32);
            self.eye = add(self.eye, push);
            self.mover.pos = add(self.mover.pos, push);
            for (i, fl) in self.eye_f.iter_mut().enumerate() {
                fl.vel = fl.vel.wrapping_add(push[i] << 16);
                fl.out = self.eye[i];
            }
        }
    }

    /// 0x406f80: start the pipeline at a point, with no history.
    fn start(&mut self, sim: &Sim, mendel: I3, on_ground: bool, at: I3, look: I3, roll: i32) {
        self.mover.setup(at, self.accel, self.max);
        for f in &mut self.eye_f {
            f.reset(self.d_eye);
        }
        self.reference(sim, mendel, on_ground);
        self.eye = filter3(&mut self.eye_f, self.eye_world);
        for f in &mut self.look_f {
            f.reset(self.d_look);
        }
        self.look = filter3(&mut self.look_f, look);
        self.angle = roll;
        self.angle_out = roll;
    }

    /// `Camera_Init` @ 0x4129a0, run at level load, spawn, respawn and
    /// "snapping" teleports: jump straight to the key of the polygon under
    /// Mendel; with no ground at all, to the section's default key.
    fn init(&mut self, sim: &Sim) {
        let (mendel, on_ground) = mendel(sim);
        let key = self.last_ground.and_then(|(o, p)| self.key_at(o, p));
        if let (Some((o, p)), Some(k)) = (self.last_ground, key) {
            self.obj = Some(o);
            self.poly = p;
            self.surface = self.surface_of(o, p);
            self.key = Some(k);
            self.apply_key(sim, mendel);
            self.start(sim, mendel, on_ground, self.key_pos, self.look_in, self.key_roll);
            self.reference(sim, mendel, on_ground);
            self.orient(sim);
            self.view.eye = self.eye;
        } else if self.last_ground.is_none() {
            // The default key: an absolute position and a full orientation,
            // used as is (the look-at point is 5000 units ahead).
            self.key = None;
            self.mover.pos = self.eye_world;
            let k = self.default;
            if let Some(style) = self.style(k.b) {
                self.accel = style.limits[0];
                self.max = style.limits[1];
                self.d_look = style.damping[0] as i32;
                self.d_eye = style.damping[1] as i32;
            }
            let (pitch, yaw, roll) = (k.c[1] as i32, k.c[0] as i32, k.c[2] as i32);
            let r = self.trig.rot(yaw, pitch, roll);
            self.look_in = add(k.position, vec_mat([0, 5000, 0], &r));
            self.key_pos = k.position;
            self.key_roll = roll;
            self.start(sim, mendel, on_ground, k.position, self.look_in, roll);
            self.view = View { eye: self.eye, yaw, pitch, roll };
        }
        // (ground without a key: the camera keeps what it has)
        self.last_eye = self.eye;
        self.samples = [self.sample(); 2];
    }

    /// `Camera_Update` @ 0x412d50, once per tick after the creature.
    fn update(&mut self, sim: &Sim) {
        let (mendel, on_ground) = mendel(sim);
        let Some((o, p)) = self.last_ground else {
            self.key = None;
            self.mover.pos = self.eye_world;
            return;
        };
        if (Some(o) != self.obj || p != self.poly) && self.keys.get(o).is_some_and(|k| !k.is_empty()) {
            if let Some(k) = self.key_at(o, p) {
                self.obj = Some(o);
                self.poly = p;
                self.surface = self.surface_of(o, p);
                self.key = Some(k);
                self.apply_key(sim, mendel);
                self.retarget();
            }
        }
        self.look_in = mendel;
        // 0x4070b0 with a full tick (0x100): the original's sub-tick
        // interpolation argument is left to the browser.
        self.mover.step();
        self.reference(sim, mendel, on_ground);
        self.eye = filter3(&mut self.eye_f, self.eye_world);
        if self.mover.moving {
            let u = (self.angle_target - (self.mover.remain * self.angle_diff) / self.mover.total + 0x400) as i16 as i32;
            self.angle = u;
            self.angle_out = u;
        } else {
            self.angle_out = self.angle;
        }
        self.look = filter3(&mut self.look_f, self.look_in);
        self.orient(sim);
        self.view.eye = self.eye;
        self.last_eye = self.eye;
    }

    /// 0x407030: a new key. The mover heads for it from where it is; the
    /// trackers keep their state and only change stiffness.
    fn retarget(&mut self) {
        if self.d_eye != self.eye_f[0].d {
            // 0x414c10: a stiffer or softer tracker restarts from its output.
            let diff = sub(self.eye, self.eye_world);
            self.mover.pos = add(self.mover.pos, diff);
            self.eye_world = add(self.eye_world, diff);
        }
        self.mover.set_target(self.key_pos);
        for f in &mut self.eye_f {
            f.set_d(self.d_eye);
        }
        for f in &mut self.look_f {
            f.set_d(self.d_look);
        }
        self.angle_target = self.key_roll;
        self.angle_diff = wrap((self.key_roll & 0x3ff) - (self.angle & 0x3ff));
    }

    /// One tick, after the creature's. `snapped`: a teleporter of kind 2
    /// asked for a camera cut this tick (`EV_CAMERA_SNAP`).
    ///
    /// The key follows Mendel's contact polygon (`Mover_GetSurface` @
    /// 0x4124a0: the mover's `+0x8`/`+0x30`): placements set it to the
    /// portal's polygon (`Mover_Place` @ 0x411bf0, from
    /// `Game_SpawnCreature` @ 0x41c590 / `Game_OnCreatureRespawn` @
    /// 0x41c700), landing changes it, flight keeps it. `Camera_Init` runs
    /// at spawn, at respawn and after kind-2 teleports, as in the original.
    pub fn tick(&mut self, sim: &Sim, snapped: bool) {
        let Some(c) = sim.creature.as_ref() else { return };
        self.snap_wanted |= snapped;
        let mode = c.mode;
        let was = self.prev_mode.replace(mode);
        let mut cut = false;
        match (was, mode) {
            (Some(Mode::Absent), Mode::Appearing(_)) => cut = true,
            // Respawn (any reason: a lethal floor, a kind-6 portal; the
            // world snapshot was just restored): back to the respawn point.
            (Some(Mode::Vanishing(_, After::Respawn)), Mode::Appearing(_)) => {
                self.snap_wanted = false;
                cut = true;
            }
            (Some(Mode::Vanishing(_, After::Teleport(..))), Mode::Appearing(_)) => {
                cut = self.snap_wanted;
                self.snap_wanted = false;
            }
            _ => {}
        }
        if let Some(g) = c.surface() {
            self.last_ground = Some(g);
        }
        if cut {
            self.init(sim);
        }
        self.samples[0] = if cut { self.sample() } else { self.samples[1] };
        self.update(sim);
        self.samples[1] = self.sample();
    }

    /// Level load: the default key (Mendel is not there yet).
    pub fn start_level(&mut self, sim: &Sim) {
        self.last_ground = None;
        self.prev_mode = sim.creature.as_ref().map(|c| c.mode);
        self.init(sim);
    }

    /// A saved game was opened: Mendel stands where he was saved, so the
    /// camera starts from the key of his floor, as `Camera_Init` does after
    /// a respawn (the saved respawn point is that same spot).
    pub fn start_loaded(&mut self, sim: &Sim) {
        let c = sim.creature.as_ref();
        self.prev_mode = c.map(|c| c.mode);
        self.last_ground = c.and_then(|c| c.surface());
        self.init(sim);
        self.samples = [self.sample(); 2];
    }

    /// 16 f32: 1 (valid), eye xyz, forward xyz, up xyz (unit vectors of the
    /// original's view matrix: rows 1 and 2 of `Math_RotationMatrix`), the
    /// filtered look-at point xyz, key object id (1-based, 0 = default key),
    /// key polygon, key style mode.
    pub fn sample(&self) -> [f32; SAMPLE] {
        let v = &self.view;
        // The renderer builds its matrix in floats (0x4302e0); so do we,
        // for smooth interpolation.
        let r = |a: i32| -(a as f32) * core::f32::consts::TAU / 1024.0;
        let (s5, c7) = r(v.yaw).sin_cos();
        let (s4, c2) = r(v.pitch).sin_cos();
        let (s3, c6) = r(v.roll).sin_cos();
        let fwd = [c2 * s5, c2 * c7, -s4];
        let up = [c6 * s5 * s4 + s3 * c7, c6 * c7 * s4 - s3 * s5, c6 * c2];
        let id = self.key.and(self.obj).map_or(0.0, |o| o as f32 + 1.0);
        [
            1.0,
            v.eye[0] as f32,
            v.eye[1] as f32,
            v.eye[2] as f32,
            fwd[0],
            fwd[1],
            fwd[2],
            up[0],
            up[1],
            up[2],
            self.look[0] as f32,
            self.look[1] as f32,
            self.look[2] as f32,
            id,
            self.poly as f32,
            self.mode().map_or(-1.0, |m| m as f32),
        ]
    }
}

/// `Camera_KeyRoll` @ 0x4132d0: the roll of orientation (pitch, yaw,
/// roll), measured in a surface's frame (`frame`, see
/// [`frame_from_normal`]; `o`: the object's world -> local rotation).
fn roll_in_frame(t: &Trig, frame: &M3, o: Option<&M3>, pitch: i32, yaw: i32, roll: i32) -> i32 {
    let r1 = t.rot(yaw, pitch, 0);
    let mut v = [r1[3], r1[4], r1[5]];
    if let Some(o) = o {
        v = vec_mat(v, &transpose(o));
    }
    let (a, b) = t.dir_angles(vec_mat(v, frame));
    let mut x = t.rot(yaw, pitch, roll);
    if let Some(o) = o {
        x = mat_mul(&x, &transpose(o));
    }
    let x = mat_mul(&mat_mul(&x, frame), &t.rot_t(a, b, 0));
    t.angle(x[6], x[8]) + 0x300
}

/// `Camera_Orient` @ 0x4134e0: (yaw, pitch) from eye -> target, and the
/// roll that puts `roll` (measured in the surface frame) back into the
/// world. `frame_mode` 0 (upright surface) uses `roll` as is.
fn orient_in_frame(t: &Trig, frame: &M3, frame_mode: u8, o: Option<&M3>, eye: I3, look: I3, roll: i32) -> (i32, i32, i32) {
    let mut d = sub(look, eye);
    let (yaw, pitch) = t.dir_angles(d);
    if frame_mode != 1 && frame_mode != 2 {
        return (yaw, pitch, roll);
    }
    if let Some(o) = o {
        d = vec_mat(d, &transpose(o));
    }
    let (ya, pa) = t.dir_angles(vec_mat(d, frame));
    let mut x = mat_mul(&t.rot(ya, pa, roll), &transpose(frame));
    if let Some(o) = o {
        x = mat_mul(&x, o);
    }
    let x = mat_mul(&x, &t.rot_t(yaw, pitch, 0));
    (yaw, pitch, t.angle(x[6], x[8]) + 0x300)
}

/// `Polygon_Normal` (0x431f90 as called by 0x409430): (v2 − v1) × (v0 − v1),
/// scaled to length 1024 and truncated.
fn polygon_normal(v0: I3, v1: I3, v2: I3) -> I3 {
    let a = sub(v2, v1).map(|c| c as f64);
    let b = sub(v0, v1).map(|c| c as f64);
    let n = [a[1] * b[2] - b[1] * a[2], b[0] * a[2] - a[0] * b[2], a[0] * b[1] - b[0] * a[1]];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len == 0.0 {
        return [0, 0, 0];
    }
    n.map(|c| (c * 1024.0 / len) as i32)
}

/// Mendel's position (`+0xa34`) and whether he stands on something.
fn mendel(sim: &Sim) -> (I3, bool) {
    sim.creature.as_ref().map_or(([0; 3], false), |c| (c.pos.map(|v| v.round() as i32), c.ground().is_some()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creature::Creature;
    use crate::world::WorldMesh;
    use galapagos_formats::creature::{overlay_for_age, Brain, CreatureBody};
    use galapagos_formats::worlds::Archive;

    /// Measuring a key's roll in a surface frame and rebuilding the view in
    /// that frame gives back the key's own orientation, when the camera
    /// looks where the key looks: on a floor, a wall and a slope.
    #[test]
    fn roll_survives_the_surface_frame() {
        let t = Trig::new();
        // Real key orientations (yaw, pitch, roll) from sections 3, 0 and 1.
        for c in [[245, 966, 0], [1020, 933, 1018], [302, 1015, 769]] {
            let [y, p, r] = c;
            let want = t.rot(y, p, r);
            let fwd = [want[3], want[4], want[5]];
            for n in [[0, -1024, 0], [1024, 0, 0], [0, -724, 724]] {
                let f = frame_from_normal(n);
                let k = roll_in_frame(&t, &f, None, p, y, r);
                let (yy, pp, rr) = orient_in_frame(&t, &f, 1, None, [0; 3], fwd.map(|v| v * 4), k);
                let got = t.rot(yy, pp, rr);
                let up_err: i32 = (6..9).map(|i| (got[i] - want[i]).abs()).max().unwrap();
                assert!(up_err < 120, "c {c:?} n {n:?}: up {:?} vs {:?}", &got[6..9], &want[6..9]);
            }
        }
    }

    #[test]
    fn math_library_agrees_with_itself() {
        let t = Trig::new();
        assert_eq!(t.sin[256], 16384);
        assert_eq!(t.atan[1024], 128);
        assert_eq!(t.angle(100, 0), 0);
        assert_eq!(t.angle(0, 100), 0x100);
        assert_eq!(t.angle(-100, 0), 0x200);
        assert_eq!(t.angle(0, -100), 0x300);
        assert_eq!(t.angle(100, 100), 0x80);
        // rot_t is the transpose of rot, and rot(0,0,0) is the identity.
        assert_eq!(t.rot(0, 0, 0), [4096, 0, 0, 0, 4096, 0, 0, 0, 4096]);
        for (y, p, r) in [(100, 50, 7), (900, 1000, 300), (3, 600, 1020)] {
            let a = t.rot(y, p, r);
            let b = transpose(&t.rot_t(y, p, r));
            assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 2), "{a:?} vs {b:?}");
        }
        // The forward row of rot(yaw, pitch, 0) points where dir_angles says.
        for (yaw, pitch) in [(200, 40), (966, 245), (933, 1020), (10, 900), (512, 100)] {
            let f = t.rot(yaw, pitch, 0);
            let (y, p) = t.dir_angles([f[3], f[4], f[5]]);
            assert!(wrap(y - yaw).abs() <= 1 && wrap(p - pitch).abs() <= 1, "({yaw}, {pitch}) → ({y}, {p})");
        }
        // A frame for +z is the identity; for n it maps n to +z.
        assert_eq!(frame_from_normal([0, 0, 1024]), [4096, 0, 0, 0, 4096, 0, 0, 0, 4096]);
        let m = frame_from_normal([0, -1024, 0]);
        assert_eq!(vec_mat([0, -1024, 0], &m), [0, 0, 1024]);
    }

    #[test]
    fn filter_tracks_a_step_without_overshooting_much() {
        let mut f = [Filter::default(); 3];
        for x in &mut f {
            x.reset(62259); // 0.95
        }
        filter3(&mut f, [0; 3]);
        let mut last = 0;
        let mut peak = 0;
        for _ in 0..240 {
            last = filter3(&mut f, [10_000, 0, 0])[0];
            peak = peak.max(last);
        }
        assert!((last - 10_000).abs() < 50, "settles: {last}");
        assert!(peak < 11_500, "critically damped-ish: {peak}");
    }

    fn data() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds")).ok()
    }

    fn section0(d: &[u8]) -> (Sim, WorldMesh, Camera) {
        let a = Archive::parse(d).unwrap();
        let section = a.world(0).unwrap();
        let mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mesh);
        let body = CreatureBody::parse(d, a.header.creature as usize).unwrap();
        let brain = Brain::parse(d, a.header.creature_overlays[overlay_for_age(128).unwrap()] as usize).unwrap();
        sim.creature = Some(Creature::new(&section, &mesh, body, brain));
        let mut cam = Camera::new(&section);
        cam.start_level(&sim);
        (sim, mesh, cam)
    }

    /// Object-relative keys (mode 1) carry the camera along with their
    /// object: on the intro ride Mendel stands on the capsule's (#3) floor
    /// polygons 25, 27 and 29 in turn (its appearance states hand the
    /// floor around as it rolls); polygon 25's key is object-relative, so
    /// while he is on it the eye flies with the capsule.
    #[test]
    fn object_relative_key_rides_the_capsule() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh, mut cam) = section0(&d);
        let mut boarded = None;
        let mut max_d = 0.0f32;
        let mut riding = 0;
        for t in 0..80 * sim::TICKS_PER_SECOND as u64 {
            sim.step(&mut mesh);
            if boarded.is_none() && sim.creature.as_ref().unwrap().ground().is_some_and(|g| g.0 == 2) {
                boarded = Some((t, sim.pose(2).0));
            }
            cam.tick(&sim, false);
            let s = cam.sample();
            if let Some((t0, _)) = boarded {
                if (s[13], s[14], s[15]) == (3.0, 25.0, 1.0) && t > t0 + 5 * 24 {
                    riding += 1;
                    let p = sim.pose(2).0;
                    let d = (0..3).map(|i| (s[1 + i] - p[i] as f32).powi(2)).sum::<f32>().sqrt();
                    max_d = max_d.max(d);
                }
            }
        }
        let (_, p0) = boarded.expect("Mendel boards the capsule");
        let p1 = sim.pose(2).0;
        let flown = (0..3).map(|i| ((p1[i] - p0[i]) as f32).powi(2)).sum::<f32>().sqrt();
        println!("capsule flew {flown}; {riding} ticks on key 25, eye within {max_d} of it");
        assert!(flown > 20_000.0, "the capsule flies ({flown})");
        assert!(riding > 24, "some of the ride uses polygon 25's object-relative key");
        // key offset |(-1887, -2454, 4426)| ≈ 5400, plus the filters' lag
        assert!(max_d < 12_000.0, "the eye rides along ({max_d})");
    }

    /// Section 0 from load through the intro skip. The camera starts on the
    /// section's default key; the spawn hands it the `Strt` portal's
    /// polygon (#57, key 0); the Space skip teleports Mendel with a camera
    /// cut to pad #83's polygon 9. Throughout, it looks at Mendel.
    #[test]
    fn camera_follows_mendel_in_section0() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let section = a.world(0).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mesh);
        let body = CreatureBody::parse(&d, a.header.creature as usize).unwrap();
        let brain = Brain::parse(&d, a.header.creature_overlays[overlay_for_age(128).unwrap()] as usize).unwrap();
        sim.creature = Some(Creature::new(&section, &mesh, body, brain));
        let mut cam = Camera::new(&section);
        cam.start_level(&sim);
        let s0 = cam.sample();
        assert_eq!(s0[13], 0.0, "default key at load");
        assert_eq!([s0[1], s0[2], s0[3]], cam.default.position.map(|v| v as f32));
        // The default key, read as (yaw, pitch, roll), aims at the Strt
        // portal (#57) where Mendel will appear: the field order is right.
        let strt = sim.pose(56).0;
        let to = (0..3).map(|i| strt[i] as f32 - s0[1 + i]).collect::<Vec<_>>();
        let l = to.iter().map(|v| v * v).sum::<f32>().sqrt();
        let cos = (0..3).map(|i| to[i] / l * s0[4 + i]).sum::<f32>();
        assert!(cos > 0.99, "default key looks at the start pad (cos {cos})");

        let mut keys = Vec::new();
        let (mut alive, mut worst_cos, mut farthest) = (0, 1.0f32, 0.0f32);
        for t in 0..60 * sim::TICKS_PER_SECOND as u64 {
            // Hold Space at 20 s, as a player skipping the intro ride.
            sim.skip_held = (20 * 24..20 * 24 + 36).contains(&t);
            let before = sim.events.len();
            sim.step(&mut mesh);
            let snap = sim.events[before..].iter().any(|e| e.kind == sim::EV_CAMERA_SNAP);
            cam.tick(&sim, snap);
            sim.events.clear();
            let s = cam.sample();
            let key = (s[13] as i32, s[14] as i32);
            if keys.last() != Some(&key) {
                keys.push(key);
            }
            let c = sim.creature.as_ref().unwrap();
            if c.mode == Mode::Alive {
                alive += 1;
                let to = [c.pos[0] - s[1], c.pos[1] - s[2], c.pos[2] - s[3]];
                let l = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
                farthest = farthest.max(l);
                // The look-at point lags Mendel by the filter; give it 1 s
                // after each cut to catch up.
                if t > 26 * 24 || (16 * 24..20 * 24).contains(&t) {
                    worst_cos = worst_cos.min((to[0] * s[4] + to[1] * s[5] + to[2] * s[6]) / l);
                }
                let up = [s[7], s[8], s[9]];
                assert!(((up[0] * up[0] + up[1] * up[1] + up[2] * up[2]).sqrt() - 1.0).abs() < 1e-3);
                assert!((up[0] * s[4] + up[1] * s[5] + up[2] * s[6]).abs() < 1e-3, "up ⟂ forward");
            }
        }
        println!("keys {keys:?}, alive {alive} ticks, worst cos {worst_cos}, farthest {farthest}");
        assert_eq!(keys.first(), Some(&(0, 0)), "default key first");
        assert!(keys.contains(&(57, 0)), "the spawn's Strt polygon: {keys:?}");
        assert!(keys.contains(&(83, 9)), "the skip's pad polygon: {keys:?}");
        assert!(alive > 30 * 24);
        assert!(worst_cos > 0.95, "Mendel stays in the middle of the view (cos {worst_cos})");
        assert!(farthest < 40_000.0, "and within sight ({farthest})");
    }
}
