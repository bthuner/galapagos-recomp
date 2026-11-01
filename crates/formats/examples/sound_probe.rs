//! Prints sound-set sizes and the range of emitter sound indices per section,
//! to settle whether emitter indices are 0- or 1-based.
use galapagos_formats::worlds::{Archive, Object};

fn main() {
    let path = std::env::args().nth(1).expect("path to worlds");
    let data = std::fs::read(path).unwrap();
    let a = Archive::parse(&data).unwrap();
    for s in 0..a.sections.len() {
        let sec = a.world(s).unwrap();
        let (mut lo, mut hi, mut n) = (i16::MAX, i16::MIN, 0);
        for o in &sec.objects {
            let Object::World(w) = o else { continue };
            for e in &w.logic.sounds {
                for &i in &e.sounds {
                    if i >= 0 {
                        lo = lo.min(i);
                        hi = hi.max(i);
                        n += 1;
                    }
                }
            }
        }
        let samples = sec.sounds.samples(&data, 4).unwrap();
        let (mut miss0, mut miss1) = (0, 0);
        for o in &sec.objects {
            let Object::World(w) = o else { continue };
            for e in &w.logic.sounds {
                for &i in e.sounds.iter().filter(|&&i| i >= 0) {
                    if samples.get(i as usize).is_none_or(|x| x.is_none()) { miss0 += 1 }
                    if samples.get(i as usize - 1).is_none_or(|x| x.is_none()) { miss1 += 1 }
                }
            }
        }
        println!("  refs hitting empty/out-of-range slots: 0-based {miss0}, 1-based {miss1}");
        let empty = samples.iter().filter(|s| s.is_none()).count();
        let first_empty = samples.iter().position(|s| s.is_none());
        println!("section {s}: {} sounds ({empty} empty, first empty {first_empty:?}), emitter refs {n}, index range {lo}..={hi}", samples.len());
    }
    let h = &a.header;
    for (name, off) in [("creature", h.creature_sounds), ("predator", h.predator_sounds)] {
        let set = galapagos_formats::worlds::SoundSet::parse(&data, off as usize).unwrap();
        let s = set.samples(&data, 4).unwrap();
        println!("{name}: {} sounds, rates {:?}", s.len(), s.iter().map(|x| x.as_ref().map(|x| (x.sample_rate(), x.bits))).collect::<Vec<_>>());
    }
}
