//! The player's statistics (Pause ▸ Stats, issue #50). Not in the 1997 game.
//!
//! [`Tally`] counts what happened to Mendel while someone played: how he
//! died and of what, how often he left the ground, which afflictions he
//! caught, how many GA generations his brain evolved. It is **write-only
//! for the simulation**: nothing in the engine ever reads it back, so
//! keeping statistics cannot change how the game plays.
//!
//! It is also **outside the game's state**. `Creature::stats` belongs to
//! the creature and travels with it: a respawn restores it from the world
//! snapshot, a saved game carries it. The tally does neither. It is not in
//! the snapshot nor in `GLPS`, it is carried from section to section like
//! the brain (`build_section`) and survives a load (which rebuilds the
//! section first, then restores the save over it). So every counter only
//! ever grows, by events that really happened on screen, and the browser
//! can turn two readings into "what happened in between" by subtraction
//! (`web/src/qol/stats.js`). Only File ▸ New starts it from zero.
//!
//! The browser reads it through `stats_snapshot` (lib.rs), a fixed array
//! of [`SNAPSHOT_LEN`] u32 described at [`Tally::snapshot`].

use crate::predator::Class;
use galapagos_formats::creature::{Brain, POPULATION};

/// Number of u32 in the snapshot (see [`Tally::snapshot`]).
pub const SNAPSHOT_LEN: usize = 52;
/// Layout version, slot 0: bump it when a slot moves.
pub const SNAPSHOT_VERSION: u32 = 1;

