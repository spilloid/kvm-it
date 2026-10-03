//! Tiny grayscale frame type and perceptual-ish comparison used by `wait_for`.
//! Frames are box-downscaled to a fixed grid so capture resolution/scaling/compression noise largely cancels.

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Row-major 8-bit luma, `width * height` bytes.
    pub gray: Vec<u8>,
}

pub const GRID_W: usize = 64;
pub const GRID_H: usize = 36;

impl Frame {
    pub fn new(width: usize, height: usize, gray: Vec<u8>) -> Option<Frame> {
        (width > 0 && height > 0 && gray.len() == width * height).then_some(Frame { width, height, gray })
    }

    /// Area-average downscale to the comparison grid (upscales by replication if smaller).
    pub fn thumb(&self) -> Vec<u8> {
        let mut out = vec![0u8; GRID_W * GRID_H];
        for gy in 0..GRID_H {
            let y0 = gy * self.height / GRID_H;
            let y1 = ((gy + 1) * self.height / GRID_H).max(y0 + 1).min(self.height);
            for gx in 0..GRID_W {
                let x0 = gx * self.width / GRID_W;
                let x1 = ((gx + 1) * self.width / GRID_W).max(x0 + 1).min(self.width);
                let mut sum = 0u32;
                for y in y0..y1 {
                    for x in x0..x1 {
                        sum += self.gray[y * self.width + x] as u32;
                    }
                }
                out[gy * GRID_W + gx] = (sum / ((y1 - y0) * (x1 - x0)) as u32) as u8;
            }
        }
        out
    }

    /// 1.0 = identical thumbnails, 0.0 = maximally different. Mean absolute difference, normalised.
    pub fn similarity(&self, other: &Frame) -> f64 {
        let (a, b) = (self.thumb(), other.thumb());
        let total: u64 = a.iter().zip(&b).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as u64).sum();
        1.0 - total as f64 / (a.len() as f64 * 255.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: usize, h: usize, v: u8) -> Frame {
        Frame::new(w, h, vec![v; w * h]).unwrap()
    }

    #[test]
    fn identical_and_scale_invariant() {
        assert_eq!(solid(1920, 1080, 90).similarity(&solid(640, 360, 90)), 1.0);
    }

    #[test]
    fn different_content_scores_lower_and_small_noise_stays_high() {
        let black = solid(640, 360, 0);
        let white = solid(640, 360, 255);
        assert!(black.similarity(&white) < 0.01);
        let mut noisy = solid(640, 360, 100);
        for (i, p) in noisy.gray.iter_mut().enumerate() {
            *p = p.saturating_add((i % 7) as u8);
        }
        assert!(solid(640, 360, 100).similarity(&noisy) > 0.98);
    }

    #[test]
    fn half_split_picture_is_not_a_solid() {
        let mut f = solid(128, 72, 0);
        for y in 0..72 {
            for x in 64..128 {
                f.gray[y * 128 + x] = 255;
            }
        }
        let s = f.similarity(&solid(128, 72, 0));
        assert!((0.45..0.55).contains(&s), "{s}");
    }

    #[test]
    fn rejects_bad_dimensions() {
        assert!(Frame::new(0, 4, vec![]).is_none());
        assert!(Frame::new(2, 2, vec![0; 3]).is_none());
    }
}
