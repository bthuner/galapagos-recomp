//! Transition effects: how Mendel and the predators dematerialise and
//! materialise (`Effect` class, ctor `0x424520`, vtable `0x460b98`).
//!
//! One manager per owner (Mendel's `creature+0x8ec`, a predator's
//! `+0x210`) owns seven sub-effects, one per *kind*; `0x424870(kind,
//! variant, height)` picks one and `0x4249b0` starts it, choosing its
//! parameters, a palette flash (`0x438830(id, 200)`) and a sound by
//! *variant*. `0x4248c0` plays the same effect backwards: `variant +
//! 0x40`, with a few kinds swapped for another (4, 5 and 7 materialise
//! with kind 2). All observed in the exe; the tables below are verbatim.
//!
//! | Kind | Class (start, step) | What the polygons do |
//! |---:|---|---|
//! | 1 | `0x40f780` (`0x40fa50`, `0x40fd00`) | **fade**: one by one (random order, or sorted by their highest x/y/z), each brightens to a white glow and fades to dark over `n` ticks, then vanishes; backwards they come in from black. Flag `0x100`: a glow wave, nothing vanishes |
//! | 2 | `0x411510` (`0x411640`, `0x411810`) | **burst**: every polygon flies out from the body's centre (or a point near the feet), each at its own random speed, optionally tumbling; a white flash on the first tick, random flicker; spins the owner's heading. Backwards: the shards fly in |
//! | 3 | `0x409c50` (`0x409cb0`, `0x409da0`) | **squash**: the body is flattened onto the ground (y scaled about `height − 10`); backwards it springs up (`256·(1 − cos 6v · e^−v)`) or grows from nothing |
//! | 4 | `0x411360` (`0x4113b0`, `0x4113e0`) | **jolt**: every vertex jitters by `rand & mask`, the body flickers bright/dark; then *chains* into a kind 2 burst |
//! | 5 | `0x4166c0` (`0x416850`, `0x416980`) | **melt**: glows white over 45 ticks, then every polygon tumbles and drops to the floor, bouncing (each bounce halves its speed) until all lie still |
//! | 6 | `0x42dba0` (`0x42dc00`, `0x42dca0`) | **stretch**: volume-preserving squash/stretch (y by `s`, x and z by `√(256/s)`). Not used on the CD |
//! | 7 | `0x429a60` (`0x429bc0`, `0x429d10`) | **shatter**: the polygons fly up and out from a point under the body, fall under gravity, land on the floor and stay; a clink for each landing |
//!
//! Every sub-effect (base class `0x425680`, vtable `0x460ba0`) runs
//! `pre` idle ticks, `duration` steps (playing the manager's pending
//! sound on the first one), `post` idle ticks, then restores the owner
//! (`0x425790`) and reports "done" (`0x425800`). While it runs the owner
//! draws the effect's private copy of its mesh (`0x4105d0`: one vertex
//! per polygon corner, one material per polygon, swapped in by
//! `0x4256f0`), so the pose is frozen and the pieces can come apart.
//! Kinds 5 and 7 keep themselves alive (duration back to 2) until every
//! piece has settled. Kind 4 v0..v2 and kind 6 v0 queue a follow-up
//! effect (`+0x10/+0x12`), started by the manager's update `0x424960`
//! the tick the first one ends.
//!
//! inferred: the exact rotation order of `0x410f90`'s per-polygon matrix
//! (`0x430430`; the angles are random, so only the look matters); kind
//! 1's sort keys are taken at the start, the exe takes them when the
//! creature is built (`0x40f780`); terms `m2`/`m3` of the per-polygon
//! material (kinds 2, 4, 5, 7 write them too) are not modelled, only
//! the brightness that replaces `m0`/`m1` (see [`Frame::bright`]).

use crate::sim::{apply, transform};

/// Brightness as `0x4111b0` takes it: below 0x8000 `m0 = 2b/65535`,
/// above, `m0 = 1` and `m1 = (b − 0x7fff)/2/65535` (whiter).
pub type Bright = u16;

/// The owner's polygons as the effect copies them at its start
/// (`0x4109a0`): model-space corners, and each polygon's material as a
/// brightness (`0x411240`: `m0/2 + 2·m1`, what kind 5 brightens from).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Shape {
    pub polys: Vec<Vec<[i32; 3]>>,
    pub bright: Vec<Bright>,
}

impl Shape {
    fn len(&self) -> usize {
        self.polys.len()
    }
}

/// What the owner draws while an effect runs: the effect mesh.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Polygon `+0x64`: drawn or not (`0x411340`).
    pub visible: Vec<bool>,
    /// The polygon's material brightness, `None` = the section material.
    pub bright: Vec<Option<Bright>>,
    /// Model-space corners, polygon by polygon.
    pub polys: Vec<Vec<[i32; 3]>>,
    /// Object `+0x2a`: back faces culled. Kinds 2, 5, 6, 7 draw both
    /// sides; kind 1 culls only with flag 4. That flag drives the software
    /// renderer's test (`0x402490`) and the Direct3D *glass* bucket only:
    /// textured buckets are always culled counter-clockwise (`0x40f000`),
    /// so in the Direct3D look we draw (the footage's), Mendel's and the
    /// predators' textured shards stay one-sided whatever this says.
    pub culling: bool,
}

/// A sound the effect plays: the manager's cue template (`+0x74`, copied
/// to `+0x38`): a 1-based world-bank sound, pitch and pitch variation
/// (16.16), distances 3000/30000, volume 256, heard at the owner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cue {
    pub sound: i16,
    pub pitch: u32,
    pub variation: u32,
    pub near: i32,
    pub far: i32,
    pub volume: i32,
}

impl Cue {
    const fn of(sound: i16) -> Cue {
        Cue { sound, pitch: 0x10000, variation: 0, near: 3000, far: 30000, volume: 0x100 }
    }
}

/// What the effect asks of the game this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Out {
    /// `Palette_Play` @ 0x438830(id, 200).
    Flash(u8),
    Sound(Cue),
}

/// The base class's counters (`+0x04`, `+0x28`, `+0x30`, `+0x34`, `+0x36`).
#[derive(Clone, Debug, Default, PartialEq)]
struct Base {
    active: bool,
    duration: i16,
    phase: u8,
    pre: i16,
    post: i16,
}

#[derive(Clone, Debug, PartialEq)]
struct Sub {
    base: Base,
    fx: Fx,
    orig: Shape,
    frame: Frame,
    /// The short a kind 2 effect spins (Mendel's heading, a predator's
    /// `+0x74`), written every step.
    spun: Option<i32>,
}

#[derive(Clone, Debug, PartialEq)]
enum Fx {
    Fade(Fade),
    Burst(Burst),
    Squash(Squash),
    Jolt(Jolt),
    Melt(Melt),
    Stretch(Stretch),
    Shatter(Shatter),
}

/// The manager (`0x424520`, 0xb4 bytes).
#[derive(Clone, Debug, Default)]
pub struct Transition {
    /// `+0xc`, `+0xe`: kind and variant; `+0x10/+0x12` the queued next.
    pub kind: u16,
    pub variant: u16,
    next: (u16, u16),
    /// `+0xb0`: the owner's height − 10 (Mendel: his stance height
    /// `+0x7d4`; predators: 0x88).
    floor: i16,
    /// `+0x38`: the sound waiting for the first step.
    cue: Option<Cue>,
    sub: Option<Sub>,
    out: Vec<Out>,
}

/// Two idle managers are the same, whatever finished effect they last
/// ran: only the reverse started the same tick reads its kind. (Saved
/// games keep no effect: one running when saved ends at the load.)
impl PartialEq for Transition {
    fn eq(&self, o: &Self) -> bool {
        match (self.frame(), o.frame()) {
            (None, None) => true,
            _ => (self.kind, self.variant, self.next, self.floor, self.cue, &self.sub) == (o.kind, o.variant, o.next, o.floor, o.cue, &o.sub),
        }
    }
}

