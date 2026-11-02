//! Screen flashes: the palette scripts of `Palette_InitScripts` @ 0x438290.
//!
//! The 1997 renderer drew into an 8-bit palettised screen, so a "flash"
//! was not a white quad over the picture but a **palette remap**: every
//! colour channel `c` (0..255) of every palette entry becomes
//! `lo + c·(hi − lo)/256`. `lo = 0, hi = 255` leaves the picture as it is,
//! `255:255` paints the channel full on, `255:0` inverts it. A script is a
//! list of such remaps, one per frame, written in a tiny text language
//! stored in the exe:
//!
//! ```text
//! C-n  r_lo:r_hi/g_lo:g_hi/b_lo:b_hi#              n frames of one remap
//! L-n  r_lo:r_hi/…/b_lo:b_hi=r_lo:r_hi/…/b_lo:b_hi#  n frames lerping A → B
//! D-n#                                              hold the current remap n frames
//! ```
//!
//! Parsed by `Palette_ParseScript` @ 0x4378e0 (the lerp is `0x438150`:
//! frame `i` of `n` = `a + i·(b − a)/(n − 1)`, both ends included), played
//! by `Palette_Play` @ 0x438830 with a priority: a script only replaces
//! the running one if its priority is at least as high (`0x437640`), and
//! the priority drops to 0 when a script ends. `Palette_Step` @ 0x437690
//! advances one frame; the game loop calls it once per tick (0x4222ad).
//!
//! The afflictions (affliction.rs) use them: affliction 6 turns the screen
//! negative for 30 s, 7/8/9 flicker warm or cold tints.

/// The scripts the afflictions use, by their slot in the exe's table
/// (`0x48d2d8 + 8·id`), verbatim from the strings at 0x469998..0x46a050.
/// The typo in 0x2a ("0:60:200") is the original's: the number parser
/// stops at the second colon, so its red channel ends at 0:60.
const SCRIPTS: &[(u8, &str)] = &[
    (0x17, "L-5 0:0/0:0/255:0=0:255/0:255/0:255#"),
    (0x18, "C-1 255:255/255:255/255:255#C-1 0:0/0:0/0:0#C-1 255:255/255:255/255:255#C-1 0:255/0:255/0:255#"),
    (0x19, "L-2 0:255/0:255/0:255=200:255/200:255/200:255#C-1 255:255/255:255/255:255#L-2 200:255/200:255/200:255=0:255/0:255/0:255#"),
    (0x1a, "C-5 255:255/255:255/255:255#L-50 255:255/255:255/255:255=0:255/0:255/0:255#"),
    (0x1b, "C-1 255:0/255:0/255:0#D-710#L-7 255:0/255:0/255:0=0:255/0:255/0:255#"),
    (0x1d, "L-5 0:255/0:255/0:255=40:255/20:255/0:255#L-5 40:255/20:255/0:255=0:255/0:255/0:255#"),
    (0x1e, "L-3 0:255/0:255/0:255=50:255/35:255/0:255#L-6 50:255/35:255/0:255=0:255/0:255/0:255#"),
    (0x1f, "L-6 0:255/0:255/0:255=20:255/20:255/0:220#L-4 20:255/20:255/0:220=0:255/0:255/0:255#"),
    (0x21, "L-5 0:255/0:255/0:255=0:255/20:255/40:255#L-6 0:255/20:255/40:255=0:255/0:255/0:255#"),
    (0x22, "L-3 0:255/0:255/0:255=0:255/35:255/50:255#L-7 0:255/35:255/50:255=0:255/0:255/0:255#"),
    (0x23, "L-6 0:255/0:255/0:255=0:220/20:255/20:255#L-5 0:220/20:255/20:255=0:255/0:255/0:255#"),
    (0x25, "L-5 0:255/0:255/0:255=0:255/40:255/10:255#L-6 0:255/40:255/10:255=0:255/0:255/0:255#"),
    (0x26, "L-3 0:255/0:255/0:255=0:255/50:255/20:255#L-7 0:255/50:255/20:255=0:255/0:255/0:255#"),
    (0x27, "L-6 0:255/0:255/0:255=0:210/20:255/0:220#L-5 0:210/20:255/0:220=0:255/0:255/0:255#"),
    (0x28, "L-6 0:255/0:255/0:255=60:200/0:255/0:255#L-5 60:200/0:255/0:255=0:255/0:255/0:255#"),
    (0x29, "L-6 0:255/0:255/0:255=60:200/60:200/0:255#L-5 60:200/60:200/0:255=0:255/0:255/0:255#"),
    (0x2a, "L-6 0:255/0:255/0:255=60:200/60:200/60:200#L-5 0:60:200/60:200/60:200=0:255/0:255/0:255#"),
];

/// One frame: `[r_lo, r_hi, g_lo, g_hi, b_lo, b_hi]`.
pub type Remap = [i32; 6];
pub const IDENTITY: Remap = [0, 255, 0, 255, 0, 255];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Show(Remap),
    /// `D-n`: keep what is on screen for n frames.
    Hold(u16),
}

