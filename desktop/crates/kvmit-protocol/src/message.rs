//! Typed requests and replies on top of the framing layer (SPEC.md "Messages").
use crate::{msg, Frame, FLAG_ACK_REQ};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Hello { major: u8, minor: u8 },
    KeyDown(u8),
    KeyUp(u8),
    KeyTap(u8),
    MouseMove { dx: i16, dy: i16 },
    ButtonDown(u8),
    ButtonUp(u8),
    Scroll { v: i8, h: i8 },
    ReleaseAll,
    Ping(Vec<u8>),
    Status,
    SetName(String),
    /// Show (or stop showing) the target the adapter's read-only boot drive. Off by default; the adapter restarts after acknowledging a change.
    SetBootDrive(bool),
}

impl Request {
    pub fn msg_type(&self) -> u8 {
        match self {
            Request::Hello { .. } => msg::HELLO,
            Request::KeyDown(_) => msg::KEY_DOWN,
            Request::KeyUp(_) => msg::KEY_UP,
            Request::KeyTap(_) => msg::KEY_TAP,
            Request::MouseMove { .. } => msg::MOUSE_MOVE,
            Request::ButtonDown(_) => msg::MOUSE_BUTTON_DOWN,
            Request::ButtonUp(_) => msg::MOUSE_BUTTON_UP,
            Request::Scroll { .. } => msg::SCROLL,
            Request::ReleaseAll => msg::RELEASE_ALL,
            Request::Ping(_) => msg::PING,
            Request::Status => msg::STATUS,
            Request::SetName(_) => msg::SET_NAME,
            Request::SetBootDrive(_) => msg::SET_BOOT_DRIVE,
        }
    }

    pub fn payload(&self) -> Vec<u8> {
        match self {
            Request::Hello { major, minor } => vec![*major, *minor],
            Request::KeyDown(u) | Request::KeyUp(u) | Request::KeyTap(u) => vec![*u],
            Request::MouseMove { dx, dy } => [dx.to_le_bytes(), dy.to_le_bytes()].concat(),
            Request::ButtonDown(m) | Request::ButtonUp(m) => vec![*m],
            Request::SetBootDrive(on) => vec![u8::from(*on)],
            Request::Scroll { v, h } => vec![*v as u8, *h as u8],
            Request::ReleaseAll | Request::Status => vec![],
            Request::Ping(p) => p.clone(),
            Request::SetName(n) => {
                let mut v = vec![n.len() as u8];
                v.extend_from_slice(n.as_bytes());
                v
            }
        }
    }

    /// Acked requests are retried with the same `seq`; motion, handshake, ping and status are not.
    pub fn wants_ack(&self) -> bool {
        !matches!(self, Request::MouseMove { .. } | Request::Hello { .. } | Request::Ping(_) | Request::Status)
    }

    /// Whether the device answers at all (everything except motion).
    pub fn expects_reply(&self) -> bool {
        !matches!(self, Request::MouseMove { .. })
    }

    pub fn flags(&self) -> u8 {
        if self.wants_ack() { FLAG_ACK_REQ } else { 0 }
    }