/// Owner-side context for a tick: its shape (taken again whenever a
/// sub-effect starts), the short a burst spins, and the CRT `rand()`.
pub struct Ctx<'a> {
    pub shape: &'a dyn Fn() -> Shape,
    pub spin: i32,
    pub rand: &'a dyn Fn() -> u32,
}

impl Transition {
    /// `0x424870(kind, variant, height)` then `0x4249b0`.
    pub fn start(&mut self, kind: u16, variant: u16, height: i16, ctx: &Ctx) {
        self.kind = kind;
        self.variant = variant;
        self.next = (0, 0);
        self.floor = height - 10;
        self.begin(ctx);
    }

    /// `0x424870` then `0x4248c0`: the same effect, played backwards.
    pub fn start_reversed(&mut self, kind: u16, variant: u16, height: i16, ctx: &Ctx) {
        self.kind = kind;
        self.variant = variant;
        self.next = (0, 0);
        self.floor = height - 10;
        self.reverse(ctx);
    }

    /// `0x4248c0`.
    pub fn reverse(&mut self, ctx: &Ctx) {
        self.variant = self.variant.wrapping_add(0x40);
        match self.kind {
            3 => self.variant = if self.variant != 0x43 { 0x40 } else { 0x43 },
            4 => (self.kind, self.variant) = (2, 0x41),
            5 => (self.kind, self.variant) = (2, 0x44),
            6 => self.variant = 0x40,
            7 => (self.kind, self.variant) = (2, 0x40),
            _ => {}
        }
        self.begin(ctx);
    }

    /// `0x424960`: one tick; false once the effect (and its follow-up)
    /// has run out. The owner calls it every tick of its effect mode.
    pub fn update(&mut self, ctx: &Ctx) -> bool {
        let mut running = self.step_sub(ctx);
        if !running && self.next.0 != 0 && self.sub.is_some() {
            (self.kind, self.variant) = self.next;
            self.next = (0, 0);
            self.begin(ctx);
            running = self.step_sub(ctx);
        }
        running
    }

    /// The effect mesh, while a sub-effect has the owner's mesh swapped
    /// out (from its start to its end, delays included).
    pub fn frame(&self) -> Option<&Frame> {
        self.sub.as_ref().filter(|s| s.base.active).map(|s| &s.frame)
    }

    /// The value a running burst wrote into the owner's spun short.
    pub fn spin(&self) -> Option<i32> {
        self.sub.as_ref().filter(|s| s.base.active).and_then(|s| s.spun)
    }

    /// Flashes and sounds of the last calls, oldest first.
    pub fn take_out(&mut self) -> Vec<Out> {
        std::mem::take(&mut self.out)
    }

    // ——— 0x4249b0 and the per-kind starters ———

    fn begin(&mut self, ctx: &Ctx) {
        let (mut pre, mut post) = (0i16, 0i16);
        let shape = (ctx.shape)();
        let v = self.variant;
        let h = self.floor;
        let rand = ctx.rand;
        let mut flash = None;
        let mut cue: Option<Cue> = None;
        let fx = match self.kind {
            1 => {
                // 0x424ab0: (rate, n, flags, sound, flash).
                let (rate, n, flags, sound, fl): (f64, i16, u16, i16, i8) = match v {
                    0 => (0.3, 8, 0xd, 1, 3),
                    1 => (0.0001, 8, 5, 2, -1),
                    2 => (0.14, 4, 0x190, 3, -1),
                    5 => (0.7, 12, 0xa4, -1, 4),
                    6 => (0.3, 10, 0x24, 7, -1),
                    7 => (0.1, 6, 0x94, 8, -1),
                    0x40 => (0.3, 8, 0xf, 1, 3),
                    0x41 => (0.0001, 8, 7, 2, -1),
                    0x42 => (0.1, 3, 0x192, -1, -1),
                    0x45 => (0.7, 20, 0xa6, -1, 5),
                    0x46 => (0.3, 10, 0x26, 7, -1),
                    0x47 => (0.1, 6, 0x96, 9, -1),
                    _ => return self.none(),
                };
                if sound > 0 {
                    cue = Some(Cue::of(sound));
                }
                // v5: sound 6 at pitch 0xd000 over 10000/20000, played at
                // once; 0x45: pitch 0xb000, volume 190, after a 50-tick
                // wait (the cue waits for the first step).
                if v == 5 {
                    self.out.push(Out::Sound(Cue { pitch: 0xd000, near: 10000, far: 20000, ..Cue::of(6) }));
                }
                if v == 0x45 {
                    cue = Some(Cue { pitch: 0xb000, near: 10000, far: 20000, volume: 0xbe, ..Cue::of(6) });
                    pre = 50;
                }
                if fl >= 0 {
                    flash = Some(fl as u8);
                }
                Fx::Fade(Fade::new(&shape, rate, n, flags, rand))
            }
            2 => {
                // 0x424e20 (its jump table at 0x4250f8): duration, spread
                // a, tumble b, spin speed and acceleration, flags, sound,
                // flash.
                let (dur, a, b, vel, acc, flags, sound, fl): (u16, u32, u32, f64, f64, u16, i16, i8) = match v {
                    0 => (100, 2000, 2000, 0.0, 0.0, 0x16, 10, 6),
                    1 => (60, 700, 1200, 0.0, 0.0, 0x14, 11, 7),
                    2 => (100, 1400, 800, 0.0, 0.0, 0x34, 12, 8),
                    3 => (60, 2000, 500, 0.0, 0.0, 0x32, 13, 9),
                    4 => (80, 1800, 1, 0.0, 0.0, 0x32, 14, 10),
                    5 => (40, 300, 500, 6.0, 0.0, 0x14, 15, 11),
                    6 => (80, 1500, 800, 0.0, 0.0, 0x1e, 16, 12),
                    0x40 => (60, 600, 500, 0.0, 0.5, 0x19, 18, -1),
                    0x41 => (60, 300, 500, 0.0, 0.0, 0x0d, 18, -1),
                    0x42 => (60, 500, 800, 10.0, 0.0, 0x31, 18, -1),
                    0x43 => (60, 500, 400, 0.4, 0.0, 0x37, 18, -1),
                    0x44 => (60, 500, 800, 0.0, 0.5, 0x37, 18, -1),
                    0x45 => (60, 300, 600, 0.0, 0.5, 0x15, 18, -1),
                    0x46 => (60, 400, 600, 0.0, 0.0, 0x1d, 18, -1),
                    _ => return self.none(),
                };
                cue = Some(Cue { variation: 0x4000, ..Cue::of(sound) });
                if fl >= 0 {
                    flash = Some(fl as u8);
                }
                let (burst, base_dur) = Burst::new(&shape, dur, a, b, vel, acc, flags, h, ctx.spin, rand);
                return self.install(shape, Fx::Burst(burst), base_dur, pre, post, flash, cue, Some(ctx.spin));
            }
            3 => {
                // 0x425140: (duration, flags, sound, flash, post).
                let (dur, flags, sound, fl, p): (i16, u16, i16, i8, i16) = match v {
                    0 => (2, 0, 0x13, 0xd, 50),
                    1 => (60, 2, 0x14, -1, 40),
                    2 => (40, 4, 0x15, 0xe, 0),
                    3 => (1, 8, -1, -1, 0),
                    0x40 => (70, 1, 0x17, -1, 0),
                    0x43 => (60, 0x10, -1, -1, 0),
                    _ => return self.none(),
                };
                post = p;
                if sound > 0 {
                    cue = Some(Cue::of(sound));
                }
                if fl >= 0 {
                    flash = Some(fl as u8);
                }
                return self.install(shape, Fx::Squash(Squash::new(dur, flags, h)), dur, pre, post, flash, cue, None);
            }
            4 => {
                // 0x425280: (duration, mask, flash, sound) and the follow-up.
                let (dur, mask, fl, sound) = match v {
                    0 => (30, 0x3f, 0xf, 0x18),
                    1 => (62, 0x1f, 0x10, 0x19),
                    2 => (21, 0x7f, 0x11, 0x1a),
                    3 => (38, 0x3f, 0xf, 0x18),
                    _ => return self.none(),
                };
                match v {
                    0 => self.next = (2, 4),
                    1 => self.next = (2, 3),
                    2 => self.next = (2, 0),
                    _ => post = 60,
                }
                return self.install(shape, Fx::Jolt(Jolt { mask, bright: 0 }), dur, pre, post, Some(fl), Some(Cue::of(sound)), None);
            }
            5 => {
                if v != 0 {
                    return self.none();
                }
                // 0x4249b0 case 5: 0x416850(45, height), sound 0x23.
                let (melt, dur) = Melt::new(&shape, 45, h, rand);
                return self.install(shape, Fx::Melt(melt), dur, pre, post, None, Some(Cue::of(0x23)), None);
            }
            6 => {
                // 0x425370: (duration, flags, sound, flash, post).
                let (dur, flags, sound, fl, p): (i16, u16, i16, i8, i16) = match v {
                    0 => (65, 0, 0x1c, -1, 0),
                    1 => (0, 2, 0x1e, 0x13, 60),
                    0x40 | 0x41 => (100, 1, 0x1d, 0x12, 0),
                    _ => return self.none(),
                };
                if v == 0 {
                    self.next = (6, 1);
                }
                post = p;
                if fl >= 0 {
                    flash = Some(fl as u8);
                }
                let (st, dur) = Stretch::new(dur, flags, h);
                let mut sub_frame = Frame::of(&shape, true);
                if flags & 2 != 0 {
                    sub_frame.visible.iter_mut().for_each(|v| *v = false);
                }
                self.install(shape, Fx::Stretch(st), dur, pre, post, flash, Some(Cue::of(sound)), None);
                if let Some(s) = &mut self.sub {
                    s.frame.visible = sub_frame.visible;
                }
                return;
            }
            7 => {
                // 0x425480: (gravity, spread step, tumble step, flags, clink
                // delay, sound, flash); post 30 for all.
                let (g, da, db, flags, delay, sound, fl): (i16, i32, i32, u16, i16, i16, u8) = match v {
                    0 => (6, 8, 5, 0, 30, 0x1f, 0x14),
                    1 => (30, 16, 5, 0, 12, 0x20, 0x15),
                    2 => (8, 2, 5, 4, 6, 0x21, 0x16),
                    3 => (1, 3, 1, 4, 6, 0x1f, 0x16),
                    _ => return self.none(),
                };
                post = 30;
                let st = Shatter::new(&shape, g, da, db, flags, delay, h, rand);
                return self.install(shape, Fx::Shatter(st), 4, pre, post, Some(fl), Some(Cue::of(sound)), None);
            }
            _ => return self.none(),
        };
        // Kind 1 only (the others returned).
        let dur = match &fx {
            Fx::Fade(f) => f.duration,
            _ => 0,
        };
        self.install(shape, fx, dur, pre, post, flash, cue, None);
    }

