//! Writes texture `t` of section `s` as a PPM and prints palette-index stats.
use galapagos_formats::worlds::Archive;
fn main() {
    let mut a = std::env::args().skip(1);
    let data = std::fs::read(a.next().unwrap()).unwrap();
    let s: usize = a.next().unwrap().parse().unwrap();
    let t: usize = a.next().unwrap().parse().unwrap();
    let out = a.next().unwrap();
    let ar = Archive::parse(&data).unwrap();
    let sec = ar.world(s).unwrap();
    let px = sec.textures[t];
    let mut hist = [0u32; 256];
    px.iter().for_each(|&p| hist[p as usize] += 1);
    let mut top: Vec<(usize, u32)> = hist.iter().copied().enumerate().filter(|x| x.1 > 0).collect();
    top.sort_by_key(|x| std::cmp::Reverse(x.1));
    println!("texture {t}: index 0 = {} of {} px; top indices {:?}", hist[0], px.len(), &top[..top.len().min(6)]);
    println!("palette[0] = {:?}, palette[top] = {:?}", sec.palette[0], sec.palette[top[0].0]);
    let mut ppm = format!("P6 128 128 255\n").into_bytes();
    ppm.extend(sec.texture_rgb(t));
    std::fs::write(out, ppm).unwrap();
}
