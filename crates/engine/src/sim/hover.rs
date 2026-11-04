//! "Would a click here do anything?" (Quality of Life #41: highlight
//! clickable objects under the cursor). not in the original: the 1997 game
//! never told the player what was clickable. This module only *reads* the
//! simulation, so asking costs nothing in faithfulness.
//!
//! The answer must never lie, so it is built from the same parts as a real
//! click (`sim_click`):
//!
//! 1. **The same pick.** [`Sim::pick_hit`]: the exe's mouse mask
//!    `0xfffffffe`, the polygons' live collision flags and real outlines,
//!    awake objects only (a folder switched off is neither updated nor
//!    picked). The nearest hit wins, so a clickable lift behind a wall is
//!    not clickable from here, exactly as in the game.
//! 2. **The same routing.** The exe hands the click to the object it hit
//!    (`ObjectRegistry_SetPicked` @ 0x406b50), nobody else; Mendel's own
//!    hull `MEnc` carries only pick bit 0, which the mask drops. So the
//!    object to test is the one hit.
//! 3. **The same conditions, one tick ahead.** A click counts during the
//!    next tick. The object is clickable if, in its **current state**, some
//!    transition has a condition group that contains `Clicked` and whose
//!    other conditions all hold *now* (see [`Sim::clickable`]). Between two
//!    ticks the registry already holds what the next evaluation will read:
//!    the events Mendel posted after the last evaluation, the timers, the
//!    other objects' states. Position and rotation tests read the current
//!    pose; the next tick's animation step may move it a little first.
//! 4. **The save pads.** A click on a polygon with frame flag `0x20` while
//!    Mendel stands on a save pad opens the Save dialog
//!    (0x41baa3..0x41bb07), whatever the pad's state machine says.
//!
//! Nothing here writes: no picked object, no random number drawn (a
//! `Chance` condition is *possible*, not rolled, so it never consumes the
//! CRT `rand()` the game's own rolls depend on).

use super::{Sim, View};
use galapagos_formats::logic::{Condition, LogicView, Transition};

impl Sim {
    /// The object a left click along this ray would reach, if that click
    /// would currently matter (1-based id). No side effects.
    pub fn hover(&self, origin: [f32; 3], dir: [f32; 3]) -> Option<u16> {
        let (id, flags) = self.pick_hit(origin, dir)?;
        let save_pad = flags & 0x20 != 0 && self.creature.as_ref().is_some_and(|c| c.on_save_pad);
        (save_pad || self.clickable(id)).then_some(id)
    }

    /// Object `id` (1-based) is awake and a click on it during the next
    /// tick would fire one of its current state's transitions.
    ///
    /// The rule, per transition of the current state: some group contains
    /// `Clicked`, and every other condition of that group is true now,
    /// with these readings:
    ///
    /// * `NotClicked` is false (the group needs *no* click: a click can
    ///   only stop it).
    /// * `Chance(n)` is possible (`rand() % 100 <= n` has a chance for any
    ///   n ≥ 0), so it counts as true: the highlight then means "a click
    ///   may do something", the honest reading of a dice roll.
    /// * `Unknown` opcodes are false, as in the interpreter.
    ///
    /// A transition whose actions are all empty is ignored: it fires but
    /// changes nothing.
    pub fn clickable(&self, id: u16) -> bool {
        let i = id.wrapping_sub(1) as usize;
        let Some(o) = self.objs.get(i) else { return false };
        if !self.awake(i) {
            return false;
        }
        let Some(sm) = o.obj.as_ref().and_then(|w| w.logic.states.as_ref()) else { return false };
        let Some(state) = sm.states.get(o.state as usize) else { return false };
        let w = IfClicked(View { sim: self, i });
        state.transitions.iter().any(|t| click_matters(t, &w))
    }

