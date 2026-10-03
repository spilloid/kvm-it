# kvm-it wire protocol v1.0

Normative spec. Frames travel over BLE GATT: controller→device by write-without-response (and write with
response for ack-requested frames if the stack prefers), device→controller by notify. One frame per
GATT write/notification; a frame never spans writes. Golden vectors: [vectors.json](vectors.json), generated
by [gen_vectors.py](gen_vectors.py) (independent of both implementations) and consumed by the C firmware
tests and the Rust `kvmit-protocol` tests.

## Frame

All multi-byte integers are little-endian.

| Offset | Size | Field | Notes |
|---|---|---|---|
| 0 | 1 | `ver` | major version; this spec = `1` |
| 1 | 1 | `type` | message type |
| 2 | 1 | `flags` | bit0 `ACK_REQ`, bit1 `RESPONSE`; bits 2-7 reserved, must be 0 |
| 3 | 1 | `seq` | per-direction counter, wraps at 256 |
| 4 | 2 | `len` | payload length, 0..=`MAX_PAYLOAD` (240) |
| 6 | `len` | `payload` | per type, below |
| 6+len | 2 | `crc` | CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no reflection, xorout 0) over bytes `0..6+len` |

Total frame = `len + 8` bytes. Senders must not exceed the negotiated `ATT_MTU - 3`. Every v1 message
except a long-named `HELLO` response and `SET_NAME` fits in 20 bytes (the BLE default MTU of 23 minus 3).
CRC check value: `crc16("123456789") = 0x29B1`.

## Decoding rules (receiver)

Checked in this order; the first failure decides the outcome.

1. Fewer than 8 bytes, or `len` inconsistent with the buffer length → `Truncated` / `LengthMismatch`. Dropped.
2. `len > MAX_PAYLOAD` → `PayloadTooLong`. Dropped.
3. CRC mismatch → `BadCrc`. **Dropped silently and counted** (`bad_crc` in STATUS); no response is sent
   because `seq`/`type` cannot be trusted.
4. `ver` major unknown → respond `ERROR(BAD_VERSION)` (only decodable ahead of other checks for HELLO).
5. Reserved flag bits set → `ERROR(BAD_FLAGS)`.
6. Unknown `type` → `ERROR(UNSUPPORTED)`. Never ignored silently.
7. Payload shorter/longer than the type requires → `ERROR(BAD_PAYLOAD)`. Appended fields from a later
   minor version are permitted only where a type says "may be extended"; otherwise length must be exact.

## Messages

`dir`: C = controller→device, D = device→controller. A frame with `RESPONSE` set answers the C frame with
the same `type` and `seq`.

| Type | Name | Dir | Request payload | Response payload |
|---|---|---|---|---|
| 0x01 | `HELLO` | C | `major u8, minor u8` | `major u8, minor u8, caps u32, fw[3] u8 (maj,min,patch), uuid[16], name_len u8, name (utf-8, ≤32)` |
| 0x10 | `KEY_DOWN` | C | `usage u8` | empty (if `ACK_REQ`) |
| 0x11 | `KEY_UP` | C | `usage u8` | empty (if `ACK_REQ`) |
| 0x12 | `KEY_TAP` | C | `usage u8` | empty (if `ACK_REQ`) |
| 0x20 | `MOUSE_MOVE` | C | `dx i16, dy i16` | none (never `ACK_REQ`) |
| 0x21 | `MOUSE_BUTTON_DOWN` | C | `mask u8` | empty (if `ACK_REQ`) |
| 0x22 | `MOUSE_BUTTON_UP` | C | `mask u8` | empty (if `ACK_REQ`) |
| 0x23 | `SCROLL` | C | `v i8, h i8` | empty (if `ACK_REQ`) |
| 0x30 | `RELEASE_ALL` | C | empty | empty (if `ACK_REQ`) |
| 0x40 | `PING` | C | 0..=8 opaque bytes | same bytes echoed |
| 0x50 | `STATUS` | C | empty | `hid_mounted u8, keys u8, buttons u8, dropped_motion u32, bad_crc u32` (may be extended) |
| 0x60 | `SET_NAME` | C | `name_len u8, name (utf-8, 1..=32)` | empty (if `ACK_REQ`) |
| 0x7F | `ERROR` | D | — | `code u8, orig_type u8` |

- `usage` is a USB HID Keyboard/Keypad page (0x07) usage ID. Modifiers are usages `0xE0..=0xE7`. `0x00..=0x03`
  (reserved/error roll-over) are rejected with `BAD_PAYLOAD`.
- `mask` is the USB HID mouse button bitmask (bit0 left, bit1 right, bit2 middle); bits 3-7 must be 0.
- `KEY_TAP` is down then up by the device with no controller-visible gap; for held timing use DOWN/UP.
- `caps` bits (HELLO_ACK): bit0 keyboard, bit1 mouse, bit2 scroll, bit3 key-keepalive (reserved for M4).
- Only `HELLO`, `PING` and `STATUS` are accepted before a successful handshake; other C frames answer
  `ERROR(NOT_READY)`. Handshake state is per BLE connection.
- `ERROR` codes: `1 UNSUPPORTED`, `2 BAD_VERSION`, `3 BAD_FLAGS`, `4 BAD_PAYLOAD`, `5 NOT_READY`,
  `6 HID_NOT_MOUNTED` (target not enumerated us), `7 BUSY`, `8 REFUSED` (e.g. 7th simultaneous key).
  `ERROR` carries the `seq` of the offending frame.

## Reliability

- `ACK_REQ` frames are retried by the controller with the **same `seq`**; the device keeps the last
  response per `seq` window and replays it for duplicates without re-applying the action. HID state is also
  idempotent, so a missed dedup cannot double-press.
- `MOUSE_MOVE` is unacknowledged; the controller accumulates deltas so loss is lag, not a missed click.
- Stuck-key defences (RELEASE_ALL on controller events, device release on link drop, keepalive timeout)
  are unchanged from [../docs/protocol.md](../docs/protocol.md); items marked M4 there are not implemented.

## Evolution

New types and appended payload fields keep major `1`. A new major breaks framing assumptions and is refused
at HELLO. Minor versions are advertised in HELLO and `caps`.
