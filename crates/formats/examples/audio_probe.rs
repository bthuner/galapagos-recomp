//! Loudness sanity check of every sound bank and the music: peak, RMS (dBFS),
//! DC offset and length. What an ear would notice: clipping, clicks from DC
//! offset, silent slots, and effects much louder than the music.
use galapagos_formats::smacker::Smacker;
use galapagos_formats::worlds::{Archive, SoundSet};

fn stats(s: &[i16]) -> (f32, f32, f32) {
    let peak = s.iter().map(|&x| (x as f32).abs()).fold(0.0, f32::max) / 32768.0;
    let rms = (s.iter().map(|&x| (x as f32 / 32768.0).powi(2)).sum::<f32>() / s.len().max(1) as f32).sqrt();
    let dc = s.iter().map(|&x| x as f32).sum::<f32>() / s.len().max(1) as f32 / 32768.0;
    (peak, rms, dc)
}
fn db(x: f32) -> f32 { 20.0 * x.max(1e-6).log10() }

fn bank(name: &str, data: &[u8], set: &SoundSet) -> Vec<f32> {
    let mut rmss = vec![];
    let (mut clip, mut dcs, mut n) = (0, 0, 0);
    for s in set.samples(data, std::env::var("Q").map_or(4, |q| q.parse().unwrap())).unwrap().into_iter().flatten() {
        let pcm: Vec<i16> = if s.bits == 8 { s.pcm.iter().map(|&b| ((b as i16) - 128) << 8).collect() }
                            else { s.pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect() };
        let (p, r, dc) = stats(&pcm);
        // Real clipping = flat tops: 3+ consecutive samples at the rail.
        let flat = pcm.windows(3).filter(|w| w.iter().all(|&x| x >= 32512 || x <= -32768)).count();
        n += 1; if p >= 0.999 && flat > 0 { clip += 1 } if dc.abs() > 0.02 { dcs += 1 }
        rmss.push(db(r));
    }
    rmss.sort_by(|a, b| a.total_cmp(b));
    let med = rmss.get(rmss.len() / 2).copied().unwrap_or(0.0);
    println!("{name:>10}: {n:3} sounds, RMS median {med:6.1} dBFS (min {:6.1}, max {:6.1}), with flat-topped clipping: {clip}, DC offset > 2%: {dcs}",
        rmss.first().unwrap_or(&0.0), rmss.last().unwrap_or(&0.0));
    rmss
}

fn main() {
    let dir = std::env::args().nth(1).unwrap();
    let data = std::fs::read(format!("{dir}/worlds")).unwrap();
    let a = Archive::parse(&data).unwrap();
    for s in 0..a.sections.len() { bank(&format!("section {s}"), &data, &a.world(s).unwrap().sounds); }
    bank("mendel", &data, &SoundSet::parse(&data, a.header.creature_sounds as usize).unwrap());
    bank("predator", &data, &SoundSet::parse(&data, a.header.predator_sounds as usize).unwrap());
    for f in 1..=10 {
        let mut m = Smacker::from_vec(std::fs::read(format!("{dir}/{f}")).unwrap()).unwrap();
        let mut pcm = vec![];
        while m.frames_remaining() > 0 { pcm.extend_from_slice(&m.decode_next_frame().unwrap().audio[0]); }
        let (p, r, dc) = stats(&pcm);
        println!("  music {f:2}: RMS {:6.1} dBFS, peak {:5.2}, DC {:+.3}", db(r), p, dc);
    }
}