/// `atoi`: leading digits, the rest ignored (hence the 0x2a typo).
fn atoi(s: &str) -> i32 {
    s.trim().chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0)
}

/// "lo:hi/lo:hi/lo:hi" → a remap. Each number is read up to its own
/// separator, like the exe's hand-written scanner.
fn remap(s: &str) -> Remap {
    let mut out = [0; 6];
    for (k, ch) in s.splitn(3, '/').enumerate().take(3) {
        let (lo, hi) = ch.split_once(':').unwrap_or((ch, "0"));
        out[2 * k] = atoi(lo);
        out[2 * k + 1] = atoi(hi);
    }
    out
}

fn parse(script: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    for cmd in script.split('#').filter(|c| !c.is_empty()) {
        let (head, body) = cmd.split_once(' ').unwrap_or((cmd, ""));
        let n = atoi(head.get(2..).unwrap_or("0")).max(0) as usize;
        match head.as_bytes().first() {
            Some(b'C') => steps.extend(std::iter::repeat_n(Step::Show(remap(body)), n)),
            Some(b'D') => steps.push(Step::Hold(n as u16)),
            Some(b'L') => {
                let (a, b) = body.split_once('=').unwrap_or((body, body));
                let (a, b) = (remap(a), remap(b));
                let d = n as i32 - 1;
                for i in 0..n as i32 {
                    let mut f = [0; 6];
                    for c in 0..6 {
                        // 8.8 fixed point, as 0x438150: start << 8, add a
                        // truncated step each frame.
                        let step = if d > 0 { ((b[c] - a[c]) << 8) / d } else { 0 };
                        f[c] = ((a[c] << 8) + i * step) >> 8;
                    }
                    steps.push(Step::Show(f));
                }
            }
            _ => break, // "Unknown sequence command (Gamma effects)"
        }
    }
    steps
}

/// The palette player (`0x48cfa0` script, `0x48d2c0` frame, `0x48cc98`
/// hold counter, `0x48cfa8` priority).
#[derive(Clone, Debug, Default)]
pub struct Flash {
    steps: Vec<Step>,
    at: usize,
    hold: u16,
    priority: u8,
    /// What the screen shows this tick.
    pub remap: Option<Remap>,
}

impl Flash {
    /// `Palette_Play` @ 0x438830(id, priority).
    pub fn play(&mut self, id: u8, priority: u8) {
        let Some(&(_, s)) = SCRIPTS.iter().find(|(k, _)| *k == id) else { return };
        if priority >= self.priority {
            self.priority = priority;
            self.steps = parse(s);
            self.at = 0;
            self.hold = 0;
        }
    }

    /// `Palette_Step` @ 0x437690, once per tick.
    pub fn step(&mut self) {
        if self.at >= self.steps.len() {
            return;
        }
        if self.hold > 0 {
            self.hold -= 1;
            return;
        }
        match self.steps[self.at] {
            // The palette keeps the last remap shown; every script on the
            // CD ends on the identity.
            Step::Show(r) => self.remap = (r != IDENTITY).then_some(r),
            Step::Hold(n) => self.hold = n,
        }
        self.at += 1;
        if self.at >= self.steps.len() {
            self.steps.clear();
            self.at = 0;
            self.priority = 0;
        }
    }

    /// The current remap, identity when no script runs.
    pub fn current(&self) -> Remap {
        self.remap.unwrap_or(IDENTITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_includes_both_ends() {
        let s = parse("L-5 0:0/0:0/255:0=0:255/0:255/0:255#");
        assert_eq!(s.len(), 5);
        assert_eq!(s[0], Step::Show([0, 0, 0, 0, 255, 0]));
        assert_eq!(s[4], Step::Show(IDENTITY));
    }

    #[test]
    fn negative_screen_holds_then_fades() {
        let mut f = Flash::default();
        f.play(0x1b, 100);
        f.step();
        assert_eq!(f.current(), [255, 0, 255, 0, 255, 0], "inverted");
        for _ in 0..600 {
            f.step();
        }
        assert_eq!(f.current(), [255, 0, 255, 0, 255, 0], "still inverted 25 s later");
        for _ in 0..200 {
            f.step();
        }
        assert_eq!(f.current(), IDENTITY, "back to normal");
        // A low-priority script can't interrupt a higher one.
        f.play(0x1a, 100);
        f.step();
        f.play(0x1d, 0);
        f.step();
        assert_eq!(f.current(), [255, 255, 255, 255, 255, 255]);
    }

    #[test]
    fn the_typo_reads_like_atoi() {
        let s = parse("L-5 0:60:200/60:200/60:200=0:255/0:255/0:255#");
        assert_eq!(s[0], Step::Show([0, 60, 60, 200, 60, 200]));
    }
}
