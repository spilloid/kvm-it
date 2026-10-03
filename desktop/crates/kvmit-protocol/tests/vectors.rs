//! Golden vectors shared with the C firmware tests: protocol/vectors.json.
use kvmit_protocol::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Valid { name: String, hex: String, ver: u8, #[serde(rename = "type")] ty: u8, flags: u8, seq: u8, payload: String }
#[derive(Deserialize)]
struct Invalid { name: String, hex: String, error: String }
#[derive(Deserialize)]
struct File { valid: Vec<Valid>, invalid: Vec<Invalid> }

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn load() -> File {
    let p = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../protocol/vectors.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn valid_vectors_decode_and_reencode_identically() {
    let f = load();
    assert!(!f.valid.is_empty());
    for v in f.valid {
        let raw = unhex(&v.hex);
        let fr = decode(&raw).unwrap_or_else(|e| panic!("{}: {e:?}", v.name));
        assert_eq!((fr.ver, fr.msg_type, fr.flags, fr.seq), (v.ver, v.ty, v.flags, v.seq), "{}", v.name);
        assert_eq!(fr.payload, unhex(&v.payload).as_slice(), "{}", v.name);
        assert_eq!(encode_vec(v.ty, v.flags, v.seq, fr.payload).unwrap(), raw, "{}", v.name);
    }
}

#[test]
fn invalid_vectors_fail_with_the_named_error() {
    let f = load();
    assert!(!f.invalid.is_empty());
    for v in f.invalid {
        let got = format!("{:?}", decode(&unhex(&v.hex)).expect_err(&v.name));
        assert_eq!(got, v.error, "{}", v.name);
    }
}
