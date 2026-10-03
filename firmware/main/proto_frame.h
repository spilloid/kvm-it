// kvm-it wire protocol v1 framing (protocol/SPEC.md). Pure C, no ESP-IDF dependencies; host-tested
// against protocol/vectors.txt.
#pragma once
#include <stddef.h>
#include <stdint.h>

#define PROTO_VERSION 1
#define PROTO_MAX_PAYLOAD 240
#define PROTO_HEADER_LEN 6
#define PROTO_OVERHEAD 8

#define PROTO_FLAG_ACK_REQ 0x01
#define PROTO_FLAG_RESPONSE 0x02

typedef enum {
    PROTO_OK = 0,
    PROTO_ERR_TRUNCATED,
    PROTO_ERR_LENGTH_MISMATCH,
    PROTO_ERR_PAYLOAD_TOO_LONG,
    PROTO_ERR_BAD_CRC,
    PROTO_ERR_BAD_VERSION,
    PROTO_ERR_BAD_FLAGS,
    PROTO_ERR_UNKNOWN_TYPE,
    PROTO_ERR_BUFFER_TOO_SMALL,
} proto_err_t;

typedef struct {
    uint8_t ver, type, flags, seq;
    const uint8_t *payload;  // points into the input buffer
    uint16_t len;
} proto_frame_t;

uint16_t proto_crc16(const uint8_t *data, size_t len);
proto_err_t proto_decode(const uint8_t *buf, size_t len, proto_frame_t *out);
// Returns bytes written via *written.
proto_err_t proto_encode(uint8_t type, uint8_t flags, uint8_t seq, const uint8_t *payload, size_t plen,
                         uint8_t *out, size_t out_cap, size_t *written);
const char *proto_err_name(proto_err_t e);  // matches the error names in vectors.txt
