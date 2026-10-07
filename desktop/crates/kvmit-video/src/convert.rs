//! Pixel-format conversion (pure, tested without hardware).
use kvmit_script::Frame;

/// YUYV (YUY2, 4:2:2) → RGBA8 using BT.601 limited-range coefficients, which is what UVC capture cards emit.
/// Rows are tightly packed (`width * 2` bytes).
pub fn yuyv_to_rgba(width: usize, height: usize, data: &[u8]) -> Option<Vec<u8>> {
    yuyv_to_rgba_strided(width, height, width * 2, data)
}

/// As [`yuyv_to_rgba`] for a buffer whose rows are `stride` bytes apart (drivers may pad each row).
pub fn yuyv_to_rgba_strided(width: usize, height: usize, stride: usize, data: &[u8]) -> Option<Vec<u8>> {
    let row = width * 2;
    if width == 0 || !width.is_multiple_of(2) || stride < row || height == 0 || data.len() < stride * (height - 1) + row {
        return None;
    }
    let mut out = vec![255u8; width * height * 4];
    let clamp = |v: i32| v.clamp(0, 255) as u8;
    for y_row in 0..height {
        let src = &data[y_row * stride..y_row * stride + row];
        for (i, px) in src.as_chunks::<4>().0.iter().enumerate() {
            let (y0, u, y1, v) = (px[0] as i32, px[1] as i32 - 128, px[2] as i32, px[3] as i32 - 128);
            for (j, y) in [y0, y1].into_iter().enumerate() {
                let c = 298 * (y - 16);
                let o = (y_row * width + i * 2 + j) * 4;
                out[o] = clamp((c + 409 * v + 128) >> 8);
                out[o + 1] = clamp((c - 100 * u - 208 * v + 128) >> 8);
                out[o + 2] = clamp((c + 516 * u + 128) >> 8);
            }
        }
    }
    Some(out)
}

pub fn decode_mjpeg(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let img = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).ok()?.into_rgba8();
    Some((img.width() as usize, img.height() as usize, img.into_raw()))
}

/// Rec.601 luma, integer math.
pub fn rgba_to_gray_frame(width: usize, height: usize, rgba: &[u8]) -> Frame {
    let gray = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .take(width * height)
        .map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8)
        .collect();
    Frame::new(width, height, gray).expect("rgba length matches dimensions")
}

/// Load a reference image (PNG/JPEG) as a comparison frame.
pub fn load_reference(path: &std::path::Path) -> Result<Frame, String> {
    let img = image::open(path).map_err(|e| format!("{}: {e}", path.display()))?.into_rgba8();
    Ok(rgba_to_gray_frame(img.width() as usize, img.height() as usize, img.as_raw()))
}

/// Save RGBA8 pixels as PNG.
pub fn save_png(path: &std::path::Path, width: usize, height: usize, rgba: &[u8]) -> Result<(), String> {
    image::save_buffer(path, rgba, width as u32, height as u32, image::ColorType::Rgba8).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuyv_stride_padding_is_skipped() {
        // 2x2, two bytes of junk after each 4-byte row.
        let data = [16, 128, 235, 128, 0xAA, 0xBB, 235, 128, 16, 128];
        let rgba = yuyv_to_rgba_strided(2, 2, 6, &data).unwrap();
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255]);
        assert_eq!(&rgba[8..12], &[255, 255, 255, 255], "row 1 starts at the stride, not at width*2");
        assert_eq!(&rgba[12..16], &[0, 0, 0, 255]);
        assert!(yuyv_to_rgba_strided(2, 2, 3, &data).is_none(), "stride below the row size");
        assert!(yuyv_to_rgba_strided(2, 2, 6, &data[..8]).is_none(), "short buffer");
        assert!(yuyv_to_rgba_strided(0, 2, 0, &[]).is_none());
    }

    #[test]
    fn yuyv_black_white_and_gray() {
        // Y=16 -> black, Y=235 -> white, neutral chroma.
        let rgba = yuyv_to_rgba(2, 1, &[16, 128, 235, 128]).unwrap();
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn yuyv_chroma_moves_channels_the_right_way() {
        let red = yuyv_to_rgba(2, 1, &[81, 90, 81, 240]).unwrap(); // BT.601 red
        assert!(red[0] > 200 && red[1] < 60 && red[2] < 60, "{red:?}");
        let blue = yuyv_to_rgba(2, 1, &[41, 240, 41, 110]).unwrap();
        assert!(blue[2] > 200 && blue[0] < 60, "{blue:?}");
    }

    #[test]
    fn yuyv_rejects_bad_geometry() {
        assert!(yuyv_to_rgba(3, 1, &[0; 6]).is_none());
        assert!(yuyv_to_rgba(2, 2, &[0; 4]).is_none());
    }

    #[test]
    fn mjpeg_round_trip_and_garbage() {
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([200, 30, 30]));
        let mut jpg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 90).encode_image(&img).unwrap();
        let (w, h, rgba) = decode_mjpeg(&jpg).unwrap();
        assert_eq!((w, h), (16, 16));
        assert!(rgba[0] > 150 && rgba[1] < 80);
        assert!(decode_mjpeg(&[1, 2, 3]).is_none());
    }

    #[test]
    fn gray_conversion() {
        let f = rgba_to_gray_frame(2, 1, &[255, 255, 255, 255, 0, 0, 0, 255]);
        assert_eq!(f.gray, vec![255, 0]); // coefficients sum to 256, so white maps to exactly 255
    }
}
