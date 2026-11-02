//! Mendel's afflictions: the "active effect" manager at `creature+0x8e8`
//! (`ActiveEffects_ctor` @ 0x405820). docs/re/mendel.md §2.4 is the spec.
//!
//! A polygon can carry an affliction number in bits 25..28 of its frame
//! flags. When Mendel touches it, `Creature_HandleSurfaceEvents` @ 0x419550
//! calls `Afflictions_Apply` @ 0x405e20 with `(flags >> 25) & 15`; the
//! number lands in `creature+0x8fc`, where the rest of his code reads it:
//!
//! | # | lasts (ticks) | what it does (who reads it) |
//! |---|---------------|-----------------------------|
//! | 1 | 1440 (60 s)   | **shield**: lethal floors flagged `0x400000` stop killing, affliction floors flagged `0x20000000` stop afflicting (0x419550); the eyes see `0x400000` polygons (0x419283); shimmering skin; flash 0x17, sound 37 |
//! | 2 | 1440          | faster muscles: actuator steps (4, 7) (`Creature_Think` @ 0x418970) |
//! | 3 | 1440          | **speed**: gait 53 (0x4189c0), actuator steps (1, 6); strobe 0x18, sound 39 |
//! | 4 | 4320 (180 s)  | **camouflage**: his skin takes the texture of the floor he stands on (0x407350); `PBal` predators stop chasing him (0x415220); flash 0x19, sound 40 |
//! | 5 | 240 (10 s)    | **blind**: both eyes read 0 (0x418fe8); his four front polygons go dark; white-out 0x1a, sound 41 at half pitch |
//! | 6 | 720 (30 s)    | **confused**: each eye reads random 0/256 noise (0x418ff9); negative screen 0x1b for 30 s, sound 42 |
//! | 7, 8, 9 | 96 (4 s) | **agitated**: gait 40, actuator steps (4, 7), the "agitated" cue (sounds 1, 2) every tick it may play; for their first two seconds, random coloured flickers and crackles |
//! | 10, 11, 12 | 100   | gravity −2, 0, +7 (`CreatureMover_Integrate` @ 0x418060); flash 0x28/0x29/0x2a, sound 53/54/55 |
//!
//! On the CD only 1, 3, 7, 8 and 9 are used by polygons (5, 6, 2, 4,
//! 10..12 are reachable code with no surface that starts them). 7/8/9 sit
//! on "delayed" lethal floors: 80 ticks of contact kill him, and meanwhile
//! he is agitated.
//!
//! Each effect object (base `ActiveEffect_ctor` @ 0x406840) runs a lead-in
//! delay, `duration` ticks of its visual (vtable slot 4), a lead-out delay,
//! then ends (slot 2); the manager's tick (0x405fc0) restores the colour
//! ramps every tick and, once the effect is over, his polygons and
//! affliction 0. Both delays are 0 for every affliction (`0x406350` sets
//! `+0x1e = +0x20 = 0`), so an affliction lasts exactly its duration.

use crate::sim::{self, HostEvent};

/// Duration in ticks (`Afflictions_Select` @ 0x406350 hands it to the
/// effect's start, vtable slot 1).
pub const DURATION: [i16; 13] = [0, 1440, 1440, 1440, 4320, 240, 720, 96, 96, 96, 100, 100, 100];

/// Normal colour-ramp brightness (`0x406110`: below 0x8000 the ramp is
/// scaled by `2v/65535`, above it whitened).
pub const NORMAL: u16 = 0x7fff;

/// The sound and palette script `Afflictions_Select` plays when an
/// affliction starts: (1-based world-bank sound, pitch 16.16, flash id).
fn start_cue(kind: u8) -> Option<(i16, u32, u8)> {
    Some(match kind {
        1 => (0x25, 0x10000, 0x17),
        3 => (0x27, 0x10000, 0x18),
        4 => (0x28, 0x10000, 0x19),
        5 => (0x29, 0x8000, 0x1a), // `+0x70 = 0x8000`: half pitch
        6 => (0x2a, 0x10000, 0x1b),
        10 => (0x34, 0x10000, 0x28),
        11 => (0x35, 0x10000, 0x29),
        12 => (0x36, 0x10000, 0x2a),
        _ => return None,
    })
}