    /// Kind 0 or an unknown variant: nothing runs (the exe keeps its old,
    /// finished sub-effect, whose update says "done").
    fn none(&mut self) {
        self.sub = None;
    }

    #[allow(clippy::too_many_arguments)]
    fn install(&mut self, shape: Shape, fx: Fx, duration: i16, pre: i16, post: i16, flash: Option<u8>, cue: Option<Cue>, spun: Option<i32>) {
        let culling = match &fx {
            Fx::Fade(f) => f.flags & 4 != 0,
            Fx::Burst(_) | Fx::Melt(_) | Fx::Shatter(_) => false,
            // 3, 4 and 6 leave the owner's `+0x2a` alone (Mendel: 1).
            _ => true,
        };
        let mut frame = Frame::of(&shape, culling);
        if let Fx::Fade(f) = &fx {
            for (i, v) in f.start_hidden.iter().enumerate() {
                if *v {
                    frame.visible[i] = false;
                }
            }
        }
        if let Some(f) = flash {
            self.out.push(Out::Flash(f));
        }
        self.cue = cue;
        self.sub = Some(Sub {
            base: Base { active: true, duration, phase: 0, pre, post },
            fx,
            orig: shape,
            frame,
            spun,
        });
    }

    // ——— 0x425800, the base update ———

    fn step_sub(&mut self, ctx: &Ctx) -> bool {
        let Some(s) = self.sub.as_mut() else { return false };
        let b = &mut s.base;
        if !b.active {
            return false;
        }
        if b.phase == 0 {
            if b.pre > 0 {
                b.pre -= 1;
                return true;
            }
            b.phase = 1;
        }
        if b.phase == 1 {
            let d = b.duration;
            b.duration -= 1;
            if d > 0 {
                if let Some(c) = self.cue.take() {
                    self.out.push(Out::Sound(c));
                }
                let left = s.base.duration;
                let mut out = Vec::new();
                let keep = s.step(left, ctx.rand, &mut out);
                if let Some(d) = keep {
                    s.base.duration = d;
                }
                self.out.extend(out);
                return true;
            }
            b.phase = 2;
        }
        if b.phase == 2 {
            if b.post > 0 {
                b.post -= 1;
                return true;
            }
            b.phase = 3;
            // 0x425790 (+ the culling restore of 0x429cf0): the owner gets
            // its own mesh back.
            b.active = false;
        }
        false
    }
}

impl Frame {
    fn of(shape: &Shape, culling: bool) -> Frame {
        Frame {
            visible: vec![true; shape.len()],
            bright: vec![None; shape.len()],
            polys: shape.polys.clone(),
            culling,
        }
    }

    fn set_all(&mut self, b: Option<Bright>) {
        self.bright.iter_mut().for_each(|x| *x = b);
    }
}

impl Sub {
    /// The kind's step (vtable slot 0x0c). `left` is the base's duration
    /// after its decrement; returns a new duration if the step reset it.
    fn step(&mut self, left: i16, rand: &dyn Fn() -> u32, out: &mut Vec<Out>) -> Option<i16> {
        let (orig, frame) = (&self.orig, &mut self.frame);
        match &mut self.fx {
            Fx::Fade(f) => {
                f.step(frame, rand);
                None
            }
            Fx::Burst(b) => {
                self.spun = Some(b.step(orig, frame, left, rand));
                None
            }
            Fx::Squash(s) => {
                s.step(orig, frame, rand);
                None
            }
            Fx::Jolt(j) => {
                j.step(orig, frame, left, rand);
                None
            }
            Fx::Melt(m) => m.step(orig, frame, left, out),
            Fx::Stretch(s) => {
                s.step(orig, frame);
                None
            }
            Fx::Shatter(s) => s.step(orig, frame, left, rand, out),
        }
    }
}

// ——— helpers: the effect mesh (0x4105d0) ———

