# Wire protocol — design rationale

The normative spec is [protocol/SPEC.md](https://github.com/spilloid/kvm-it/blob/main/protocol/SPEC.md) (framing + codecs implemented and host-tested;
BLE transport and message handling are not). Where this draft and SPEC.md differ, SPEC.md wins: PONG is a
`PING` response frame, not a separate type. This file fixes the shape so firmware
and desktop cannot diverge silently: both sides will test against the same `protocol/vectors.json`.

## Frame

```
 0      1      2      3        4..5      6..(6+len-1)   last 2
+------+------+------+--------+---------+--------------+------+
| ver  | type | flags| seq    | len(LE) |   payload    | crc16|
+------+------+------+--------+---------+--------------+------+
```

- `ver`: protocol major version; unknown major ⇒ handshake fails with an error response.
- `type`: message type (below). Unknown types are answered with `ERR_UNSUPPORTED`, never ignored silently.
- `flags`: bit0 = ack requested, bit1 = response. Reserved bits must be zero.
- `seq`: 8-bit, per direction; used for ack/retry and drop counters.
- `len`: payload length, little-endian; frames never span more than the negotiated MTU in v1.
- Trailing CRC16 guards against corruption beyond BLE's link-layer protection.
- Evolution: new message types and appended payload fields under the same major; capability negotiation
  in the handshake states which are supported.

## Message families

HELLO/HELLO_ACK (versions, capabilities, device UUID, name, hardware id, firmware version) · KEY_DOWN ·
KEY_UP · KEY_TAP · MOUSE_MOVE (relative, int16, droppable) · MOUSE_BUTTON_DOWN/UP · SCROLL · RELEASE_ALL ·
PING/PONG · STATUS (HID mounted state, counters) · SET_NAME · ERROR.

## Reliability rules

- Key down/up, button, and RELEASE_ALL: ack requested, retried with the same `seq`; the firmware
  deduplicates by `seq` and `hid_state` is idempotent, so a retry can never double-press.
- Mouse motion: write-without-response, no ack, loss tolerated; motion deltas are accumulated by the
  controller so a lost packet is a small cursor lag, not a missed click.
- **Stuck-key defence in depth:** (1) RELEASE_ALL on capture exit/focus loss/reconnect/app close;
  (2) firmware auto-releases everything when the BLE link drops; (3) a held key must be refreshed by the
  controller (keepalive) or the firmware releases it after a timeout; (4) `hid_state` ignores duplicates.
  Items 2-3 are design commitments for Milestone 4, not implemented.
- Typed text is sent as key events built by the controller, never as a string; payloads are not logged.