    pub fn encode(&self, seq: u8) -> Vec<u8> {
        crate::encode_vec(self.msg_type(), self.flags(), seq, &self.payload()).expect("request payloads fit")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloInfo {
    pub major: u8,
    pub minor: u8,
    pub caps: u32,
    pub fw: [u8; 3],
    pub uuid: [u8; 16],
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    pub hid_mounted: bool,
    pub keys: u8,
    pub buttons: u8,
    pub dropped_motion: u32,
    pub bad_crc: u32,
    /// Is the read-only boot drive presented to the target right now. `None` from firmware that predates it (protocol minor 0).
    pub boot_drive: Option<bool>,
}

/// `HelloInfo::caps` bit: the adapter understands `SET_BOOT_DRIVE` and reports the drive in `STATUS`.
pub const CAP_BOOT_DRIVE: u32 = 16;

impl HelloInfo {
    pub fn supports_boot_drive(&self) -> bool {
        self.caps & CAP_BOOT_DRIVE != 0
    }
}

pub mod error_code {
    pub const UNSUPPORTED: u8 = 1;
    pub const BAD_VERSION: u8 = 2;
    pub const BAD_FLAGS: u8 = 3;
    pub const BAD_PAYLOAD: u8 = 4;
    pub const NOT_READY: u8 = 5;
    pub const HID_NOT_MOUNTED: u8 = 6;
    pub const BUSY: u8 = 7;
    pub const REFUSED: u8 = 8;
    pub const MTU_TOO_SMALL: u8 = 9;

    pub fn describe(code: u8) -> &'static str {
        match code {
            UNSUPPORTED => "message not supported by this firmware",
            BAD_VERSION => "protocol version not supported",
            BAD_FLAGS => "invalid flags",
            BAD_PAYLOAD => "invalid payload",
            NOT_READY => "handshake required first",
            HID_NOT_MOUNTED => "the target computer has not enumerated the adapter (USB not connected, suspended or off)",
            BUSY => "adapter busy",
            REFUSED => "refused (too many simultaneous keys)",
            MTU_TOO_SMALL => "Bluetooth link MTU too small for the handshake (need >= 40)",
            _ => "unknown error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Ack,
    Hello(HelloInfo),
    Pong(Vec<u8>),
    Status(StatusInfo),
    Error { code: u8, orig_type: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    NotAResponse,
    BadPayload,
}

pub fn parse_reply(f: &Frame<'_>) -> Result<Reply, ParseError> {
    if !f.is_response() {
        return Err(ParseError::NotAResponse);
    }
    let p = f.payload;
    match f.msg_type {
        msg::ERROR => match p {
            [code, orig] => Ok(Reply::Error { code: *code, orig_type: *orig }),
            _ => Err(ParseError::BadPayload),
        },
        msg::HELLO => {
            if p.len() < 26 || p.len() != 26 + p[25] as usize {
                return Err(ParseError::BadPayload);
            }
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(&p[9..25]);
            Ok(Reply::Hello(HelloInfo {
                major: p[0],
                minor: p[1],
                caps: u32::from_le_bytes([p[2], p[3], p[4], p[5]]),
                fw: [p[6], p[7], p[8]],
                uuid,
                name: String::from_utf8_lossy(&p[26..]).into_owned(),
            }))
        }
        msg::PING => Ok(Reply::Pong(p.to_vec())),
        msg::STATUS => {
            if p.len() < 11 {
                return Err(ParseError::BadPayload);
            }
            Ok(Reply::Status(StatusInfo {
                hid_mounted: p[0] != 0,
                keys: p[1],
                buttons: p[2],
                dropped_motion: u32::from_le_bytes([p[3], p[4], p[5], p[6]]),
                bad_crc: u32::from_le_bytes([p[7], p[8], p[9], p[10]]),
                boot_drive: p.get(11).map(|b| *b != 0),
            }))
        }
        _ if p.is_empty() => Ok(Reply::Ack),
        _ => Err(ParseError::BadPayload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, encode_vec, FLAG_RESPONSE};

    #[test]
    fn requests_round_trip_through_framing() {
        let r = Request::KeyDown(0x04);
        let bytes = r.encode(7);
        let f = decode(&bytes).unwrap();
        assert_eq!((f.msg_type, f.seq, f.payload), (msg::KEY_DOWN, 7, &[4u8][..]));
        assert!(f.ack_requested());
        assert!(!decode(&Request::MouseMove { dx: -5, dy: 300 }.encode(1)).unwrap().ack_requested());
        assert_eq!(Request::MouseMove { dx: -5, dy: 300 }.payload(), vec![0xFB, 0xFF, 0x2C, 0x01]);
        assert_eq!(Request::SetName("desk".into()).payload(), vec![4, b'd', b'e', b's', b'k']);
    }

    #[test]
    fn parses_replies() {
        let mut hello = vec![1, 0, 7, 0, 0, 0, 0, 1, 0];
        hello.extend_from_slice(&[0xAA; 16]);
        hello.push(4);
        hello.extend_from_slice(b"desk");
        let bytes = encode_vec(msg::HELLO, FLAG_RESPONSE, 0, &hello).unwrap();
        match parse_reply(&decode(&bytes).unwrap()).unwrap() {
            Reply::Hello(h) => assert_eq!((h.major, h.caps, h.fw, h.name.as_str()), (1, 7, [0, 1, 0], "desk")),
            r => panic!("{r:?}"),
        }
        let err = encode_vec(msg::ERROR, FLAG_RESPONSE, 3, &[6, 0x10]).unwrap();
        assert_eq!(parse_reply(&decode(&err).unwrap()), Ok(Reply::Error { code: 6, orig_type: 0x10 }));
        // STATUS: the original 11 bytes (older firmware) and the 12-byte form with the boot drive
        let mut st = vec![1, 2, 1, 7, 0, 0, 0, 3, 0, 0, 0];
        let old = encode_vec(msg::STATUS, FLAG_RESPONSE, 4, &st).unwrap();
        match parse_reply(&decode(&old).unwrap()).unwrap() {
            Reply::Status(s) => assert_eq!((s.hid_mounted, s.dropped_motion, s.bad_crc, s.boot_drive), (true, 7, 3, None)),
            r => panic!("{r:?}"),
        }
        st.push(1);
        let new = encode_vec(msg::STATUS, FLAG_RESPONSE, 5, &st).unwrap();
        assert!(matches!(parse_reply(&decode(&new).unwrap()), Ok(Reply::Status(s)) if s.boot_drive == Some(true)));
        assert_eq!(Request::SetBootDrive(true).payload(), vec![1]);
        assert_eq!(Request::SetBootDrive(false).payload(), vec![0]);
        assert_eq!(Request::SetBootDrive(true).msg_type(), msg::SET_BOOT_DRIVE);
        assert!(Request::SetBootDrive(true).wants_ack());
        let ack = encode_vec(msg::KEY_UP, FLAG_RESPONSE, 3, &[]).unwrap();
        assert_eq!(parse_reply(&decode(&ack).unwrap()), Ok(Reply::Ack));
        let req = encode_vec(msg::KEY_UP, 0, 3, &[4]).unwrap();
        assert_eq!(parse_reply(&decode(&req).unwrap()), Err(ParseError::NotAResponse));
        let short = encode_vec(msg::HELLO, FLAG_RESPONSE, 0, &[1, 0]).unwrap();
        assert_eq!(parse_reply(&decode(&short).unwrap()), Err(ParseError::BadPayload));
    }
}
