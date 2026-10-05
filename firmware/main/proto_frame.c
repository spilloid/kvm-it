#include "proto_frame.h"

static int known_type(uint8_t t)
{
    switch (t) {
    case 0x01: case 0x10: case 0x11: case 0x12: case 0x20: case 0x21: case 0x22: case 0x23:
    case 0x30: case 0x40: case 0x50: case 0x60: case 0x61: case 0x7F:
        return 1;
    default:
        return 0;
    }
}

uint16_t proto_crc16(const uint8_t *data, size_t len)
{
    uint16_t crc = 0xFFFF;
    for (size_t i = 0; i < len; i++) {
        crc ^= (uint16_t)(data[i] << 8);
        for (int b = 0; b < 8; b++) {
            crc = (crc & 0x8000) ? (uint16_t)((crc << 1) ^ 0x1021) : (uint16_t)(crc << 1);
        }
    }
    return crc;
}

proto_err_t proto_decode(const uint8_t *buf, size_t len, proto_frame_t *out)
{
    if (len < PROTO_OVERHEAD) return PROTO_ERR_TRUNCATED;
    size_t plen = (size_t)buf[4] | ((size_t)buf[5] << 8);
    if (len != plen + PROTO_OVERHEAD) return PROTO_ERR_LENGTH_MISMATCH;
    if (plen > PROTO_MAX_PAYLOAD) return PROTO_ERR_PAYLOAD_TOO_LONG;
    uint16_t want = (uint16_t)(buf[PROTO_HEADER_LEN + plen] | (buf[PROTO_HEADER_LEN + plen + 1] << 8));
    if (proto_crc16(buf, PROTO_HEADER_LEN + plen) != want) return PROTO_ERR_BAD_CRC;
    if (buf[0] != PROTO_VERSION) return PROTO_ERR_BAD_VERSION;
    if (buf[2] & ~(PROTO_FLAG_ACK_REQ | PROTO_FLAG_RESPONSE)) return PROTO_ERR_BAD_FLAGS;
    if (!known_type(buf[1])) return PROTO_ERR_UNKNOWN_TYPE;
    out->ver = buf[0];
    out->type = buf[1];
    out->flags = buf[2];
    out->seq = buf[3];
    out->payload = buf + PROTO_HEADER_LEN;
    out->len = (uint16_t)plen;
    return PROTO_OK;
}

proto_err_t proto_encode(uint8_t type, uint8_t flags, uint8_t seq, const uint8_t *payload, size_t plen,
                         uint8_t *out, size_t out_cap, size_t *written)
{
    if (plen > PROTO_MAX_PAYLOAD) return PROTO_ERR_PAYLOAD_TOO_LONG;
    if (out_cap < plen + PROTO_OVERHEAD) return PROTO_ERR_BUFFER_TOO_SMALL;
    out[0] = PROTO_VERSION;
    out[1] = type;
    out[2] = flags;
    out[3] = seq;
    out[4] = (uint8_t)(plen & 0xFF);
    out[5] = (uint8_t)(plen >> 8);
    for (size_t i = 0; i < plen; i++) out[PROTO_HEADER_LEN + i] = payload[i];
    uint16_t crc = proto_crc16(out, PROTO_HEADER_LEN + plen);
    out[PROTO_HEADER_LEN + plen] = (uint8_t)(crc & 0xFF);
    out[PROTO_HEADER_LEN + plen + 1] = (uint8_t)(crc >> 8);
    *written = plen + PROTO_OVERHEAD;
    return PROTO_OK;
}

const char *proto_err_name(proto_err_t e)
{
    switch (e) {
    case PROTO_OK: return "Ok";
    case PROTO_ERR_TRUNCATED: return "Truncated";
    case PROTO_ERR_LENGTH_MISMATCH: return "LengthMismatch";
    case PROTO_ERR_PAYLOAD_TOO_LONG: return "PayloadTooLong";
    case PROTO_ERR_BAD_CRC: return "BadCrc";
    case PROTO_ERR_BAD_VERSION: return "BadVersion";
    case PROTO_ERR_BAD_FLAGS: return "BadFlags";
    case PROTO_ERR_UNKNOWN_TYPE: return "UnknownType";
    case PROTO_ERR_BUFFER_TOO_SMALL: return "BufferTooSmall";
    }
    return "?";
}
