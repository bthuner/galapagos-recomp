//! "Mendel's mind": a read-only snapshot of what he senses, feels and
//! decides, for the Quality of Life panel (#48, `web/src/qol/mind.js`).
//! Not in the original: the 1997 game never showed any of this.
//!
//! Two halves:
//!
//! * [`Trace`] is written *during* his tick, by the senses, the touch and
//!   the surface events (a handful of stores; nothing is allocated). It
//!   keeps what the tick computes and then throws away: each eye ray's
//!   hit, which ray the eye believed, whether a touch overrode sight.
//! * [`Mind`] is assembled from the creature and its trace *on demand*
//!   ([`Creature::mind`], called by `creature_mind_ptr` in lib.rs). A
//!   closed panel never asks, so it costs nothing.
//!
//! This module is a child of `creature`, so it may read the creature's
//! private fields (the actuators, the cost accumulators) without making
//! them public to the rest of the engine.
//!
//! ## The layout JavaScript reads
//!
//! `Mind` is `#[repr(C)]` and made only of `f32`: JavaScript reads it as
//! one `Float32Array` of [`Mind::LEN`] floats. Indices (the tests pin
//! them with `offset_of!`, and `web/src/qol/mind.js` mirrors this table):
//!
//! ```text
//!   0 version (2)           1 mode (0 absent, 1 appearing, 2 alive, 3 vanishing)
//!   2 ticks in that mode     3 ticks alive (ever)
//!   4 heading (1024ths)      5 speed drive 2·muscle₁−256 (−256 back … 256 forward)
//!   6 turn drive 2·muscle₂−256 (+ = left)   7 step: units walked next tick
//!   8 turn rate: 1024ths next tick (+ = left)   9 gait (24; 53 speed; 40 agitated)
//!  10 stance muscle (0..256)  11 stance height (41..207)   12 grounded (0/1)
//!  13 ground object + 1 (0 = airborne)   14 ground polygon   15 ticks falling
//!  16 save pad under him (0/1)   17 distance walked (ever)
//!  18 eye angle a (1024ths)   19 eye range R (units)
//!  20..34 left eye, 35..49 right eye; each:
//!       +0 value (0..256)  +1 source (0 sight, 1 bump, 2 right click,
//!       3 blind, 4 confused, 5 falling)  +2 ray it believed (0 high,
//!       1 middle, 2 ledge; −1 none)
//!       +3.. three rays (high, middle, ledge), 4 floats each:
//!       distance from the ray's start (−1 nothing hit), class (0 none,
//!       1 floor, 2 wall, 3 hazard, 4 drop: a floor's edge seen by the
//!       ledge ray), seen (1 if the eye counts it, 0 if it only hides
//!       what is behind), value (0..256)
//!  50 bump left  51 bump right (the hull's front corners, 0/1)
//!  52 right click: side (0 none, 1 left, 2 right)  53 its ticks left (4..1)
//!  54 touch events posted (ever)
//!  55 pains (lethal surfaces that hurt, this session)   56 deaths (ever)
//!  57 delayed lethal floor: ticks until it fires (0 = not on one)
//!  58 affliction (0 none, 1..12)   59 its ticks left   60 its duration
//!  61 GA generations, all controllers (ever)
//!  62..76 posture controller, 77..91 speed, 92..106 turn; each:
//!       +0 chain state (0..9)  +1 chain output = muscle target (0..256)
//!       +2 muscle after slew (0..256)  +3 muscle velocity (units/tick)
//!       +4 genome on trial (0..29)  +5 trial progress (0..1)
//!       +6 running cost of this trial (lower = fitter)
//!       +7 cost of the last finished trial  +8 best  +9 worst in the
//!       population's record   +10 ≈ ticks to the next trial
//!       +11 ≈ ticks to the next generation  +12 trials finished (session)
//!       +13 generations (session)  +14 cost-ticks per tick now
//! 107 right clicks felt (session)   108 the last one's side (1 left, 2 right):
//!       a click lasts 4 ticks, and a slow frame may run 5, so the panel
//!       watches this counter rather than catching the click in the act
//! ```

use super::{Creature, Mode, DELAYED, FLOOR, HAZARD, LETHAL};
use crate::affliction::DURATION;
use galapagos_formats::creature::POPULATION;

/// Bumped whenever the layout changes, so a stale page can tell.
pub const VERSION: f32 = 2.0;

/// A trial ends when its cost count *exceeds* 65,536 (`Agent_LearnIfDue`).
const TRIAL: i64 = 65_536;

/// Ray class: what an eye ray met (for the panel's colours; 0 = nothing).
pub const CLASS_FLOOR: u8 = 1;
pub const CLASS_WALL: u8 = 2;
pub const CLASS_HAZARD: u8 = 3;
pub const CLASS_DROP: u8 = 4;