/// The five predator classes, in the order of `sim_predator_info`.
pub const CLASSES: usize = 5;
pub fn class_index(c: Class) -> usize {
    match c {
        Class::Pred => 0,
        Class::Ball => 1,
        Class::Fork => 2,
        Class::Man => 3,
        Class::Pole => 4,
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tally {
    /// Every death, whatever the cause.
    pub deaths: u32,
    /// Fell for longer than the fall limit (the injected `FELL_TOO_LONG`).
    pub fall_deaths: u32,
    /// Killed by a predator's bite, by [`class_index`].
    pub predator_deaths: [u32; CLASSES],
    /// Killed by a lethal surface, by its kind `(flags >> 12) & 15` (the
    /// transition effect it plays). The debug `creature_react` lands here.
    pub floor_deaths: [u32; 16],
    /// Afflictions caught (a start, not every tick of contact), by number 1..12.
    pub afflictions: [u32; 13],
    /// Times he left the ground (a hop, a bounce, a walk off an edge).
    pub leaps: u32,
    /// Landings after more than 5 airborne ticks (his "landing" cue).
    pub landings: u32,
    /// GA generations completed, summed over the three NERMs.
    pub generations: u32,
    /// Bumps: touch events posted to walls and floors.
    pub touches: u32,
    pub ticks_alive: u32,
    /// Distance walked on the ground, world units.
    pub walked: f64,
    /// A predator bit this tick (set by predator.rs, consumed by the
    /// creature's next touch).
    pub(crate) bite: Option<usize>,
    /// The bite felt this tick carried lethal bits: its class.
    pub(crate) felt_bite: Option<usize>,
}

impl Tally {
    /// A predator of class `c` bit him (with `Creature::react`).
    pub(crate) fn bitten_by(&mut self, c: Class) {
        self.bite = Some(class_index(c));
    }

    /// The creature takes this tick's injected flags; remember whether a
    /// lethal bite is among them.
    pub(crate) fn feel(&mut self, injected_lethal: bool) {
        self.felt_bite = self.bite.take().filter(|_| injected_lethal);
    }

    /// He dies of surface `flags`; `fell` if the fall limit injected them.
    pub(crate) fn died(&mut self, flags: u32, fell: bool) {
        self.deaths += 1;
        if fell {
            self.fall_deaths += 1;
        } else if let Some(c) = self.felt_bite {
            self.predator_deaths[c] += 1;
        } else {
            self.floor_deaths[((flags >> 12) & 15) as usize] += 1;
        }
    }

    /// After `Afflictions::apply(n)`: count `n` if it has just started
    /// (`before` = the kind and ticks left before the call).
    pub(crate) fn afflicted(&mut self, n: u8, before: (u8, i16), now: (u8, i16)) {
        let started = now.0 == n && now.1 > 0 && (before.0 != n || before.1 <= 0);
        if started && (n as usize) < self.afflictions.len() {
            self.afflictions[n as usize] += 1;
        }
    }

    /// The flat snapshot, `SNAPSHOT_LEN` u32 (signed slots marked i32):
    ///
    /// ```text
    ///  0      layout version (1)
    ///  1      deaths, all causes
    ///  2      deaths by falling
    ///  3..8   deaths by predator class: Pred, PBal, PFrk, PMan, PPol
    ///  8..24  deaths by lethal surface kind 0..15 ((flags >> 12) & 15)
    /// 24..37  afflictions caught, by number (24 = none, unused; 25 = 1 … 36 = 12)
    /// 37      leaps (left the ground)      38  landings after a fall
    /// 39      GA generations evolved       40  bumps (touch events posted)
    /// 41      ticks alive                  42  distance walked (whole units)
    /// 43..46  i32: best (lowest) cost of each NERM's population, posture,
    ///         speed, turn (0 = none evaluated yet)
    /// 46..49  individual being tried in each NERM (0..29: the generation's progress)
    /// 49      Mendel present (1) or not (0)
    /// 50..52  reserved (0)
    /// ```
    ///
    /// Slots 1..43 only grow (until File ▸ New); 43..50 are the brain now.
    pub fn snapshot(&self, brain: Option<&Brain>) -> [u32; SNAPSHOT_LEN] {
        let mut s = [0u32; SNAPSHOT_LEN];
        s[0] = SNAPSHOT_VERSION;
        s[1] = self.deaths;
        s[2] = self.fall_deaths;
        s[3..8].copy_from_slice(&self.predator_deaths);
        s[8..24].copy_from_slice(&self.floor_deaths);
        s[24..37].copy_from_slice(&self.afflictions);
        s[37] = self.leaps;
        s[38] = self.landings;
        s[39] = self.generations;
        s[40] = self.touches;
        s[41] = self.ticks_alive;
        s[42] = self.walked as u32;
        if let Some(b) = brain {
            for (k, n) in b.nerms.iter().enumerate() {
                // Only the individuals already tried this generation have a
                // cost of their own; the rest hold the previous one's.
                let tried = (n.individual as usize).min(POPULATION);
                let pool = if tried > 0 { &n.fitness[..tried] } else { &n.fitness[..] };
                s[43 + k] = pool.iter().copied().filter(|&c| c > 0).min().unwrap_or(0) as u32;
                s[46 + k] = n.individual as u32;
            }
            s[49] = 1;
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::creature::{Creature, Mode};
    use crate::sim::{self, Sim};
    use crate::world::WorldMesh;
    use galapagos_formats::creature::{overlay_for_age, CreatureBody};
    use galapagos_formats::worlds::Archive;

    #[test]
    fn causes_land_in_their_slots() {
        let mut t = Tally::default();
        t.died(0x0158_2380, false); // kind 2 floor
        t.died(0x18_1000, true); // fall
        t.bitten_by(Class::Ball);
        t.feel(true);
        t.died(0x405b_2100, false);
        t.bitten_by(Class::Man);
        t.feel(false); // a bite without lethal bits kills no one
        t.died(0x0000_5000, false);
        let s = t.snapshot(None);
        assert_eq!(s[0], SNAPSHOT_VERSION);
        assert_eq!(s[1], 4);
        assert_eq!(s[2], 1);
        assert_eq!(s[3 + 1], 1, "PBal");
        assert_eq!(s[3 + 3], 0, "PMan");
        assert_eq!((s[8 + 2], s[8 + 5]), (1, 1), "floor kinds 2 and 5");
        assert_eq!(s[49], 0, "no brain");
        t.afflicted(3, (0, 0), (3, 1440));
        t.afflicted(3, (3, 1439), (3, 1440)); // re-armed: same affliction
        t.afflicted(6, (3, 10), (6, 200));
        assert_eq!((t.afflictions[3], t.afflictions[6]), (1, 1));
    }

    fn data() -> Option<Vec<u8>> {
        std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds")).ok()
    }

    /// Section 0 after the Space skip, Mendel alive on the arrival pad.
    fn section0(d: &[u8]) -> (Sim, WorldMesh) {
        let a = Archive::parse(d).unwrap();
        let section = a.world(0).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mut mesh);
        let body = CreatureBody::parse(d, a.header.creature as usize).unwrap();
        let brain = Brain::parse(d, a.header.creature_overlays[overlay_for_age(128).unwrap()] as usize).unwrap();
        sim.creature = Some(Creature::new(&section, &mesh, body, brain));
        sim.take_snapshot();
        for _ in 0..10 * sim::TICKS_PER_SECOND {
            sim.skip_held = (120..122).contains(&sim.tick);
            sim.step(&mut mesh);
        }
        sim.skip_held = false;
        assert_eq!(sim.creature.as_ref().unwrap().mode, Mode::Alive);
        (sim, mesh)
    }

    fn wait_alive(sim: &mut Sim, mesh: &mut WorldMesh) {
        for _ in 0..400 {
            sim.step(mesh);
            if sim.creature.as_ref().unwrap().mode == Mode::Alive {
                return;
            }
        }
        panic!("he never came back");
    }

    /// The tally sees a lethal surface and a fall as what they are, and the
    /// respawn (which restores `Creature::stats` from the snapshot) does
    /// not rewind it; neither does a saved game opened over it.
    #[test]
    fn deaths_by_cause_survive_respawn_and_load_when_data_present() {
        let Some(d) = data() else { return };
        let (mut sim, mut mesh) = section0(&d);
        let walked0 = sim.creature.as_ref().unwrap().tally.walked;
        assert!(walked0 > 0.0 || sim.creature.as_ref().unwrap().tally.ticks_alive > 0);

        // 1. The debug bite: the commonest lethal floor, kind 2.
        sim.creature.as_mut().unwrap().react(0x0158_2380);
        sim.step(&mut mesh);
        wait_alive(&mut sim, &mut mesh);

        // 2. A fall: lifted far above the world, he falls past the limit.
        let c = sim.creature.as_mut().unwrap();
        let up = c.pos;
        c.drop_at([up[0], up[1] + 400_000.0, up[2]], 0);
        c.mode = Mode::Alive;
        for _ in 0..200 {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().tally.deaths == 2 {
                break;
            }
        }
        wait_alive(&mut sim, &mut mesh);
        let c = sim.creature.as_ref().unwrap();
        let s = c.tally.snapshot(Some(&c.brain));
        println!("snapshot {s:?}");
        assert_eq!(s[1], 2, "two deaths");
        assert_eq!(s[8 + 2], 1, "one on a kind-2 lethal surface");
        assert_eq!(s[2], 1, "one fall");
        assert_eq!(s[49], 1);
        assert!(s[46..49].iter().all(|&i| i < POPULATION as u32));

        // 3. A saved game from before both deaths, opened now: the
        // creature's stats go back, the tally does not.
        let (mut early, _) = section0(&d);
        early.take_snapshot();
        let bytes = early.save_game();
        let tally = sim.creature.as_ref().unwrap().tally.clone();
        sim.load_game(&bytes, &mut mesh).unwrap();
        let c = sim.creature.as_ref().unwrap();
        assert_eq!(c.stats.deaths, 0, "the save's own counters");
        assert_eq!(c.tally.deaths, tally.deaths, "the tally is not part of a save");
    }

    /// Section 2's ball `PBal` #54 launched next to him (as in
    /// creature.rs's `ball_appears_chases_and_bites_mendel`): the death is
    /// the ball's, not the lethal kind its bite carries.
    #[test]
    fn a_bite_counts_as_the_predator_when_data_present() {
        let Some(d) = data() else { return };
        let a = Archive::parse(&d).unwrap();
        let section = a.world(2).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mut mesh);
        sim.section = 2;
        let body = CreatureBody::parse(&d, a.header.creature as usize).unwrap();
        let brain = Brain::parse(&d, a.header.creature_overlays[overlay_for_age(128).unwrap()] as usize).unwrap();
        sim.creature = Some(Creature::new(&section, &mesh, body, brain));
        sim.take_snapshot();
        sim.wake_all = true;
        wait_alive(&mut sim, &mut mesh);
        let pole = sim.position_of(55).map(|v| v as f32);
        let c = sim.creature.as_mut().unwrap();
        c.drop_at([pole[0], pole[1], pole[2] - 1500.0], 0);
        c.mode = Mode::Alive;
        sim.debug_goto(54 - 1, 3);
        for _ in 0..60 {
            sim.step(&mut mesh);
            if sim.creature.as_ref().unwrap().tally.deaths > 0 {
                break;
            }
        }
        let t = &sim.creature.as_ref().unwrap().tally;
        assert_eq!(t.deaths, 1, "bitten");
        assert_eq!(t.predator_deaths[class_index(Class::Ball)], 1, "by the ball");
        assert_eq!(t.floor_deaths.iter().sum::<u32>(), 0);
    }
}
