//! Wall-clock pacing. The encoder is told a constant frame rate, so each frame written stands for 1/fps
//! seconds; the pacer turns "a frame arrived at elapsed time t" into "write it N times" (N=0 drops a frame that
//! came too early, N>1 repeats it to cover a gap), keeping playback speed equal to real time.
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Pacer {
    fps: u32,
    emitted: u64,
}

impl Pacer {
    pub fn new(fps: u32) -> Self {
        Pacer {
            fps: fps.max(1),
            emitted: 0,
        }
    }

    /// Frames emitted so far.
    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    /// How many copies of a frame arriving at `elapsed` (since recording start) to emit now. The first call
    /// at t=0 returns 1. Records the emission.
    pub fn frames_due(&mut self, elapsed: Duration) -> u32 {
        let target = (elapsed.as_secs_f64() * self.fps as f64).floor() as u64 + 1;
        let n = target.saturating_sub(self.emitted);
        self.emitted += n;
        n.min(u32::MAX as u64) as u32
    }

    /// Like [`Pacer::frames_due`] but never lets the total exceed `max_frames`.
    pub fn frames_due_capped(&mut self, elapsed: Duration, max_frames: Option<u64>) -> u32 {
        let Some(max) = max_frames else {
            return self.frames_due(elapsed);
        };
        let room = max.saturating_sub(self.emitted);
        let target = (elapsed.as_secs_f64() * self.fps as f64).floor() as u64 + 1;
        let n = target.saturating_sub(self.emitted).min(room);
        self.emitted += n;
        n as u32
    }

    pub fn interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn steady_arrival() {
        let mut p = Pacer::new(10);
        assert_eq!(p.frames_due(MS(0)), 1);
        assert_eq!(p.frames_due(MS(100)), 1);
        assert_eq!(p.frames_due(MS(200)), 1);
        assert_eq!(p.emitted(), 3);
    }

    #[test]
    fn early_frames_dropped_gaps_filled() {
        let mut p = Pacer::new(10);
        assert_eq!(p.frames_due(MS(0)), 1);
        assert_eq!(p.frames_due(MS(30)), 0); // too early
        assert_eq!(p.frames_due(MS(450)), 4); // frames for 100,200,300,400
        assert_eq!(p.emitted(), 5);
    }

    #[test]
    fn cap() {
        let mut p = Pacer::new(10);
        assert_eq!(p.frames_due_capped(MS(0), Some(3)), 1);
        assert_eq!(p.frames_due_capped(MS(1000), Some(3)), 2);
        assert_eq!(p.frames_due_capped(MS(2000), Some(3)), 0);
        assert_eq!(p.emitted(), 3);
    }
}
