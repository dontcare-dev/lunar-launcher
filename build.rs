use std::{env, fs::File, io::BufWriter, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/icons/moon.svg");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is missing"));
    let (rgba, size) = moon_pixels(64);
    let file = File::create(out.join("moon.png")).expect("Could not create moon PNG");
    let mut encoder = png::Encoder::new(BufWriter::new(file), size, size);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().expect("Could not write moon PNG header")
        .write_image_data(&rgba).expect("Could not write moon PNG data");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let icon_path = out.join("moon.ico");
        std::fs::write(&icon_path, make_ico(&rgba, size)).expect("Could not write moon ICO");
        let mut resource = winres::WindowsResource::new();
        resource.set_icon(icon_path.to_str().expect("Invalid icon path"));
        resource.compile().expect("Could not embed the Lunar moon application icon");
    }
}

fn moon_pixels(size: u32) -> (Vec<u8>, u32) {
    let mut pixels = vec![0_u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let px = x as f32;
            let py = y as f32;
            let dx = px - 25.0;
            let dy = py - 37.0;
            let outer = dx * dx + dy * dy <= 23.0 * 23.0;
            let cut_x = px - 39.0;
            let cut_y = py - 22.0;
            let cutout = cut_x * cut_x + cut_y * cut_y <= 20.0 * 20.0;
            let star_x = px - 48.0;
            let star_y = py - 13.0;
            let star = (star_x.abs() < 2.0 && star_y.abs() < 8.0)
                || (star_y.abs() < 2.0 && star_x.abs() < 8.0);
            if (outer && !cutout) || star {
                let t = py / size as f32;
                let i = ((y * size + x) * 4) as usize;
                pixels[i] = (185.0 - 55.0 * t) as u8;
                pixels[i + 1] = (204.0 - 42.0 * t) as u8;
                pixels[i + 2] = 255;
                pixels[i + 3] = 255;
            }
        }
    }
    (pixels, size)
}

fn make_ico(rgba: &[u8], size: u32) -> Vec<u8> {
    let image_bytes = size * size * 4;
    let mask_row = size.div_ceil(32) * 4;
    let mask_bytes = mask_row * size;
    let resource_size = 40 + image_bytes + mask_bytes;
    let mut out = Vec::with_capacity((22 + resource_size) as usize);
    out.extend_from_slice(&0_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.push(size as u8);
    out.push(size as u8);
    out.push(0);
    out.push(0);
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&32_u16.to_le_bytes());
    out.extend_from_slice(&resource_size.to_le_bytes());
    out.extend_from_slice(&22_u32.to_le_bytes());
    out.extend_from_slice(&40_u32.to_le_bytes());
    out.extend_from_slice(&(size as i32).to_le_bytes());
    out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&32_u16.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&image_bytes.to_le_bytes());
    out.extend_from_slice(&0_i32.to_le_bytes());
    out.extend_from_slice(&0_i32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    out.extend_from_slice(&0_u32.to_le_bytes());
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out.resize(out.len() + mask_bytes as usize, 0);
    out
}