/// Where an eye's value came from this tick.
pub const SIGHT: u8 = 0;
pub const BUMP: u8 = 1;
pub const CLICK: u8 = 2;
pub const BLIND: u8 = 3;
pub const CONFUSED: u8 = 4;
pub const FALLING: u8 = 5;

/// What a polygon's flags make of it, for the panel.
pub(crate) fn class_of(flags: u32) -> u8 {
    if flags & HAZARD != 0 {
        CLASS_HAZARD
    } else if flags & FLOOR != 0 {
        CLASS_FLOOR
    } else {
        CLASS_WALL
    }
}

/// One eye ray of this tick.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RayTrace {
    /// Distance from the ray's start to what it met; `None`: nothing.
    pub dist: Option<f32>,
    pub class: u8,
    /// The eye counts it (IR-visible, a hazard, a drop); otherwise it is
    /// a blocker that only hides what lies behind.
    pub seen: bool,
    pub value: i32,
}

/// What the tick computed and would otherwise forget. Not saved: a loaded
/// game starts with an empty trace and fills it on its first tick.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Trace {
    /// Per eye (left, right): three rays, the one believed, the source.
    pub rays: [[RayTrace; 3]; 2],
    pub winner: [Option<u8>; 2],
    pub source: [u8; 2],
    /// The hull's front corners hit something this tick.
    pub bump: [bool; 2],
    /// The right click felt this tick: (left side?, ticks left incl. this one).
    pub click: Option<(bool, u8)>,
    /// Right clicks felt this session, and the side of the last one.
    pub clicks: u32,
    pub last_click: u8,
    pub pains: u32,
    /// Per controller: trials finished and generations, this session.
    pub trials: [u32; 3],
    pub generations: [u32; 3],
}

/// One ray as JavaScript reads it (4 floats).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ray {
    pub dist: f32,
    pub class: f32,
    pub seen: f32,
    pub value: f32,
}

/// One eye (15 floats).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Eye {
    pub value: f32,
    pub source: f32,
    pub winner: f32,
    pub rays: [Ray; 3],
}

/// One NERM controller and its muscle (15 floats).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Controller {
    pub state: f32,
    pub output: f32,
    pub muscle: f32,
    pub velocity: f32,
    pub genome: f32,
    pub progress: f32,
    pub cost: f32,
    pub last_cost: f32,
    pub best_cost: f32,
    pub worst_cost: f32,
    pub trial_ticks: f32,
    pub generation_ticks: f32,
    pub trials: f32,
    pub generations: f32,
    pub rate: f32,
}

/// The snapshot (see the module doc for the index table).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Mind {
    pub version: f32,
    pub mode: f32,
    pub mode_ticks: f32,
    pub ticks_alive: f32,
    pub heading: f32,
    pub speed: f32,
    pub turn: f32,
    pub step: f32,
    pub turn_rate: f32,
    pub gait: f32,
    pub stance: f32,
    pub stance_height: f32,
    pub grounded: f32,
    pub ground_object: f32,
    pub ground_polygon: f32,
    pub fall_ticks: f32,
    pub save_pad: f32,
    pub walked: f32,
    pub eye_angle: f32,
    pub eye_range: f32,
    pub eyes: [Eye; 2],
    pub bump: [f32; 2],
    pub click_side: f32,
    pub click_ticks: f32,
    pub touches_posted: f32,
    pub pains: f32,
    pub deaths: f32,
    pub hazard_delay: f32,
    pub affliction: f32,
    pub affliction_left: f32,
    pub affliction_duration: f32,
    pub generations_total: f32,
    pub controllers: [Controller; 3],
    pub clicks: f32,
    pub last_click_side: f32,
}

impl Mind {
    /// Floats in the snapshot (`creature_mind_len`).
    pub const LEN: usize = core::mem::size_of::<Mind>() / 4;
}

fn flag(b: bool) -> f32 {
    b as u8 as f32
}

