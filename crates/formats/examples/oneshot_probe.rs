//! Lists every non-looping appearance state that animates (some polygon
//! has more than one frame): its frame counts, which appearance states of
//! the object loop, and which state entries / transitions select it.
//! A one-shot like this plays once, then the object goes back to the
//! looping state it showed before (`0x423810` → `0x423350`).
//! Usage: oneshot_probe <worlds file>
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let data = std::fs::read(std::env::args().nth(1).expect("worlds file")).unwrap();
    let a = Archive::parse(&data).unwrap();
    for s in 0..8 {
        let Ok(sec) = a.world(s) else { continue };
        for (i, o) in sec.objects.iter().enumerate() {
            let Object::World(w) = o else { continue };
            let Some(app) = &w.appearance else { continue };
            for (k, st) in app.iter().enumerate() {
                let counts: Vec<usize> = st.polygons.iter().map(|f| f.len()).collect();
                if st.looping != 0 || !counts.iter().any(|&c| c > 1) {
                    continue;
                }
                let loops: Vec<u16> = app.iter().map(|x| x.looping).collect();
                let mut by = vec![];
                if let Some(sm) = &w.logic.states {
                    for (si, x) in sm.states.iter().enumerate() {
                        if x.entry.appearance == Some(k as i16 + 1) {
                            by.push(format!("entry of {si}"));
                        }
                        for t in &x.transitions {
                            if t.actions.appearance == Some(k as i16 + 1) {
                                by.push(format!("transition in {si}"));
                            }
                        }
                    }
                }
                let (lo, hi) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
                println!(
                    "s{s} #{} {} appearance {k}: frames {lo}..{hi} (polygon 0: {}), looping {loops:?}, selected by {by:?}",
                    i + 1,
                    String::from_utf8_lossy(&w.kind),
                    counts[0]
                );
            }
        }
    }
}
