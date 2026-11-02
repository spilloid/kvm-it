//! Tiny grayscale frame type and perceptual-ish comparison used by `wait_for`.
//! Frames are box-downscaled to a fixed grid so capture resolution/scaling/compression noise largely cancels.

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Row-major 8-bit luma, `width * height` bytes.
    pub gray: Vec<u8>,
    /// The capture's own frame counter, when the source has one. Two reads of the same captured frame carry the same
    /// number, so a stalled capture is not mistaken for a screen that stayed the same (workflows rely on this).
    pub seq: Option<u64>,
    /// When the capture backend received this picture from its driver (before decoding). A picture received before a
    /// decision was taken can never be evidence after it, however late it is delivered (workflows rely on this). The
    /// driver's own queue is not covered: that is what a workflow's capture latency window is for.
    pub captured_at: Option<std::time::Instant>,
}

pub const GRID_W: usize = 64;
pub const GRID_H: usize = 36;

impl Frame {
    pub fn new(width: usize, height: usize, gray: Vec<u8>) -> Option<Frame> {
        (width > 0 && height > 0 && gray.len() == width * height).then_some(Frame { width, height, gray, seq: None, captured_at: None })
    }

    pub fn with_seq(mut self, seq: Option<u64>) -> Frame {
        self.seq = seq;
        self
    }

    pub fn with_captured_at(mut self, at: Option<std::time::Instant>) -> Frame {
        self.captured_at = at;
        self
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

    /// How alike the *pattern* of light and dark is: normalised cross-correlation of the thumbnails, mapped from
    /// -1..1 to 0..1. Brightness and contrast changes cancel out, and screens that share a background but differ in
    /// text and layout score far apart (Windows OOBE screens are 96-98 % alike by [`Frame::similarity`], at most
    /// 84 % by this). Flat thumbnails have no pattern: two flat ones of nearly the same level score 1, otherwise 0.
    pub fn structure_similarity(&self, other: &Frame) -> f64 {
        let (a, b) = (self.thumb(), other.thumb());
        if a == b {
            return 1.0;
        }
        let mean = |v: &[u8]| v.iter().map(|x| *x as f64).sum::<f64>() / v.len() as f64;
        let (ma, mb) = (mean(&a), mean(&b));
        let (mut ab, mut aa, mut bb) = (0.0, 0.0, 0.0);
        for (x, y) in a.iter().zip(&b) {
            let (dx, dy) = (*x as f64 - ma, *y as f64 - mb);
            ab += dx * dy;
            aa += dx * dx;
            bb += dy * dy;
        }
        if aa < 1e-9 || bb < 1e-9 {
            return if aa < 1e-9 && bb < 1e-9 && (ma - mb).abs() <= 2.0 { 1.0 } else { 0.0 };
        }
        ((ab / (aa.sqrt() * bb.sqrt()) + 1.0) / 2.0).clamp(0.0, 1.0)
    }

    /// The part of the frame inside a rectangle given as fractions of its width and height (clamped; at least one
    /// pixel each way). The capture number carries over.
    pub fn crop(&self, x: f64, y: f64, w: f64, h: f64) -> Frame {
        let clamp = |v: f64| v.clamp(0.0, 1.0);
        let x0 = ((clamp(x) * self.width as f64) as usize).min(self.width - 1);
        let y0 = ((clamp(y) * self.height as f64) as usize).min(self.height - 1);
        let x1 = ((clamp(x + w) * self.width as f64) as usize).clamp(x0 + 1, self.width);
        let y1 = ((clamp(y + h) * self.height as f64) as usize).clamp(y0 + 1, self.height);
        let mut gray = Vec::with_capacity((x1 - x0) * (y1 - y0));
        for row in y0..y1 {
            gray.extend_from_slice(&self.gray[row * self.width + x0..row * self.width + x1]);
        }
        Frame { width: x1 - x0, height: y1 - y0, gray, seq: self.seq, captured_at: self.captured_at }
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
    fn structure_tells_apart_what_brightness_cannot() {
        // Same background, a dark bar in a different place: nearly equal by brightness, far apart by structure.
        let bar = |x0: usize| {
            let mut f = solid(128, 72, 230);
            for y in 10..14 {
                for x in x0..x0 + 30 {
                    f.gray[y * 128 + x] = 20;
                }
            }
            f
        };
        let (a, b) = (bar(10), bar(80));
        assert!(a.similarity(&b) > 0.95, "{}", a.similarity(&b));
        assert!(a.structure_similarity(&b) < 0.8, "{}", a.structure_similarity(&b));
        assert!(a.structure_similarity(&a) > 0.999);
        // Brighter but the same picture still matches.
        let mut brighter = a.clone();
        for p in brighter.gray.iter_mut() {
            *p = p.saturating_add(10);
        }
        assert!(a.structure_similarity(&brighter) > 0.99);
        // Flat frames: equal levels match, different levels do not.
        assert_eq!(solid(64, 36, 100).structure_similarity(&solid(64, 36, 101)), 1.0);
        assert_eq!(solid(64, 36, 100).structure_similarity(&solid(64, 36, 160)), 0.0);
    }

    #[test]
    fn identical_pictures_score_exactly_one() {
        // Floating point would make this 0.9999999999999999, failing a threshold of 1.0 against its own reference.
        let mut g = vec![1u8; 64 * 36];
        g[0] = 0;
        g[1] = 2;
        let f = Frame::new(64, 36, g).unwrap();
        assert_eq!(f.structure_similarity(&f.clone()), 1.0);
    }

    #[test]
    fn crops_by_fraction_and_keeps_the_frame_number() {
        let mut f = solid(100, 50, 0).with_seq(Some(7));
        f.gray[10 * 100 + 60] = 255;
        let c = f.crop(0.5, 0.2, 0.5, 0.2);
        assert_eq!((c.width, c.height, c.seq), (50, 10, Some(7)));
        assert_eq!(c.gray[10], 255);
        let tiny = f.crop(0.99, 0.99, 0.5, 0.5);
        assert!(tiny.width >= 1 && tiny.height >= 1);
    }

    #[test]
    fn rejects_bad_dimensions() {
        assert!(Frame::new(0, 4, vec![]).is_none());
        assert!(Frame::new(2, 2, vec![0; 3]).is_none());
    }
}