/// `0x410c80(about_floor, random, h)`: per polygon, the mean of its
/// corners (`about_floor`: only `y − h`, x and z 0), plus `rand & 0xff −
/// 0x7f` on each axis if `random`.
fn centroids(shape: &Shape, about_floor: bool, random: bool, h: i32, rand: &dyn Fn() -> u32) -> Vec<[i32; 3]> {
    shape
        .polys
        .iter()
        .map(|p| {
            let n = p.len().max(1) as i32;
            let mut s = [0i32; 3];
            for v in p {
                if about_floor {
                    s[1] += v[1] - h;
                } else {
                    s = [s[0] + v[0], s[1] + v[1], s[2] + v[2]];
                }
            }
            let mut c = s.map(|x| (x / n) as i16 as i32);
            if random {
                for k in &mut c {
                    *k = (*k + (rand() & 0xff) as i32 - 0x7f) as i16 as i32;
                }
            }
            c
        })
        .collect()
}

/// `0x410f90(poly, {0, angles}, −c, t)`: the polygon turned by `angles`
/// (1024ths) about `c`, then moved so `c` lands on `t`.
fn turn_poly(src: &[[i32; 3]], dst: &mut [[i32; 3]], ang: [i32; 3], c: [i32; 3], t: [i32; 3]) {
    let m = transform([0; 3], ang);
    for (o, v) in dst.iter_mut().zip(src) {
        let p = apply(&m, [(v[0] - c[0]) as f32, (v[1] - c[1]) as f32, (v[2] - c[2]) as f32]);
        *o = [0, 1, 2].map(|k| ((p[k] as i32) + t[k]) as i16 as i32);
    }
}

// ——— kind 1: fade (0x40f780) ———

const IDLE: i32 = -10000;

#[derive(Clone, Debug, PartialEq)]
struct Fade {
    flags: u16,
    duration: i16,
    rate: f64,
    timer: f64,
    step: i32,
    next: usize,
    order: Vec<usize>,
    value: Vec<i32>,
    state: Vec<u8>,
    start_hidden: Vec<bool>,
}

impl Fade {
    /// `0x40fa50(rate, n, flags)`: one polygon starts every `rate` ticks,
    /// each takes `n` ticks; duration `N·rate + n − 1`.
    fn new(shape: &Shape, rate: f64, n: i16, flags: u16, rand: &dyn Fn() -> u32) -> Fade {
        let count = shape.len();
        let duration = (count as f64 * rate + n as f64 - 1.0) as i16;
        let back = flags & 2 != 0;
        let mut order: Vec<usize> = (0..count).map(|i| if back { count - 1 - i } else { i }).collect();
        let start_hidden = vec![back && flags & 0x100 == 0; count];
        let sort_flags = if back { flags ^ 0x80 } else { flags };
        if sort_flags & 1 != 0 {
            for i in 0..count {
                let j = (rand() as usize) % count;
                order.swap(i, j);
            }
        } else if let Some(axis) = [0x10, 0x20, 0x40].iter().position(|&b| sort_flags & b != 0) {
            // Highest corner on the axis, ascending (qsort with
            // `0x4100d0`); 0x80 reverses. The exe stores `i + 1` as each
            // entry's polygon (0x40f8b0), so polygon 0 is never picked and
            // the last pick points past the end: we keep the first and
            // drop the second, as harmless as the original's.
            let mut keys: Vec<(i32, usize)> =
                shape.polys.iter().enumerate().map(|(i, p)| (p.iter().map(|v| v[axis]).max().unwrap_or(i32::MIN), i + 1)).collect();
            keys.sort_by_key(|k| k.0);
            order = if sort_flags & 0x80 != 0 {
                keys.iter().rev().map(|k| k.1).collect()
            } else {
                keys.iter().map(|k| k.1).collect()
            };
        }
        Fade {
            flags,
            duration,
            rate,
            timer: 0.0,
            step: 0x7fff / n.max(1) as i32,
            next: 0,
            order,
            value: vec![IDLE; count],
            state: vec![0; count],
            start_hidden,
        }
    }

    fn shade(&self, v: i32, rand: &dyn Fn() -> u32) -> Bright {
        if self.flags & 8 != 0 {
            if (rand() as i32) <= v { 0xffff } else { 0 }
        } else {
            ((v as i16 as i32) << 1) as u16
        }
    }

    fn step(&mut self, frame: &mut Frame, rand: &dyn Fn() -> u32) {
        let n = self.value.len();
        if self.flags & 0x100 != 0 {
            // 0x40ff60: brighten to 30000, darken to 5000, stay.
            for i in 0..n {
                match self.state[i] {
                    1 => {
                        self.value[i] += self.step;
                        if self.value[i] >= 30001 {
                            self.value[i] = 30000;
                            self.state[i] = 2;
                        }
                        frame.bright[i] = Some(((self.value[i] as i16 as i32) << 1) as u16);
                    }
                    2 => {
                        self.value[i] -= self.step;
                        if self.value[i] < 5000 {
                            self.value[i] = 5000;
                            self.state[i] = 3;
                        }
                        frame.bright[i] = Some(((self.value[i] as i16 as i32) << 1) as u16);
                    }
                    _ => {}
                }
            }
            self.timer -= 1.0;
            while self.timer <= 0.0 && self.next < n {
                self.timer += self.rate;
                let k = self.order[self.next];
                self.next += 1;
                if k < n {
                    self.value[k] = 0;
                    self.state[k] = 1;
                }
            }
        } else if self.flags & 2 != 0 {
            // 0x40fe50: show, then brighten from black.
            self.timer -= 1.0;
            while self.timer <= 0.0 && self.next < n {
                self.timer += self.rate;
                let k = self.order[self.next];
                self.next += 1;
                if k < n {
                    frame.visible[k] = true;
                    self.value[k] = 0;
                }
            }
            for i in 0..n {
                let v = self.value[i];
                if v != IDLE && v < 30001 {
                    self.value[i] = v + self.step;
                    frame.bright[i] = Some(self.shade(self.value[i], rand));
                }
            }
        } else {
            // 0x40fd40: fade from the glow, gone below 5000.
            for i in 0..n {
                let v = self.value[i];
                if v == IDLE {
                    continue;
                }
                if v < 5000 {
                    frame.visible[i] = false;
                } else {
                    self.value[i] = v - self.step;
                    frame.bright[i] = Some(self.shade(self.value[i], rand));
                }
            }
            self.timer -= 1.0;
            while self.timer <= 0.0 && self.next < n {
                self.timer += self.rate;
                let k = self.order[self.next];
                self.next += 1;
                if k < n {
                    self.value[k] = 0x7fff;
                }
            }
        }
    }
}

// ——— kind 2: burst (0x411510) ———

#[derive(Clone, Debug, PartialEq)]
struct Burst {
    flags: u16,
    /// `+0x50/+0x54`, `+0x58/+0x5c`: spread and tumble (8.8) and steps.
    a: i32,
    da: i32,
    b: i32,
    db: i32,
    /// `+0x44/+0x48/+0x4c`: the spun short (<< 8), its speed, acceleration.
    spin: i32,
    spin_v: i32,
    spin_a: i32,
    /// `+0x60`: the tick (duration left) of the white flash; `+0x64` set
    /// while the flash shows.
    flash_at: i32,
    flashing: bool,
    centre: Vec<[i32; 3]>,
    tumble: Vec<[i32; 3]>,
    speed: Vec<i32>,
}

