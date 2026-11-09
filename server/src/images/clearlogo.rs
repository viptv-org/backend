use std::io::Cursor;

pub(super) fn normalize(bytes: &[u8]) -> Result<Vec<u8>, ()> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_limits(png::Limits { bytes: 16 * 1024 * 1024 });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|_| ())?;
    let (w, h) = (reader.info().width, reader.info().height);
    if w == 0 || h == 0 || w > 4096 || h > 2048 || w as u64 * h as u64 > 2_097_152 { return Err(()); }
    let mut data = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut data).map_err(|_| ())?;
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    let samples = info.color_type.samples();
    for p in data[..info.buffer_size()].chunks_exact(samples) {
        match info.color_type {
            png::ColorType::Rgba => rgba.extend_from_slice(p),
            png::ColorType::Rgb => rgba.extend_from_slice(&[p[0],p[1],p[2],255]),
            png::ColorType::Grayscale => rgba.extend_from_slice(&[p[0],p[0],p[0],255]),
            png::ColorType::GrayscaleAlpha => rgba.extend_from_slice(&[p[0],p[0],p[0],p[1]]),
            _ => return Err(()),
        }
    }
    prepare(&mut rgba, w, h);
    let mut left = w;let mut top = h;let mut right = 0;let mut bottom = 0;
    for y in 0..h { for x in 0..w {
        if rgba[((y*w+x)*4+3) as usize] > 4 { left=left.min(x);top=top.min(y);right=right.max(x+1);bottom=bottom.max(y+1); }
    } }
    if left >= right || top >= bottom { return Err(()); }
    let mut trimmed = Vec::with_capacity(((right-left)*(bottom-top)*4) as usize);
    for y in top..bottom { trimmed.extend_from_slice(&rgba[((y*w+left)*4) as usize..((y*w+right)*4) as usize]); }
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, right-left, bottom-top);
        encoder.set_color(png::ColorType::Rgba);encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().map_err(|_| ())?.write_image_data(&trimmed).map_err(|_| ())?;
    }
    Ok(output)
}

fn prepare(rgba: &mut [u8], w: u32, h: u32) {
    let transparent = rgba.chunks_exact(4).any(|p| p[3] < 250);
    if !transparent {
        let corners = [0, w-1, (h-1)*w, w*h-1];
        let black = corners.iter().all(|i| rgba[*i as usize*4..*i as usize*4+3].iter().all(|c| *c < 16));
        let white = corners.iter().all(|i| rgba[*i as usize*4..*i as usize*4+3].iter().all(|c| *c > 239));
        if black || white { for p in rgba.chunks_exact_mut(4) {
            let maximum = *p[..3].iter().max().unwrap();let minimum = *p[..3].iter().min().unwrap();
            let alpha = if black { maximum } else { 255-minimum };
            for c in &mut p[..3] { *c = if alpha == 0 { 255 } else if black { (*c as u16*255/alpha as u16) as u8 } else { 255-((255-*c) as u16*255/alpha as u16) as u8 }; }
            p[3] = alpha;
        } }
    }
    let mut weight = 0u64;let mut dark_neutral = 0u64;let mut colored=0u64;
    for p in rgba.chunks_exact(4) {
        let a=p[3] as u64;weight+=a;
        let max=*p[..3].iter().max().unwrap();let min=*p[..3].iter().min().unwrap();
        if max < 112 && max-min < 24 { dark_neutral+=a; }
        if max-min >= 24 { colored+=a; }
    }
    if weight > 0 && dark_neutral*100 > weight*90 && colored*100 < weight {
        for p in rgba.chunks_exact_mut(4) { p[..3].fill(255); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dark_transparent_wordmarks_become_legible_without_a_background() {
        let mut pixels = [0,0,0,0, 0,0,0,255, 2,2,2,128];
        prepare(&mut pixels, 3, 1);
        assert_eq!(pixels, [255,255,255,0, 255,255,255,255, 255,255,255,128]);
    }
    #[test]
    fn colorful_and_light_transparent_logos_keep_their_original_colors() {
        let mut pixels=[0,0,0,0, 200,25,40,255, 240,240,240,255];let original=pixels;
        prepare(&mut pixels,3,1);assert_eq!(pixels,original);
    }
    #[test]
    fn opaque_dark_matte_is_removed_without_turning_into_a_white_rectangle() {
        let mut pixels=[0,0,0,255, 255,255,255,255, 0,0,0,255];
        prepare(&mut pixels,3,1);assert_eq!(pixels[3],0);assert_eq!(pixels[7],255);assert_eq!(pixels[11],0);
    }
    #[test]
    fn invalid_and_oversized_decodes_are_rejected() { assert!(normalize(b"not an image").is_err()); }
}