/// How Mendel looks this tick, per polygon of his posed mesh.
#[derive(Clone, Debug)]
pub struct Look {
    /// Ramp brightness, [`NORMAL`] = as drawn.
    pub bright: Vec<u16>,
    /// Texture layer override (afflictions 1 and 4).
    pub texture: Option<i16>,
    /// Added to every polygon's texel offset `u` (`+0x5c`): affliction 1
    /// scrolls his skin 8 a tick (`0x42b200`), i.e. 1/16 of a texture,
    /// a full turn of the texture every 16 ticks. 0 = his own offsets.
    pub scroll: i16,
}

#[derive(Clone, Debug)]
pub struct Afflictions {
    /// `creature+0x8fc`: the current affliction, 0 = none.
    pub kind: u8,
    /// The effect object's `+0x4` (running) and `+0x10` (ticks left).
    active: bool,
    left: i16,
    /// `+0x20`: affliction 3's flicker timer, 7/8/9's next flicker.
    timer: i32,
    /// `+0x24`: 7/8/9 play their first flicker without dice.
    first: bool,
    /// Affliction 1's four oscillators (`+0x20..+0x2c`) and their
    /// directions (`+0x30..+0x3c`, 1 = rising).
    osc: [i32; 4],
    rising: [bool; 4],
    pub look: Look,
    /// Affliction 5's "eyes": the four polygons sorted first by their
    /// mean model x (`0x414e10`; inferred: the front of the shell).
    eyes: [usize; 4],
}

/// What a running affliction needs from the world this tick.
pub struct Ctx<'a> {
    pub sim: &'a mut sim::Sim,
    /// Texture of the floor polygon under him (affliction 4).
    pub floor_texture: Option<i16>,
    /// The section's last texture slot (affliction 1: `FUN_00410ef0() - 1`).
    pub last_texture: i16,
}

impl Afflictions {
    pub fn new(polygons: usize, eyes: [usize; 4]) -> Self {
        Afflictions {
            kind: 0,
            active: false,
            left: 0,
            timer: 0,
            first: false,
            osc: [0; 4],
            rising: [false; 4],
            look: Look { bright: vec![NORMAL; polygons], texture: None, scroll: 0 },
            eyes,
        }
    }

    /// `Afflictions_End` @ 0x405f10: restore his polygons, affliction 0.
    /// The exe copies back the whole 0x68-byte record of each of his
    /// polygons from the copy the manager took when the section was read
    /// (`0x4059c0`): texture `+0x40`, texel offset `+0x5c/+0x5e`, all.
    pub fn end(&mut self) {
        self.active = false;
        self.left = 0;
        self.kind = 0;
        self.look.bright.fill(NORMAL);
        self.look.texture = None;
        self.look.scroll = 0;
    }

    /// `Afflictions_Apply` @ 0x405e20(n, 0): switch to affliction `n`
    /// (ending a different running one first), then (re)start it.
    pub fn apply(&mut self, n: u8, sim: &mut sim::Sim) {
        if n == 0 || n > 12 {
            return; // "Unknown active effect type" (0x406350); none on the CD
        }
        if n != self.kind {
            if self.active {
                self.end();
            }
            self.kind = 0;
        }
        self.kind = n;
        let was_running = self.active;
        // vtable slot 1 of each effect class. 1, 3, 4, 5 re-arm their
        // duration on every call (0x42b1a0, 0x42cbe0, 0x4068b0, 0x414f70);
        // 2, 6, 10..12 (0x4241b0) and 7..9 (0x424390) only start if idle.
        match n {
            1 | 3 | 4 | 5 => {
                self.left = DURATION[n as usize];
                self.active = true;
                if n == 1 {
                    self.osc = [0xffff, 5000, 1000, 0xffff];
                    self.rising = [false, true, false, false];
                }
                if n == 3 {
                    self.timer = 1;
                }
            }
            _ if !self.active => {
                self.left = DURATION[n.min(12) as usize];
                self.active = true;
                if (7..=9).contains(&n) {
                    self.timer = 0;
                    self.first = true;
                }
            }
            _ => {}
        }
        // inferred: the original replays the start sound and flash on
        // every call, i.e. every tick he touches the polygon; we play them
        // when the affliction (re)starts, which is what one hears and sees
        // once the flash has run its course.
        if !was_running {
            if let Some((sound, pitch, flash)) = start_cue(n) {
                sim.flash.play(flash, 100);
                push_sound(sim, sound, pitch, 0, 256);
            }
        }
    }