    /// Every awake object a click would currently matter for (1-based ids,
    /// in registry order): the candidates keyboard-only play (#49) cycles
    /// through. It ignores line of sight and the save-pad rule; the caller
    /// keeps the ones on screen (`view.project`) and can confirm each with
    /// [`Sim::hover`] along the ray to its centre. One pass over the
    /// registry: cheap enough to call once per frame.
    pub fn clickable_ids(&self) -> Vec<u16> {
        (1..=self.objs.len() as u16).filter(|&id| self.clickable(id)).collect()
    }

    /// The renderer's draw index of object `id` (1-based), if it has a mesh.
    pub fn draw_of(&self, id: u16) -> Option<usize> {
        self.objs.get(id.wrapping_sub(1) as usize)?.draw
    }

    /// Updated by the next tick: not inside a switched-off folder.
    fn awake(&self, i: usize) -> bool {
        self.visible().get(i).copied().unwrap_or(false)
    }
}

/// `t` would fire because of a click: a group with `Clicked` whose other
/// conditions hold, and something to do.
fn click_matters(t: &Transition, w: &IfClicked) -> bool {
    if t.actions == Default::default() {
        return false;
    }
    t.groups.iter().any(|g| {
        g.contains(&Condition::Clicked)
            && g.iter().all(|c| match c {
                Condition::Chance(_) => true, // possible (rand() % 100 can be 0), not rolled
                c => c.eval(w),
            })
    })
}

/// The object's view of the world, as if it were clicked this tick.
struct IfClicked<'a>(View<'a>);

impl LogicView for IfClicked<'_> {
    fn clicked(&self) -> bool {
        true
    }
    fn ticks(&self) -> u16 {
        self.0.ticks()
    }
    fn events(&self) -> u32 {
        self.0.events()
    }
    fn previous_events(&self) -> u32 {
        self.0.previous_events()
    }
    fn random(&self) -> u32 {
        0 // never reached: `Chance` is decided in `click_matters`
    }
    fn position(&self) -> [i32; 3] {
        self.0.position()
    }
    fn rotation(&self) -> [i16; 3] {
        self.0.rotation()
    }
    fn state_of(&self, id: u16) -> u16 {
        self.0.state_of(id)
    }
}

#[cfg(test)]
mod tests {
    use crate::sim::{self, apply, transform, Sim};
    use crate::world::WorldMesh;
    use galapagos_formats::worlds::Archive;

    fn section(s: usize) -> Option<(Sim, WorldMesh)> {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let data = std::fs::read(p).ok()?;
        let section = Archive::parse(&data).unwrap().world(s).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let sim = Sim::new(&section, &mut mesh);
        Some((sim, mesh))
    }

    /// A ray straight down onto the centre of polygon `p` of object `i`.
    fn ray_onto(sim: &Sim, i: usize, p: usize) -> [f32; 3] {
        let m = sim.mesh_of(i).unwrap();
        let verts = sim.vertices_of(i).unwrap_or(&m.vertices);
        let (pos, rot) = sim.pose(i);
        let t = transform(pos, rot);
        let pts: Vec<_> = m.polygons[p].iter().map(|&k| apply(&t, verts[k as usize].map(|v| v as f32))).collect();
        let n = pts.len() as f32;
        let c = pts.iter().fold([0f32; 3], |a, v| [a[0] + v[0] / n, a[1] + v[1] / n, a[2] + v[2] / n]);
        [c[0], c[1], c[2] + 20.0]
    }

