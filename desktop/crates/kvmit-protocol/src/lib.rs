//! kvm-it wire protocol v1 — framing only (see `protocol/SPEC.md`). No I/O, no allocation on decode.
pub mod message;

pub const VERSION: u8 = 1;
pub const MAX_PAYLOAD: usize = 240;
pub const HEADER_LEN: usize = 6;
pub const OVERHEAD: usize = HEADER_LEN + 2;

pub const FLAG_ACK_REQ: u8 = 0b01;
pub const FLAG_RESPONSE: u8 = 0b10;

/// Message types defined by protocol v1.
pub mod msg {
    pub const HELLO: u8 = 0x01;
    pub const KEY_DOWN: u8 = 0x10;
    pub const KEY_UP: u8 = 0x11;
    pub const KEY_TAP: u8 = 0x12;
    pub const MOUSE_MOVE: u8 = 0x20;
    pub const MOUSE_BUTTON_DOWN: u8 = 0x21;
    pub const MOUSE_BUTTON_UP: u8 = 0x22;
    pub const SCROLL: u8 = 0x23;
    pub const RELEASE_ALL: u8 = 0x30;
    pub const PING: u8 = 0x40;
    pub const STATUS: u8 = 0x50;
    pub const SET_NAME: u8 = 0x60;
    pub const SET_BOOT_DRIVE: u8 = 0x61;
    pub const ERROR: u8 = 0x7F;

    pub fn is_known(t: u8) -> bool {
        matches!(
            t,
            HELLO | KEY_DOWN | KEY_UP | KEY_TAP | MOUSE_MOVE | MOUSE_BUTTON_DOWN
                | MOUSE_BUTTON_UP | SCROLL | RELEASE_ALL | PING | STATUS | SET_NAME | SET_BOOT_DRIVE | ERROR
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    LengthMismatch,
    PayloadTooLong,
    BadCrc,
    BadVersion,
    BadFlags,
    UnknownType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    PayloadTooLong,
    BufferTooSmall,
}

/// A decoded frame borrowing its payload from the input buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub ver: u8,
    pub msg_type: u8,
    pub flags: u8,
    pub seq: u8,
    pub payload: &'a [u8],
}

impl Frame<'_> {
    pub fn ack_requested(&self) -> bool {
        self.flags & FLAG_ACK_REQ != 0
    }
    pub fn is_response(&self) -> bool {
        self.flags & FLAG_RESPONSE != 0
    }
}

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no reflection, xorout 0).
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// Encode a frame into `out`, returning the number of bytes written.
pub fn encode(
    msg_type: u8,
    flags: u8,
    seq: u8,
    payload: &[u8],
    out: &mut [u8],
) -> Result<usize, EncodeError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(EncodeError::PayloadTooLong);
    }
    let total = payload.len() + OVERHEAD;
    if out.len() < total {
        return Err(EncodeError::BufferTooSmall);
    }
    out[0] = VERSION;
    out[1] = msg_type;
    out[2] = flags;
    out[3] = seq;
    out[4..6].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    out[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
    let crc = crc16(&out[..HEADER_LEN + payload.len()]);
    out[HEADER_LEN + payload.len()..total].copy_from_slice(&crc.to_le_bytes());
    Ok(total)
}

pub fn encode_vec(msg_type: u8, flags: u8, seq: u8, payload: &[u8]) -> Result<Vec<u8>, EncodeError> {
    let mut v = vec![0u8; payload.len() + OVERHEAD];
    let n = encode(msg_type, flags, seq, payload, &mut v)?;
    v.truncate(n);
    Ok(v)
}

/// Decode one frame; checks follow the order in SPEC.md "Decoding rules".
pub fn decode(buf: &[u8]) -> Result<Frame<'_>, DecodeError> {
    if buf.len() < OVERHEAD {
        return Err(DecodeError::Truncated);
    }
    let len = u16::from_le_bytes([buf[4], buf[5]]) as usize;
    if buf.len() != len + OVERHEAD {
        return Err(DecodeError::LengthMismatch);
    }
    if len > MAX_PAYLOAD {
        return Err(DecodeError::PayloadTooLong);
    }
    let body = &buf[..HEADER_LEN + len];
    let want = u16::from_le_bytes([buf[HEADER_LEN + len], buf[HEADER_LEN + len + 1]]);
    if crc16(body) != want {
        return Err(DecodeError::BadCrc);
    }
    if buf[0] != VERSION {
        return Err(DecodeError::BadVersion);
    }
    if buf[2] & !(FLAG_ACK_REQ | FLAG_RESPONSE) != 0 {
        return Err(DecodeError::BadFlags);
    }
    if !msg::is_known(buf[1]) {
        return Err(DecodeError::UnknownType);
    }
    Ok(Frame { ver: buf[0], msg_type: buf[1], flags: buf[2], seq: buf[3], payload: &buf[HEADER_LEN..HEADER_LEN + len] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn encode_rejects_oversize_and_small_buffer() {
        let mut out = [0u8; 300];
        assert_eq!(encode(msg::PING, 0, 0, &[0; 241], &mut out), Err(EncodeError::PayloadTooLong));
        assert_eq!(encode(msg::PING, 0, 0, &[1], &mut out[..8]), Err(EncodeError::BufferTooSmall));
    }

    #[test]
    fn decode_never_panics_on_short_inputs() {
        let good = encode_vec(msg::KEY_DOWN, FLAG_ACK_REQ, 1, &[4]).unwrap();
        for n in 0..good.len() {
            assert!(decode(&good[..n]).is_err());
        }
    }
}