impl Creature {
    /// Everything the panel shows, read from this creature as it stands
    /// between two ticks. Cheap (no allocation), and only called when the
    /// panel is open.
    pub fn mind(&self) -> Mind {
        let t = &self.trace;
        let (mode, mode_ticks) = match self.mode {
            Mode::Absent => (0.0, 0),
            Mode::Appearing(n) => (1.0, n),
            Mode::Alive => (2.0, 0),
            Mode::Vanishing(n, _) => (3.0, n),
        };
        let (gobj, gpoly) = self.ground().map_or((0.0, 0.0), |(o, p)| (o as f32 + 1.0, p as f32));
        let posture = self.act[0].value;
        let eye = |e: usize| Eye {
            value: [self.left, self.right][e] as f32,
            source: t.source[e] as f32,
            winner: t.winner[e].map_or(-1.0, |w| w as f32),
            rays: t.rays[e].map(|r| Ray {
                dist: r.dist.unwrap_or(-1.0),
                class: r.class as f32,
                seen: flag(r.seen),
                value: r.value as f32,
            }),
        };
        // Cost-ticks per tick (`Creature_AccumulateCost`): the proximity
        // weight for posture and turn, a flat 256 for speed.
        let w = (self.left as i64 + self.right as i64) * 416 / 512 + 96;
        let rates = [w, 256, w];
        let controller = |k: usize| {
            let n = &self.brain.nerms[k];
            let count = self.count[k];
            let rate = rates[k].max(1);
            // The trial ends on the first tick its count exceeds 65,536.
            let ceil = |a: i64| (a + rate - 1) / rate;
            let trial_ticks = ceil((TRIAL + 1 - count).max(0));
            let per_trial = ceil(TRIAL + 1);
            let individual = n.individual.rem_euclid(POPULATION as i16) as i64;
            let last = n.fitness[(individual as usize + POPULATION - 1) % POPULATION];
            Controller {
                state: n.state as f32,
                output: self.outputs[k] as f32,
                muscle: self.act[k].value as f32,
                velocity: self.act[k].vel as f32 / 65536.0,
                genome: individual as f32,
                progress: (count as f32 / TRIAL as f32).clamp(0.0, 1.0),
                cost: if count > 0 { (self.cost[k] * TRIAL / count) as f32 } else { 0.0 },
                last_cost: last as f32,
                best_cost: *n.fitness.iter().min().unwrap_or(&0) as f32,
                worst_cost: *n.fitness.iter().max().unwrap_or(&0) as f32,
                trial_ticks: trial_ticks as f32,
                generation_ticks: (trial_ticks + (POPULATION as i64 - 1 - individual) * per_trial) as f32,
                trials: t.trials[k] as f32,
                generations: t.generations[k] as f32,
                rate: rate as f32,
            }
        };
        let kind = self.afflictions.kind;
        Mind {
            version: VERSION,
            mode,
            mode_ticks: mode_ticks as f32,
            ticks_alive: self.stats.ticks_alive as f32,
            heading: self.heading() as f32,
            speed: (2 * self.act[1].value - 256) as f32,
            turn: (2 * self.act[2].value - 256) as f32,
            step: self.forward as f32,
            turn_rate: self.turn_d as f32,
            gait: self.afflictions.gait() as f32,
            stance: posture as f32,
            stance_height: self.stance_height as f32,
            grounded: flag(self.mover.grounded),
            ground_object: gobj,
            ground_polygon: gpoly,
            fall_ticks: self.fall_count as f32,
            save_pad: flag(self.on_save_pad),
            walked: self.stats.walked,
            eye_angle: (100 - posture * 50 / 256) as f32,
            eye_range: (1500 + posture * 500 / 256) as f32,
            eyes: [eye(0), eye(1)],
            bump: t.bump.map(flag),
            click_side: t.click.map_or(0.0, |(left, _)| if left { 1.0 } else { 2.0 }),
            click_ticks: t.click.map_or(0.0, |(_, n)| n as f32),
            touches_posted: self.stats.touches_posted as f32,
            pains: t.pains as f32,
            deaths: self.stats.deaths as f32,
            hazard_delay: if self.surface & LETHAL != 0 && self.surface & DELAYED != 0 { self.delay as f32 } else { 0.0 },
            affliction: kind as f32,
            affliction_left: self.afflictions.left().max(0) as f32,
            affliction_duration: DURATION.get(kind as usize).copied().unwrap_or(0) as f32,
            generations_total: self.stats.evolutions as f32,
            controllers: [controller(0), controller(1), controller(2)],
            clicks: t.clicks as f32,
            last_click_side: t.last_click as f32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{data, world};
    use super::*;
    use crate::sim;
    use core::mem::offset_of;

    /// The table in the module doc is the contract with mind.js.
    #[test]
    fn layout_matches_the_documented_indices() {
        let at = |bytes: usize| bytes / 4;
        assert_eq!(core::mem::size_of::<Ray>(), 4 * 4);
        assert_eq!(core::mem::size_of::<Eye>(), 15 * 4);
        assert_eq!(core::mem::size_of::<Controller>(), 15 * 4);
        assert_eq!(at(offset_of!(Mind, heading)), 4);
        assert_eq!(at(offset_of!(Mind, gait)), 9);
        assert_eq!(at(offset_of!(Mind, walked)), 17);
        assert_eq!(at(offset_of!(Mind, eye_angle)), 18);
        assert_eq!(at(offset_of!(Mind, eyes)), 20);
        assert_eq!(at(offset_of!(Mind, eyes)) + 15, 35);
        assert_eq!(at(offset_of!(Mind, bump)), 50);
        assert_eq!(at(offset_of!(Mind, click_side)), 52);
        assert_eq!(at(offset_of!(Mind, touches_posted)), 54);
        assert_eq!(at(offset_of!(Mind, pains)), 55);
        assert_eq!(at(offset_of!(Mind, hazard_delay)), 57);
        assert_eq!(at(offset_of!(Mind, affliction)), 58);
        assert_eq!(at(offset_of!(Mind, generations_total)), 61);
        assert_eq!(at(offset_of!(Mind, controllers)), 62);
        assert_eq!(Mind::LEN, 109);
        assert_eq!(at(offset_of!(Mind, clicks)), 107);
        assert_eq!(at(offset_of!(Controller, rate)), 14);
    }

    /// The snapshot says what the creature's own fields say, tick after
    /// tick, through a walk, a right click and GA trials.
    #[test]
    fn mind_matches_the_creature() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = world(&d, 0);
        let mut seen_trial = false;
        let mut clicked = false;
        for _ in 0..90 * sim::TICKS_PER_SECOND {
            sim.skip_held = (120..122).contains(&sim.tick);
            let before = sim.creature.as_ref().unwrap().brain.nerms.each_ref().map(|n| n.individual);
            sim.step(&mut mesh);
            let c = sim.creature.as_mut().unwrap();
            let m = c.mind();
            assert_eq!(m.version, VERSION);
            assert_eq!(m.mode, match c.mode { Mode::Absent => 0.0, Mode::Appearing(_) => 1.0, Mode::Alive => 2.0, Mode::Vanishing(..) => 3.0 });
            assert_eq!(m.heading, c.heading() as f32);
            assert_eq!(m.stance, c.act[0].value as f32);
            assert_eq!(m.speed, (2 * c.act[1].value - 256) as f32);
            assert_eq!(m.grounded, flag(c.mover.grounded));
            assert_eq!(m.ground_object, c.ground().map_or(0.0, |g| g.0 as f32 + 1.0));
            assert_eq!([m.eyes[0].value, m.eyes[1].value], [c.left as f32, c.right as f32]);
            assert_eq!(m.affliction, c.afflictions.kind as f32);
            assert_eq!(m.deaths, c.stats.deaths as f32);
            for k in 0..3 {
                let (n, mc) = (&c.brain.nerms[k], &m.controllers[k]);
                assert_eq!(mc.state, n.state as f32);
                assert_eq!(mc.output, c.outputs[k] as f32);
                assert_eq!(mc.muscle, c.outputs()[k] as f32);
                assert_eq!(mc.genome, n.individual as f32);
                assert!(mc.trial_ticks >= 1.0 && mc.trial_ticks <= 683.0, "{}", mc.trial_ticks);
                if n.individual != before[k] {
                    seen_trial = true;
                    assert_eq!(mc.progress, 0.0, "a new trial starts from nothing");
                    assert_eq!(mc.last_cost, n.fitness[(n.individual as usize + POPULATION - 1) % POPULATION] as f32);
                }
            }
            // The speed controller's trial is exactly 257 ticks.
            if c.mode == Mode::Alive && c.count[1] == 0 {
                assert_eq!(m.controllers[1].trial_ticks, 257.0);
            }
            // An eye that saw something believed one of its rays.
            for e in 0..2 {
                if m.eyes[e].source == SIGHT as f32 && m.eyes[e].value > 0.0 {
                    let w = m.eyes[e].winner as usize;
                    assert!(m.eyes[e].winner >= 0.0);
                    assert_eq!(m.eyes[e].rays[w].value, m.eyes[e].value);
                    assert_eq!(m.eyes[e].rays[w].seen, 1.0);
                }
            }
            // A right click, once he walks: felt on that side for 4 ticks.
            if !clicked && c.mode == Mode::Alive && c.stats.ticks_alive == 200 {
                clicked = true;
                c.touch(false);
                sim.step(&mut mesh);
                let c = sim.creature.as_ref().unwrap();
                let m = c.mind();
                assert_eq!((m.click_side, m.click_ticks), (2.0, 4.0));
                assert_eq!(m.eyes[1].source, CLICK as f32);
                assert_eq!(m.eyes[1].value, 256.0);
            }
        }
        assert!(clicked, "he came alive");
        let c = sim.creature.as_ref().unwrap();
        println!("after 90 s: trials {:?}, generations {:?}, {:?}", c.trace.trials, c.trace.generations, c.mind().controllers.map(|k| (k.genome, k.cost, k.trial_ticks)));
        assert!(seen_trial, "a GA trial ended within 90 s");
    }
}