    /// Section 0's lift #159 rides on `Event(1) ∧ Clicked` (Mendel touches
    /// it and the player clicks it): hovering it is "clickable" only while
    /// he is aboard, and asking, however often, never clicks.
    #[test]
    fn hover_tells_the_truth_and_touches_nothing() {
        let Some((mut sim, mut mesh)) = section(0) else { return };
        sim.wake_all = true; // its folder sleeps until Mendel is near
        let lift = 159 - 1;
        let down = [0.0, 0.0, -1.0];
        for _ in 0..4 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
        }
        let o = (0..sim.mesh_of(lift).unwrap().polygons.len())
            .filter(|&p| sim.frame_flags(lift, p) & 0x100 != 0)
            .map(|p| ray_onto(&sim, lift, p))
            .find(|&o| sim.pick(o, down) == Some(159))
            .expect("a pickable floor on the lift");
        assert_eq!(sim.state_of(159), 0);
        assert_eq!(sim.hover(o, down), None, "nobody aboard: a click does nothing");
        assert!(!sim.clickable_ids().contains(&159));

        // What Mendel's tick posts when he stands on it (read by the next
        // evaluation): now a click would start the ride.
        sim.post_event(lift, 1);
        assert_eq!(sim.hover(o, down), Some(159), "Mendel aboard: a click rides");
        assert!(sim.clickable_ids().contains(&159));
        assert_eq!(sim.draw_of(159), mesh.draw_of_object[lift]);
        for _ in 0..10 {
            sim.hover(o, down);
        }
        sim.step(&mut mesh);
        assert_eq!(sim.state_of(159), 0, "hover must not click");

        // A real click (same event) still rides.
        sim.post_event(lift, 1);
        sim.click(159);
        sim.step(&mut mesh);
        assert_eq!(sim.state_of(159), 1);
        // Riding (state 1) waits for no click: not highlighted.
        sim.post_event(lift, 1);
        assert!(!sim.clickable(159));
    }

    /// Objects without a state machine are never clickable, and neither
    /// is anything inside a folder that is switched off.
    #[test]
    fn scenery_and_sleeping_objects_are_not_clickable() {
        let Some((mut sim, mut mesh)) = section(0) else { return };
        for _ in 0..4 * sim::TICKS_PER_SECOND {
            sim.step(&mut mesh);
        }
        let awake = sim.visible();
        let ids = sim.clickable_ids();
        assert!(!ids.is_empty(), "section 0 has something to click at the start");
        assert!(ids.iter().all(|&id| awake[id as usize - 1]), "only awake objects");
        for (i, o) in sim.objs.iter().enumerate() {
            if o.obj.as_ref().is_some_and(|w| w.logic.states.is_none()) {
                assert!(!sim.clickable(i as u16 + 1), "#{} has no state machine", i + 1);
            }
        }
        println!("section 0 clickable at 4 s: {ids:?}");
    }
}

/// The rule above is exact on the CD: across every section, the condition
/// groups that contain `Clicked` (1,340 of them, on 485 objects) pair it
/// only with `Event`, `NoEvent`, `TicksAtLeast`, `TicksAtMost` and
/// `ObjectInState`, all of which the registry answers between ticks. No
/// click is gated by a dice roll (`Chance`), a pose or an unknown opcode.
#[cfg(test)]
mod survey {
    use galapagos_formats::logic::Condition::{self, *};
    use galapagos_formats::worlds::{Archive, Object};

    #[test]
    fn click_groups_only_test_what_the_registry_knows() {
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let a = Archive::parse(&data).unwrap();
        let mut groups = 0;
        for s in 0..a.sections.len() {
            for o in &a.world(s).unwrap().objects {
                let Object::World(w) = o else { continue };
                let Some(sm) = w.logic.states.as_ref() else { continue };
                let all = sm.states.iter().flat_map(|st| &st.transitions).flat_map(|t| &t.groups);
                for g in all.filter(|g| g.contains(&Condition::Clicked)) {
                    groups += 1;
                    for c in g {
                        assert!(
                            matches!(c, Clicked | Event(_) | NoEvent(_) | TicksAtLeast(_) | TicksAtMost(_) | ObjectInState { .. }),
                            "section {s}: a click group tests {c:?}"
                        );
                    }
                }
            }
        }
        println!("{groups} click groups, all decidable between ticks");
        assert!(groups > 1000);
    }
}