impl Burst {
    #[allow(clippy::too_many_arguments)]
    fn new(shape: &Shape, dur: u16, a: u32, b: u32, vel: f64, acc: f64, flags: u16, h: i16, spun: i32, rand: &dyn Fn() -> u32) -> (Burst, i16) {
        let n = dur as u32;
        let v = (vel * 256.0) as i32;
        let ac = (acc * 256.0) as i32;
        let step = |x: u32| (x.wrapping_sub(0x100) / n) as i32;
        let mut s = Burst {
            flags,
            a: 0x100,
            da: step(a),
            b: 0x100,
            db: step(b),
            spin: (spun as i16 as i32) << 8,
            spin_v: v,
            spin_a: ac,
            flash_at: -1,
            flashing: false,
            centre: Vec::new(),
            tumble: Vec::new(),
            speed: Vec::new(),
        };
        let n = n as i32;
        if flags & 1 == 0 {
            if flags & 0x10 != 0 {
                s.flash_at = n - 1;
            }
        } else {
            // Backwards: start where the forward run ends, end at rest.
            s.spin = s.spin.wrapping_add(n.wrapping_mul(v)).wrapping_add(n.wrapping_mul(n).wrapping_mul(ac) / 2);
            s.spin_v = -((n - 1).wrapping_mul(ac) + v);
            s.da = -s.da;
            s.a = 0x100i32.wrapping_sub(s.da.wrapping_mul(n));
            s.db = -s.db;
            s.b = 0x100i32.wrapping_sub(s.db.wrapping_mul(n));
            if flags & 0x10 != 0 {
                s.flash_at = 0;
            }
        }
        s.centre = centroids(shape, flags & 8 != 0, true, h as i32, rand);
        s.tumble = (0..shape.len()).map(|_| [0; 3].map(|_| (rand() & 0x1ff) as i32)).collect();
        s.speed = (0..shape.len()).map(|_| (rand() & 0xff) as i32).collect();
        (s, dur as i16)
    }

    /// `0x411810`; returns the spun short's new value.
    fn step(&mut self, orig: &Shape, frame: &mut Frame, left: i16, rand: &dyn Fn() -> u32) -> i32 {
        if left as i32 == self.flash_at {
            frame.set_all(Some(0xffff));
            self.flashing = true;
        } else if self.flashing {
            frame.set_all(None); // 0x410c20: the section materials again
            self.flashing = false;
        } else if self.flags & 2 != 0 {
            for b in &mut frame.bright {
                *b = Some((rand() << 1) as u16);
            }
        }
        for (i, src) in orig.polys.iter().enumerate() {
            let c = self.centre[i];
            let k = self.a.wrapping_add(((self.speed[i].wrapping_mul(self.a - 0x100)) as u32 >> 5) as i32);
            let mut d = c.map(|x| ((x * k) >> 8) as i16 as i32);
            if self.flags & 0x20 != 0 {
                d[1] = (d[1] as i16 >> 3) as i32;
            }
            if self.flags & 4 != 0 {
                let ang = self.tumble[i].map(|r| ((r.wrapping_mul(self.b - 0x100) as u32) >> 8) as i16 as i32);
                turn_poly(src, &mut frame.polys[i], ang, c, d);
            } else {
                for (o, v) in frame.polys[i].iter_mut().zip(src) {
                    *o = [0, 1, 2].map(|j| (v[j] + d[j] - c[j]) as i16 as i32);
                }
            }
        }
        self.a = self.a.wrapping_add(self.da);
        self.b = self.b.wrapping_add(self.db);
        let out = self.spin >> 8;
        self.spin = self.spin.wrapping_add(self.spin_v);
        self.spin_v = self.spin_v.wrapping_add(self.spin_a);
        out as i16 as i32
    }
}

// ——— kind 3: squash (0x409c50) ———

#[derive(Clone, Debug, PartialEq)]
struct Squash {
    flags: u16,
    floor: i32,
    value: f64,
    step: f64,
}

impl Squash {
    /// `0x409cb0(duration, flags, h)`.
    fn new(dur: i16, flags: u16, h: i16) -> Squash {
        let d = dur.max(1) as f64;
        let (value, step) = if flags & 2 != 0 {
            (256.0, 256.0 / d)
        } else if flags & 1 != 0 {
            (0.0, 5.0 / d)
        } else if flags & 0x10 != 0 {
            (0.0, 256.0 / d)
        } else {
            (0.0, 0.0)
        };
        Squash { flags, floor: h as i32, value, step }
    }

    /// `0x409da0`.
    fn step(&mut self, orig: &Shape, frame: &mut Frame, rand: &dyn Fn() -> u32) {
        if self.flags & 0x10 != 0 {
            scale(orig, frame, self.value as i32, 7, None);
            self.value += self.step;
            for b in &mut frame.bright {
                *b = Some((rand() << 1) as u16);
            }
        } else if self.flags & 1 != 0 {
            let v = self.value;
            let s = ((1.0 - (6.0 * v).cos() * (-v).exp()) * 256.0) as i32;
            scale(orig, frame, s, 2, Some(self.floor));
            self.value += self.step;
        } else {
            scale(orig, frame, self.value as i32, 2, Some(self.floor));
            self.value -= self.step;
        }
    }
}

/// `0x409e90(s, axes, about)`: corners scaled by `s/256` on the axes of
/// the mask; y about `floor` if given.
fn scale(orig: &Shape, frame: &mut Frame, s: i32, axes: u8, floor: Option<i32>) {
    for (dst, src) in frame.polys.iter_mut().zip(&orig.polys) {
        for (o, v) in dst.iter_mut().zip(src) {
            if axes & 1 != 0 {
                o[0] = ((v[0] * s) >> 8) as i16 as i32;
            }
            if axes & 2 != 0 {
                o[1] = match floor {
                    Some(h) => ((((v[1] - h) * s) >> 8) + h) as i16 as i32,
                    None => ((v[1] * s) >> 8) as i16 as i32,
                };
            }
            if axes & 4 != 0 {
                o[2] = ((v[2] * s) >> 8) as i16 as i32;
            }
        }
    }
}

// ——— kind 4: jolt (0x411360) ———

#[derive(Clone, Debug, PartialEq)]
struct Jolt {
    mask: u16,
    bright: u16,
}

impl Jolt {
    /// `0x4113e0`.
    fn step(&mut self, orig: &Shape, frame: &mut Frame, left: i16, rand: &dyn Fn() -> u32) {
        if left > 2 {
            self.bright = if self.bright < 0x8000 { (rand() + 0x7fff) as u16 } else { rand() as u16 };
        } else {
            self.bright = 0x6000;
            self.mask = 0x1f;
        }
        let m = self.mask as i32;
        for (dst, src) in frame.polys.iter_mut().zip(&orig.polys) {
            for (o, v) in dst.iter_mut().zip(src) {
                for k in 0..3 {
                    o[k] = (v[k] + ((rand() as i32) & m) - (m >> 1)) as i16 as i32;
                }
            }
        }
        frame.set_all(Some(self.bright));
    }
}

// ——— kind 5: melt (0x4166c0) ———

#[derive(Clone, Debug, PartialEq)]
struct Melt {
    floor: i32,
    ramp: i32,
    falling: bool,
    t: i32,
    centre: Vec<[i32; 3]>,
    tumble: Vec<[i32; 3]>,
    /// `+0x40`: 0 = tumbling, −1 = just hit, else the tick it hit.
    hit: Vec<i32>,
    drop: Vec<i32>,
    speed: Vec<i32>,
}

impl Melt {
    /// `0x416850(duration, h)`: the base runs `duration + 2`.
    fn new(shape: &Shape, dur: i16, h: i16, rand: &dyn Fn() -> u32) -> (Melt, i16) {
        let n = shape.len();
        let tumble = (0..n).map(|_| [0; 3].map(|_| ((rand() & 0xf) as i32 - 8) as i8 as i32)).collect();
        let m = Melt {
            floor: h as i32,
            ramp: 60000 / dur as i32,
            falling: false,
            t: 0,
            centre: centroids(shape, false, false, h as i32, rand),
            tumble,
            hit: vec![0; n],
            drop: vec![1; n],
            speed: vec![1; n],
        };
        (m, dur + 2)
    }