    /// The manager's tick (0x405fc0), between the cost and the surface
    /// events of `Creature_Update`.
    pub fn tick(&mut self, ctx: Ctx) {
        self.look.bright.fill(NORMAL);
        if !self.active {
            return;
        }
        let before = self.left;
        self.left -= 1;
        if before <= 0 {
            self.end();
            return;
        }
        let sim = ctx.sim;
        let left = self.left;
        match self.kind {
            1 => {
                self.shimmer(sim);
                self.look.texture = Some(ctx.last_texture);
            }
            3 => {
                // 0x42cc00: every 20..23 ticks the ramps are restored; the
                // five ticks before, each polygon re-reads its brightness and
                // rolls a die whose value is dropped (no visible change).
                self.timer -= 1;
                if self.timer < 0 {
                    self.timer = (sim.rand() & 3) as i32 + 20;
                } else if self.timer < 5 {
                    for _ in 0..self.look.bright.len() {
                        sim.rand();
                    }
                }
            }
            4 => self.look.texture = ctx.floor_texture.or(self.look.texture),
            5 => {
                // 0x414f80: the eyes stay dark (6000) and light up again over
                // the last second.
                let b = if left < 24 { (0x7fff - (left as i32 * 24000) / 24) as u16 } else { 6000 };
                for &e in &self.eyes {
                    if let Some(x) = self.look.bright.get_mut(e) {
                        *x = b;
                    }
                }
            }
            7..=9 => self.flicker(sim, left),
            _ => {}
        }
    }

    /// Affliction 1's visual (0x42b200): four triangle-wave oscillators;
    /// the last one, bouncing between the first two, sets the mean skin
    /// brightness, and each polygon adds its own noise. Every polygon also
    /// gets the section's last texture (`+0x40 = 0x410ef0() − 1`) and
    /// `+0x5c += 8`: the Direct3D draw adds `(u & 0x1ff) / 128` to his
    /// texture coordinates (0x40e850), so the skin slides along `u` by
    /// 1/16 of the texture a tick. Nothing touches `v` (`+0x5e`). The
    /// offsets keep growing while he stands on the polygon (re-arming
    /// does not reset them) and come back with the record when it ends.
    fn shimmer(&mut self, sim: &mut sim::Sim) {
        self.look.scroll = self.look.scroll.wrapping_add(8);
        // (step, low turn, high turn) of oscillators 1, 0, 2 in the
        // order the exe updates them: +0x24, +0x20, +0x28.
        for (k, step, lo, hi) in [(1, 200, 7000, 12000), (0, 300, 15000, 25000), (2, 100, 700, 7000)] {
            if self.rising[k] {
                self.osc[k] += step;
                if self.osc[k] > hi {
                    self.rising[k] = false;
                }
            } else {
                self.osc[k] -= step;
                if self.osc[k] < lo {
                    self.rising[k] = true;
                }
            }
            if self.osc[k] < 0 {
                self.osc[k] = 0;
                self.rising[k] = true;
            }
            if self.osc[k] > 65000 {
                self.osc[k] = 65000;
                self.rising[k] = false;
            }
        }
        let [o0, o1, o2, _] = self.osc;
        if self.rising[3] {
            self.osc[3] += o2;
            if self.osc[3] > o0 + o1 {
                self.rising[3] = false;
            }
        } else {
            self.osc[3] -= o2;
            if self.osc[3] < o1 {
                self.rising[3] = true;
            }
        }
        if self.osc[3] < 0 {
            self.osc[3] = 0;
            self.rising[3] = true;
        }
        if self.osc[3] > 65000 {
            self.osc[3] = 65000;
            self.rising[3] = false;
        }
        for b in &mut self.look.bright {
            let r1 = sim.rand() as i32;
            let r2 = sim.rand() as i32;
            *b = ((r2 >> 1) - (r1 >> 2) + self.osc[3]).clamp(0, 60000) as u16;
        }
    }

