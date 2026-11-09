//! `gen-icon <out.ico> [out.png] [size]` — export the Health icon (and a preview PNG of every pose).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ico = args.first().cloned().unwrap_or_else(|| "health.ico".into());
    if let Err(e) = health_icon::write_ico(std::path::Path::new(&ico)) {
        eprintln!("ico: {e}");
        std::process::exit(1);
    }
    println!("{ico}");
    if let Some(png) = args.get(1) {
        let size: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(256);
        // sheet with all poses side by side
        let poses = health_icon::Pose::all();
        let sheet_w = size * poses.len() as u32;
        let mut sheet = vec![0u8; (sheet_w * size * 4) as usize];
        for (i, p) in poses.iter().enumerate() {
            let img = health_icon::render_rgba(size, *p);
            for y in 0..size {
                let src = (y * size * 4) as usize;
                let dst = ((y * sheet_w + i as u32 * size) * 4) as usize;
                sheet[dst..dst + (size * 4) as usize].copy_from_slice(&img[src..src + (size * 4) as usize]);
            }
        }
        let mut buf = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut buf, sheet_w, size);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&sheet).expect("png data");
        }
        std::fs::write(png, buf).expect("write png");
        println!("{png}");
    }
}