    /// `0x416980`; returns the base's new duration.
    fn step(&mut self, orig: &Shape, frame: &mut Frame, left: i16, out: &mut Vec<Out>) -> Option<i16> {
        if !self.falling {
            for (i, b) in frame.bright.iter_mut().enumerate() {
                let cur = b.unwrap_or(orig.bright[i]) as i32;
                *b = Some((cur + self.ramp).min(0xea60) as u16);
            }
            if left == 2 {
                // 0x4255c0(0x24, 0, now)
                out.push(Out::Sound(Cue::of(0x24)));
                self.falling = true;
            }
            return None;
        }
        let n = orig.len();
        for i in 0..n {
            self.drop[i] = (self.drop[i] + self.speed[i]) as i16 as i32;
            self.speed[i] = (self.speed[i] + 1) as i16 as i32;
        }
        self.t += 1;
        for i in 0..n {
            let at = if self.hit[i] == -1 {
                self.hit[i] = self.t;
                self.t
            } else if self.hit[i] == 0 {
                self.t
            } else {
                self.hit[i]
            };
            let ang = self.tumble[i].map(|r| (r * at) as i16 as i32);
            let c = self.centre[i];
            turn_poly(&orig.polys[i], &mut frame.polys[i], ang, c, c);
        }
        let mut still = true;
        for i in 0..n {
            let mut below = false;
            for o in &mut frame.polys[i] {
                o[1] = (o[1] + self.drop[i]) as i16 as i32;
                below |= o[1] > self.floor;
            }
            if below {
                self.speed[i] = (self.speed[i] as i16 >> 1) as i32;
                if self.hit[i] == 0 {
                    self.hit[i] = -1;
                }
                if self.speed[i] > 0 {
                    self.speed[i] = -self.speed[i];
                }
            }
            still &= matches!(self.speed[i], -1..=1);
        }
        Some(if still { 0 } else { 2 })
    }
}

// ——— kind 6: stretch (0x42dba0), not on the CD ———

#[derive(Clone, Debug, PartialEq)]
struct Stretch {
    flags: u16,
    floor: i32,
    value: i32,
    step: i32,
}

impl Stretch {
    /// `0x42dc00(duration, flags, h)`; flag 2 hides it at once.
    fn new(dur: i16, flags: u16, h: i16) -> (Stretch, i16) {
        let d = dur.max(1) as i32;
        let (value, step) = if flags & 1 != 0 { (0x32, 0x500 / d) } else { (0x100, 0xa00 / d) };
        (Stretch { flags, floor: h as i32, value, step }, if flags & 2 != 0 { 0 } else { dur })
    }

    /// `0x42dca0` → `0x42dd80(s)`.
    fn step(&mut self, orig: &Shape, frame: &mut Frame) {
        let s = if self.flags & 1 != 0 {
            let x = self.value as f64 / 256.0;
            let y = 1.0 - (9.0 * x).sin() * (-0.5 / (x * x));
            if y > 1.0 { (y * 256.0) as i32 } else { ((y - 1.0).exp() * 256.0) as i32 }
        } else {
            self.value
        };
        let s = s.max(1);
        let f = ((256.0 / s as f64).sqrt() * 256.0) as i32;
        for (dst, src) in frame.polys.iter_mut().zip(&orig.polys) {
            for (o, v) in dst.iter_mut().zip(src) {
                o[0] = ((v[0] * f) >> 8) as i16 as i32;
                o[1] = ((((v[1] - self.floor) * s) >> 8) + self.floor) as i16 as i32;
                o[2] = ((v[2] * f) >> 8) as i16 as i32;
            }
        }
        self.value += self.step;
    }
}

// ——— kind 7: shatter (0x429a60) ———

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Piece {
    y: i32,
    vy: i32,
    b: i32,
    a: i32,
    /// 0 flying, 1 just landed, 2 down.
    state: u8,
}

#[derive(Clone, Debug, PartialEq)]
struct Shatter {
    gravity: i32,
    a: i32,
    da: i32,
    b: i32,
    db: i32,
    flags: u16,
    delay: i16,
    floor: i32,
    flash_at: i32,
    flashing: bool,
    centre: Vec<[i32; 3]>,
    tumble: Vec<[i32; 3]>,
    speed: Vec<i32>,
    pieces: Vec<Piece>,
}

impl Shatter {
    /// `0x429bc0(g, da, db, flags, delay, h)` (the base runs 4 ticks, kept
    /// alive by the step).
    #[allow(clippy::too_many_arguments)]
    fn new(shape: &Shape, g: i16, da: i32, db: i32, flags: u16, delay: i16, h: i16, rand: &dyn Fn() -> u32) -> Shatter {
        let n = shape.len();
        // From a point 4h below the polygons' mean height.
        let centre = centroids(shape, true, false, (h as i32) << 2, rand);
        let tumble = (0..n).map(|_| [0; 3].map(|_| (rand() & 0x1ff) as i32)).collect();
        let speed = (0..n).map(|_| (rand() & 0xff) as i32).collect();
        Shatter {
            gravity: g as i32,
            a: 0x100,
            da,
            b: 0x100,
            db,
            flags,
            delay,
            floor: h as i32,
            flash_at: if flags & 2 != 0 { 3 } else { -1 },
            flashing: false,
            centre,
            tumble,
            speed,
            pieces: vec![Piece::default(); n],
        }
    }

    /// `0x429d10`; returns the base's new duration while pieces fly.
    fn step(&mut self, orig: &Shape, frame: &mut Frame, left: i16, rand: &dyn Fn() -> u32, out: &mut Vec<Out>) -> Option<i16> {
        self.delay = self.delay.wrapping_sub(1);
        if left as i32 == self.flash_at {
            frame.set_all(Some(0xffff));
            self.flashing = true;
        } else if self.flashing {
            frame.set_all(None);
            self.flashing = false;
        } else if self.flags & 1 != 0 {
            for b in &mut frame.bright {
                *b = Some((rand() << 1) as u16);
            }
        }
        // The landing clink: sound 0x22 at pitch 0xb000 ± 0.25; the exe
        // draws its `+0x22` (50 − rand & 31) every tick.
        let _ = rand();
        let clink = Cue { pitch: 0xb000, variation: 0x4000, ..Cue::of(0x22) };
        let mut all_down = true;
        for i in 0..orig.len() {
            let p = &mut self.pieces[i];
            p.vy = (p.vy + self.gravity) as i16 as i32;
            p.y = (p.y + p.vy) as i16 as i32;
            let (ang, k) = match p.state {
                0 => {
                    all_down = false;
                    let ang = self.tumble[i].map(|r| ((r.wrapping_mul(self.b - 0x100) as u32) >> 8) as i16 as i32);
                    (ang, self.a + ((self.speed[i].wrapping_mul(self.a - 0x100) as u32) >> 5) as i32)
                }
                _ => {
                    if p.state == 1 {
                        (p.b, p.a) = (self.b, self.a);
                        p.state = 2;
                    }
                    let ang = self.tumble[i].map(|r| ((r * (p.b - 0x100)) >> 8) as i16 as i32);
                    (ang, p.a + ((self.speed[i] * (p.a - 0x100)) >> 5))
                }
            };
            let c = self.centre[i];
            let mut d = c.map(|x| ((x * k) >> 8) as i16 as i32);
            if self.flags & 4 != 0 {
                d[0] = (d[0] << 2) as i16 as i32;
                d[2] = (d[2] << 2) as i16 as i32;
            }
            d[1] = (d[1] + p.y) as i16 as i32;
            turn_poly(&orig.polys[i], &mut frame.polys[i], ang, c, d);
            for o in &mut frame.polys[i] {
                if o[1] > self.floor {
                    if p.state == 0 {
                        p.state = 1;
                        if self.delay < 0 {
                            out.push(Out::Sound(clink));
                        }
                    }
                    o[1] = self.floor;
                }
            }
        }
        self.a += self.da;
        self.b += self.db;
        (!all_down).then_some(2)
    }
}

