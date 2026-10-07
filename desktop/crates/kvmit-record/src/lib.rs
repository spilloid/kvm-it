//! Session recording core (issue #29). Decoded RGBA frames are piped as `rawvideo` to an `ffmpeg` subprocess;
//! nothing here links an encoder. Layout:
//!
//! - [`args`]: pure ffmpeg argument building, settings, duration cap (unit-tested)
//! - [`naming`]: pure timestamped, never-overwriting output paths (unit-tested)
//! - [`pacer`]: pure wall-clock to frame-count pacing (unit-tested)
//! - [`audio`]: audio input device listing, pure parsers over `pactl` / `ffmpeg` output (unit-tested)
//! - [`recorder`]: the subprocess glue ([`Recorder`], [`poll_record`])
//!
//! Frame-size changes mid-recording: the encoder is fixed to the size of the FIRST frame; later frames of a
//! different size are nearest-neighbour rescaled to it (see [`resize_rgba_nearest`]). This is the simple honest
//! option (one file, no segments); a mid-recording resolution change costs quality, not the recording.
//!
//! Never logs frame contents. Nothing here is hardware-verified: it is host-tested against synthetic frames.
pub mod args;
pub mod audio;
pub mod naming;
pub mod pacer;
mod recorder;

pub use args::{build_args, AudioBackend, AudioInput, Format, Settings};
pub use audio::{list_audio_devices, AudioDevice};
pub use recorder::{
    check_ffmpeg, poll_record, Finished, PollHandle, PushOutcome, RecordError, Recorder,
};

/// Nearest-neighbour RGBA rescale (used when the capture size changes mid-recording).
pub fn resize_rgba_nearest(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh * 4];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || src.len() < sw * sh * 4 {
        return out;
    }
    for y in 0..dh {
        let sy = y * sh / dh;
        for x in 0..dw {
            let sx = x * sw / dw;
            let (s, d) = ((sy * sw + sx) * 4, (y * dw + x) * 4);
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::resize_rgba_nearest;

    #[test]
    fn resize_up_and_down() {
        // 2x1: red, blue -> 4x2
        let src = [255, 0, 0, 255, 0, 0, 255, 255];
        let up = resize_rgba_nearest(&src, 2, 1, 4, 2);
        assert_eq!(up.len(), 4 * 2 * 4);
        assert_eq!(&up[0..4], &[255, 0, 0, 255]);
        assert_eq!(&up[4..8], &[255, 0, 0, 255]);
        assert_eq!(&up[8..12], &[0, 0, 255, 255]);
        let down = resize_rgba_nearest(&up, 4, 2, 2, 1);
        assert_eq!(down, src);
    }

    #[test]
    fn resize_bad_input_is_blank_not_panic() {
        assert_eq!(resize_rgba_nearest(&[1, 2], 2, 2, 2, 2), vec![0; 16]);
    }
}