    /// Afflictions 7, 8, 9 (0x4243c0, 0x407470, 0x406d40): while more than
    /// 48 ticks are left, every 0..31 ticks a coloured screen flicker
    /// (priority 0: it never interrupts a real flash) and a crackle.
    fn flicker(&mut self, sim: &mut sim::Sim, left: i16) {
        if left < 0x31 {
            return;
        }
        let t = self.timer;
        self.timer -= 1;
        if t > 0 {
            return;
        }
        self.timer = (sim.rand() & 0x1f) as i32;
        let k = self.kind;
        let flashes: [u8; 3] = match k {
            7 => [0x1d, 0x1e, 0x1f],
            8 => [0x21, 0x22, 0x23],
            _ => [0x25, 0x26, 0x27],
        };
        // Variation of the crackle's pitch (`+0x74`), 16.16.
        let variation = if k == 8 { 0x4000 } else { 0x8000 };
        if self.first {
            self.first = false;
            sim.flash.play(flashes[0], 0);
            let (sound, volume) = match k {
                7 => (0x2b, 0xfa),
                8 => (0x2e, 0xff),
                _ => (0x2b, (sim.rand() & 0x7f) as i32 + 0x80),
            };
            push_sound(sim, sound, 0x10000, variation, volume);
            return;
        }
        let r = sim.rand() & 7;
        if r < 3 {
            sim.flash.play(flashes[r as usize], 0);
        }
        let pick = sim.rand() & 3;
        // The exe rolls the channel priority before the volume; we drop the
        // priority (WebAudio has no channel limit) but keep the roll.
        let (sound, volume) = match (k, pick) {
            (7, _) => {
                sim.rand();
                (0x2b + pick.min(2) as i16, (sim.rand() & 0x7f) as i32 + 0x80)
            }
            (8, 3) => {
                sim.rand();
                (0x30, (sim.rand() & 0x7f) as i32 + 0x40)
            }
            (8, _) => {
                sim.rand();
                (0x2e + pick as i16, (sim.rand() & 0x7f) as i32 + 100)
            }
            _ => (0x2b + pick.min(2) as i16, (sim.rand() & 0x7f) as i32 + 0x80),
        };
        push_sound(sim, sound, 0x10000, variation, volume);
    }

    // ——— what the rest of Mendel reads ———

    /// `body+0x69c`, set by `Creature_ApplyMotorOutputs` @ 0x4189c0.
    pub fn gait(&self) -> i32 {
        match self.kind {
            3 => 53,
            7..=9 => 40,
            _ => 24,
        }
    }

    /// Actuator sub-steps per tick `(n38, n3c)` (`Creature_Think` @ 0x418970).
    pub fn actuator_steps(&self) -> (u32, u32) {
        match self.kind {
            2 | 7..=9 => (4, 7),
            3 => (1, 6),
            _ => (1, 2),
        }
    }

    /// The mover's gravity, added to the vertical speed every tick
    /// (`CreatureMover_Integrate` @ 0x418060): −7, or +7 / −2 / 0 under
    /// afflictions 12 / 10 / 11.
    pub fn gravity(&self) -> i32 {
        match self.kind {
            12 => 7,
            10 => -2,
            11 => 0,
            _ => -7,
        }
    }

    /// Ticks left of the running affliction (for tests and the host).
    pub fn left(&self) -> i16 {
        if self.active { self.left } else { 0 }
    }
}

/// Event [`sim::EV_AFFLICTION_SOUND`]: a world-bank sound heard at Mendel
/// (the manager's cue template `+0x9c`: near 2000, far 20000).
fn push_sound(sim: &mut sim::Sim, sound: i16, pitch: u32, variation: u32, volume: i32) {
    let c = volume.clamp(0, 0xffff) | (((variation >> 8) as i32) << 16);
    sim.events.push(HostEvent { kind: sim::EV_AFFLICTION_SOUND, a: sound as i32 - 1, b: pitch as i32, c });
}