// ——— the owners' side ———

/// Hands this tick's flashes and sounds to the game: `0x438830(id, 200)`
/// and a host [`crate::sim::EV_EFFECT_SOUND`] heard at the owner
/// (`object` = 1-based id, 0 = Mendel).
pub(crate) fn emit(t: &mut Transition, sim: &mut crate::sim::Sim, object: i32) {
    for o in t.take_out() {
        match o {
            Out::Flash(id) => sim.flash.play(id, 200),
            Out::Sound(c) => sim.events.push(crate::sim::HostEvent {
                kind: crate::sim::EV_EFFECT_SOUND,
                a: c.sound as i32 - 1,
                b: object,
                c: ((c.pitch >> 4) & 0x1fff) as i32
                    | (((c.variation >> 8) & 0xff) as i32) << 13
                    | ((c.volume >> 2) & 0x7f) << 21
                    | ((c.far != 30000) as i32) << 28,
            }),
        }
    }
}

/// `0x411240`: a material's `(m0, m1)` read back as a brightness.
pub(crate) fn bright_of(m: [u16; 6]) -> Bright {
    (m[0] >> 1).wrapping_add(m[1].wrapping_mul(2))
}

/// Mendel's posed body as the effect copies it: corner positions per face.
pub(crate) fn shape_of_posed(mesh: &galapagos_formats::creature::PosedMesh, material: impl Fn(u16) -> [u16; 6]) -> Shape {
    Shape {
        polys: mesh.faces.iter().map(|f| f.indices.iter().map(|&i| mesh.vertices[i as usize]).collect()).collect(),
        bright: mesh.faces.iter().map(|f| bright_of(material(f.color_ramp))).collect(),
    }
}

impl Transition {
    /// Mendel's mesh and look while the effect has them: one vertex per
    /// face corner at the effect's positions, hidden faces emptied (the
    /// draw skips faces under three corners), the effect's brightness in
    /// place of the affliction's (the effect mesh has its own materials).
    pub(crate) fn dress(&self, mesh: galapagos_formats::creature::PosedMesh, look: &crate::affliction::Look) -> (galapagos_formats::creature::PosedMesh, crate::affliction::Look) {
        let Some(f) = self.frame().filter(|f| f.polys.len() == mesh.faces.len()) else { return (mesh, look.clone()) };
        let mut vertices = Vec::new();
        let mut faces = mesh.faces;
        for (k, face) in faces.iter_mut().enumerate() {
            let corners = &f.polys[k];
            if !f.visible[k] || corners.len() != face.indices.len() {
                face.indices.clear();
                continue;
            }
            let first = vertices.len() as u16;
            vertices.extend_from_slice(corners);
            face.indices = (first..first + corners.len() as u16).collect();
        }
        let bright = f.bright.iter().map(|b| b.unwrap_or(crate::affliction::NORMAL)).collect();
        (galapagos_formats::creature::PosedMesh { vertices, faces }, crate::affliction::Look { bright, texture: look.texture })
    }
}

/// A predator's polygons in the section's vertex buffer (object space),
/// painted with the effect while it runs and put back after.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Skin {
    saved: Option<(usize, Vec<f32>)>,
}

use crate::world::{WorldMesh, FLOATS_PER_MATERIAL, FLOATS_PER_VERTEX};

impl Skin {
    /// The object's polygons, rebuilt from their triangle fans.
    pub(crate) fn shape(mesh: &WorldMesh, draw: usize) -> Shape {
        let mut s = Shape::default();
        for r in &mesh.poly_ranges[draw] {
            let v = |i: u32| {
                let o = (r.first + i) as usize * FLOATS_PER_VERTEX;
                [0, 1, 2].map(|k| mesh.vertices[o + k] as i32)
            };
            let mut p = Vec::new();
            if r.count >= 3 {
                p.extend([v(0), v(1), v(2)]);
                for t in 1..r.count / 3 {
                    p.push(v(t * 3 + 2));
                }
            }
            s.polys.push(p);
            // The material's (m0, m1) back as a brightness: shader terms
            // m0 + m2·ambient (ambient is 0 on the CD) and m1.
            let m = (r.count > 0)
                .then(|| mesh.vertices[r.first as usize * FLOATS_PER_VERTEX + 6])
                .filter(|&l| l >= 0.0)
                .map(|l| l as usize * FLOATS_PER_MATERIAL)
                .and_then(|o| mesh.materials.get(o..o + 2))
                .map_or([0xffff, 0], |m| [(m[0] * 65535.0) as u16, (m[1] * 65535.0) as u16]);
            s.bright.push(bright_of([m[0], m[1], 0, 0, 0, 0]));
        }
        s
    }

