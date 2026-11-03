//! The `Over` class: a flat picture over the 3-D view.
//!
//! The huge chrome "GALAPAGOS" that greets Mendel's birth in world 0 is
//! not part of the intro movie and not a 3-D object: it is a world object
//! of class `Over` (section 0, #59) whose state machine waits 40 ticks,
//! fires its trigger with params entry 1, waits 150 ticks (6¼ s at 24 Hz)
//! and fires entry 2. The trigger (`0x419e90` → `0x419c50`) reads the
//! entry's `shorts`:
//!
//! | short | meaning |
//! |---|---|
//! | 0 | picture: `0` = the title strip, `1..9` = a time-screen picture, `-1` = take the overlay down (`0x436230(1)`) |
//! | 1 | centre on the screen (`0x436270` adds `(screen − picture) / 2`) |
//! | 2, 3 | x, y on the screen (added to the centring) |
//! | 4, 5 | read, unused (508 and 37 on the CD: roughly the strip's size) |
//! | 6 | non-zero: flash the screen white with palette script 0 at priority 250 (`0x438830(0, 0xfa)`) |
//!
//! The picture goes through the same selector as the time screen
//! (`0x41a2d0`, size by the view) into one DirectDraw surface
//! (`0x48b65c`, built by `0x435e70`, filled by `0x41aa10`) with its
//! top-left pixel as the source colour key; `0x436270` blits it over
//! every frame (`BltFast`, `SRCCOLORKEY`) until it is taken down. One
//! overlay at a time; the renderer, not the world, owns it, so it is
//! neither saved nor part of the world snapshot.

/// What the screen shows over the 3-D view (the surface at `0x48b65c`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overlay {
    /// Logical picture: 0 = title strip, 1..9 = time-screen picture.
    pub picture: i16,
    pub centred: bool,
    pub x: i16,
    pub y: i16,
}

/// The overlay after an `Over` trigger with `shorts`, and whether the
/// trigger flashes the screen (`0x419c50`).
pub fn trigger(shorts: [i16; 10]) -> (Option<Overlay>, bool) {
    if shorts[0] == -1 {
        return (None, false);
    }
    let o = Overlay { picture: shorts[0], centred: shorts[1] != 0, x: shorts[2], y: shorts[3] };
    (Some(o), shorts[6] != 0)
}

/// The title strip for a view of `w`×`h` (`0x41a2d0` with `n = 0`, the
/// 8-bit set): picture index, width, height. Smaller than 144×100: none.
pub fn title_strip(w: u32, h: u32) -> Option<(i32, i32, i32)> {
    if w >= 560 && h >= 400 {
        Some((0, 504, 30))
    } else if w >= 288 && h >= 200 {
        Some((10, 256, 15))
    } else if w >= 144 && h >= 100 {
        Some((20, 128, 8))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section0_title_entries() {
        // Section 0's #59, params 1 and 2 as stored on the CD.
        let show = trigger([0, 1, 0, 0, 508, 37, 1, 0, 0, 0]);
        assert_eq!(show, (Some(Overlay { picture: 0, centred: true, x: 0, y: 0 }), true));
        assert_eq!(trigger([-1, 0, 0, 0, 0, 0, 0, 0, 0, 0]), (None, false));
    }

    /// World 0 from load: the title goes up on tick 40 with a white flash
    /// and comes down 150 ticks later, as in the 1997 footage (1.7 s to
    /// 8.0 s after the world appears).
    #[test]
    fn world0_title_timing_when_data_present() {
        use crate::sim::Sim;
        use crate::world::WorldMesh;
        use galapagos_formats::worlds::Archive;
        let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../originals/mounted/install/data/worlds");
        let Ok(data) = std::fs::read(p) else { return };
        let archive = Archive::parse(&data).unwrap();
        let section = archive.world(0).unwrap();
        let mut mesh = WorldMesh::build(&section);
        let mut sim = Sim::new(&section, &mut mesh);
        let mut shown = Vec::new();
        let mut flashed = None;
        for t in 1..=240u32 {
            sim.step(&mut mesh);
            shown.push(sim.overlay.is_some());
            if flashed.is_none() && sim.flash.remap.is_some() {
                flashed = Some(t);
            }
        }
        let first = shown.iter().position(|&s| s).expect("the title appears") as u32 + 1;
        let last = shown.iter().rposition(|&s| s).unwrap() as u32 + 1;
        assert_eq!(sim.overlay, None);
        // `TicksAtLeast(150)` after a timer reset: 150 or 151 frames up.
        assert!((150..=151).contains(&(last - first + 1)), "up for {} ticks", last - first + 1);
        assert!((40..=42).contains(&first), "up at tick {first}");
        println!("title up on ticks {first}..={last}");
        assert_eq!(flashed, Some(first), "the white flash starts with it");
    }

    #[test]
    fn strip_sizes_follow_the_view() {
        assert_eq!(title_strip(640, 480), Some((0, 504, 30)));
        assert_eq!(title_strip(320, 240), Some((10, 256, 15)));
        assert_eq!(title_strip(256, 192), Some((20, 128, 8)));
        assert_eq!(title_strip(140, 100), None);
    }
}