    /// Writes the effect mesh into the object's vertices: corners moved,
    /// hidden polygons on layer −2, a replaced brightness pre-lit (the
    /// shader's level formula with the object's turned normal).
    pub(crate) fn paint(&mut self, frame: Option<&Frame>, mesh: &mut WorldMesh, draw: usize, sim: &mut crate::sim::Sim) {
        let r = mesh.draws[draw];
        let (a, b) = (r.first as usize * FLOATS_PER_VERTEX, (r.first + r.count) as usize * FLOATS_PER_VERTEX);
        let Some(f) = frame else {
            if let Some((d, saved)) = self.saved.take() {
                let r = mesh.draws[d];
                let a = r.first as usize * FLOATS_PER_VERTEX;
                mesh.vertices[a..a + saved.len()].copy_from_slice(&saved);
                sim.mark_dirty(r.first, r.first + r.count);
            }
            return;
        };
        if self.saved.is_none() {
            self.saved = Some((draw, mesh.vertices[a..b].to_vec()));
        }
        let Some((_, orig)) = &self.saved else { return };
        let rot = &mesh.transforms[draw * 16..draw * 16 + 16];
        let turn = |n: [f32; 3]| [0, 1, 2].map(|i| rot[i] * n[0] + rot[4 + i] * n[1] + rot[8 + i] * n[2]);
        let light = mesh.light;
        for (p, pr) in mesh.poly_ranges[draw].clone().iter().enumerate() {
            let Some(corners) = f.polys.get(p) else { continue };
            for t in 0..pr.count as usize {
                let o = (pr.first as usize) * FLOATS_PER_VERTEX - a + t * FLOATS_PER_VERTEX;
                let at = o + a;
                mesh.vertices[at..at + FLOATS_PER_VERTEX].copy_from_slice(&orig[o..o + FLOATS_PER_VERTEX]);
                // Fan corner of this triangle vertex: 0, k, k+1.
                let (tri, j) = (t / 3, t % 3);
                let c = match j {
                    0 => 0,
                    1 => tri + 1,
                    _ => tri + 2,
                };
                if let Some(v) = corners.get(c) {
                    mesh.vertices[at..at + 3].copy_from_slice(&v.map(|x| x as f32));
                }
                if !f.visible[p] {
                    mesh.vertices[at + 5] = -2.0;
                } else if let (Some(b), true) = (f.bright[p], orig[o + 6] >= 0.0) {
                    let mi = orig[o + 6] as usize * FLOATS_PER_MATERIAL;
                    let m = mesh.materials.get(mi..mi + 4).map_or([1.0, 0.0, 0.0, 0.0], |m| [m[0], m[1], m[2], m[3]]);
                    let (m0, m1) = if b < 0x8000 {
                        ((b as u32 * 2) as f32 / 65535.0, 0.0)
                    } else {
                        (1.0, ((b as u32 - 0x7fff) / 2) as f32 / 65535.0)
                    };
                    let n = turn([orig[o + 7], orig[o + 8], orig[o + 9]]);
                    let d = n[0] * light.dir[0] + n[1] * light.dir[1] + n[2] * light.dir[2];
                    let l = if d < 0.0 { m0 - m[2] * d } else { m0 };
                    let level = (((l / 2.0).min(0.5) + m1).min(1.0) * (crate::world::LEVELS - 1) as f32) as i32;
                    mesh.vertices[at + 6] = -1.0 - level as f32;
                    mesh.vertices[at + 7] = m[3];
                }
            }
        }
        sim.mark_dirty(r.first, r.first + r.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A test body: `n` unit squares in a column, model y down to 200.
    fn body(n: usize) -> Shape {
        let polys = (0..n)
            .map(|i| {
                let y = 200 - 20 * i as i32;
                let x = (i as i32 % 3) * 30 - 30;
                vec![[x, y, -10], [x + 20, y, -10], [x + 20, y - 20, 10], [x, y - 20, 10]]
            })
            .collect();
        Shape { polys, bright: vec![0x4000; n] }
    }

    fn lcg() -> impl Fn() -> u32 {
        let s = Cell::new(1u32);
        move || {
            let v = s.get().wrapping_mul(214013).wrapping_add(2531011);
            s.set(v);
            (v >> 16) & 0x7fff
        }
    }

    /// Runs an effect to its end; returns the ticks it took and the frames.
    fn run(kind: u16, variant: u16, reversed: bool) -> (usize, Vec<Frame>, Vec<Out>) {
        let shape = body(12);
        let rand = lcg();
        let shape_fn = || shape.clone();
        let ctx = Ctx { shape: &shape_fn, spin: 100, rand: &rand };
        let mut t = Transition::default();
        if reversed {
            t.start_reversed(kind, variant, 210, &ctx);
        } else {
            t.start(kind, variant, 210, &ctx);
        }
        let mut frames = Vec::new();
        let mut n = 0;
        while t.update(&ctx) {
            frames.push(t.frame().expect("a running effect has a frame").clone());
            n += 1;
            assert!(n < 5000, "kind {kind} variant {variant:#x} never ends");
        }
        assert!(t.frame().is_none(), "the owner gets its mesh back");
        (n, frames, t.take_out())
    }

    /// Every kind and variant the CD uses runs to an end, with the
    /// duration the tables give (pre + steps + post).
    #[test]
    fn every_used_effect_ends() {
        // Dying: lethal floors, falls, portals, bites.
        for (k, v) in [(1, 0), (1, 1), (1, 5), (1, 6), (1, 7), (2, 0), (2, 2), (2, 3), (2, 4), (2, 6), (3, 0), (3, 1), (4, 0), (5, 0), (7, 0)] {
            let (n, _, _) = run(k, v, false);
            assert!(n > 0, "kind {k} variant {v}");
        }
        // Materialising: the same, backwards, plus the two spawn pads.
        for (k, v) in [(1, 0), (1, 1), (1, 5), (1, 6), (1, 7), (2, 0), (2, 2), (2, 4), (3, 0), (3, 1), (3, 3), (4, 0), (5, 0), (7, 0)] {
            let (n, _, _) = run(k, v, true);
            assert!(n > 0, "reversed kind {k} variant {v}");
        }
    }

    #[test]
    fn durations_follow_the_tables() {
        // Kind 2 v0: 100 steps.
        assert_eq!(run(2, 0, false).0, 100);
        // Kind 3 v1: 60 steps + 40 held flat.
        assert_eq!(run(3, 1, false).0, 100);
        // Kind 1 v1 (12 polygons, rate 0.0001, 8 ticks each): 7 steps.
        assert_eq!(run(1, 1, false).0, 7);
        // Kind 4 v0: 30 ticks of jolt, then a kind 2 v4 burst of 80.
        assert_eq!(run(4, 0, false).0, 110);
        // Kind 1 0x45 (a teleport's arrival): 50 ticks wait + 12·0.7 + 19.
        assert_eq!(run(1, 5, true).0, 50 + (12.0f64 * 0.7 + 19.0) as usize);
    }

    #[test]
    fn fade_darkens_all_but_the_first_polygon_when_sorted() {
        // Kind 1 v7 (a teleport) sorts by x: the exe's 1-based picks never
        // reach polygon 0. Its 6 steps end before anything vanishes: the
        // body is dark when the arrival effect takes over.
        let (_, frames, _) = run(1, 7, false);
        let last = frames.last().unwrap();
        assert_eq!(last.bright[0], None);
        assert!(last.bright[1..].iter().all(|b| b.is_some_and(|b| b < 0x6000)), "{:?}", last.bright);
        // Random order (v0, the long fall): the first ones are gone by
        // the end, the last ones started too late and are only dark.
        let (_, frames, _) = run(1, 0, false);
        let gone = frames.last().unwrap().visible.iter().filter(|v| !**v).count();
        assert!((4..12).contains(&gone), "{gone}");
        // Backwards (0x47) everything starts hidden and polygon 0 is never
        // picked: it comes back only when the effect hands the body back.
        let (_, frames, _) = run(1, 7, true);
        assert!(frames.iter().all(|f| !f.visible[0]));
        assert!(frames.last().unwrap().visible[1..].iter().all(|v| *v));
    }

    #[test]
    fn burst_flies_apart_and_back() {
        let shape = body(12);
        let (_, frames, out) = run(2, 0, false);
        let spread = |f: &Frame| f.polys.iter().flatten().map(|v| v[0].abs() + v[2].abs()).max().unwrap();
        let rest = spread(&Frame::of(&shape, true));
        assert!(spread(frames.last().unwrap()) > 4 * rest, "pieces fly out");
        assert_eq!(frames[0].bright[0], Some(0xffff), "white flash on the first tick");
        assert!(out.contains(&Out::Flash(6)));
        assert!(out.iter().any(|o| matches!(o, Out::Sound(c) if c.sound == 10)));
        // Backwards (0x40): the shards come back to (one step short of)
        // where the body was.
        let (_, frames, _) = run(2, 0, true);
        assert!(spread(&frames[0]) > 2 * spread(frames.last().unwrap()));
        assert!(spread(frames.last().unwrap()) < 2 * rest);
    }

    #[test]
    fn squash_flattens_onto_the_floor() {
        let (_, frames, _) = run(3, 1, false);
        let f = frames.last().unwrap();
        // The last step scales by 4/256, then 40 ticks hold it.
        assert!(f.polys.iter().flatten().all(|v| (v[1] - 200).abs() <= 4), "all at y = 210 − 10");
    }

    #[test]
    fn melt_and_shatter_settle_on_the_floor() {
        for k in [5, 7] {
            let (_, frames, _) = run(k, 0, false);
            let f = frames.last().unwrap();
            let low = f.polys.iter().flatten().map(|v| v[1]).max().unwrap();
            assert!(low >= 180 && low <= 230, "kind {k}: lowest corner at {low}");
        }
    }

    #[test]
    fn burst_spins_and_unspins() {
        // 0x40 (acceleration 0.5): the spun short ends where it started.
        let shape = body(4);
        let rand = lcg();
        let shape_fn = || shape.clone();
        let ctx = Ctx { shape: &shape_fn, spin: 300, rand: &rand };
        let mut t = Transition::default();
        t.start_reversed(2, 0, 210, &ctx);
        let mut last = None;
        let mut first = None;
        while t.update(&ctx) {
            first = first.or(t.spin());
            last = t.spin();
        }
        assert_ne!(first, Some(300));
        // It sets out from n²·a/2 but adds back only n(n−1)/2·a: the exe
        // ends n·a/512 = 15 off.
        assert_eq!(last, Some(315));
    }
}
