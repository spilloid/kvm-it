#include "proto_dispatch.h"
#include <string.h>

void proto_dev_init(proto_dev_t *d, const proto_ops_t *ops, const uint8_t uuid[16], const uint8_t fw[3],
                    const char *name)
{
    memset(d, 0, sizeof *d);
    d->ops = *ops;
    memcpy(d->uuid, uuid, 16);
    memcpy(d->fw, fw, 3);
    strncpy(d->name, name, PROTO_NAME_MAX);
}

void proto_dev_session_start(proto_dev_t *d, uint32_t now_ms)
{
    d->handshaken = false;
    d->active = true;
    d->last_rx_ms = now_ms;
    memset(d->dedup, 0, sizeof d->dedup);
    d->dedup_next = 0;
    d->rx_count = 0;
}

bool proto_dev_active(const proto_dev_t *d) { return d->active; }

void proto_dev_session_end(proto_dev_t *d)
{
    d->handshaken = false;
    d->active = false;
    d->ops.release_all(d->ops.ctx);  // stuck-key defence (2): link drop releases everything
}

bool proto_dev_tick(proto_dev_t *d, uint32_t now_ms)
{
    if (d->handshaken && d->ops.any_held(d->ops.ctx) && (uint32_t)(now_ms - d->last_rx_ms) > PROTO_KEEPALIVE_MS) {
        d->ops.release_all(d->ops.ctx);  // stuck-key defence (3): keepalive timeout
        return true;
    }
    return false;
}

static size_t emit(uint8_t type, uint8_t flags, uint8_t seq, const uint8_t *p, size_t n, uint8_t *resp,
                   size_t cap)
{
    size_t w = 0;
    return proto_encode(type, flags, seq, p, n, resp, cap, &w) == PROTO_OK ? w : 0;
}

static size_t emit_error(uint8_t seq, uint8_t code, uint8_t orig, uint8_t *resp, size_t cap)
{
    uint8_t p[2] = {code, orig};
    return emit(0x7F, PROTO_FLAG_RESPONSE, seq, p, 2, resp, cap);
}

static uint8_t err_from(hidop_t r)
{
    switch (r) {
    case HIDOP_NOT_MOUNTED: return PROTO_ERRC_HID_NOT_MOUNTED;
    case HIDOP_BUSY: return PROTO_ERRC_BUSY;
    case HIDOP_REFUSED: return PROTO_ERRC_REFUSED;
    default: return PROTO_ERRC_BAD_PAYLOAD;
    }
}

static bool valid_name(const uint8_t *p, size_t n)
{
    for (size_t i = 0; i < n; i++)
        if (p[i] < 0x20 || p[i] == 0x7F) return false;  // printable ASCII/UTF-8 continuation only
    return true;
}

static void dedup_store(proto_dev_t *d, uint32_t now, uint8_t seq, uint8_t type, const uint8_t *frame, size_t n)
{
    if (n == 0 || n > PROTO_DEDUP_MAX_FRAME) return;
    proto_dedup_t *e = &d->dedup[d->dedup_next++ % PROTO_DEDUP_SLOTS];
    d->dedup_next %= PROTO_DEDUP_SLOTS;
    e->valid = true; e->seq = seq; e->type = type; e->len = (uint8_t)n;
    e->at_ms = now; e->at_count = d->rx_count;
    memcpy(e->frame, frame, n);
}

/* A cached answer may only be replayed for a genuine retry: recent in time AND in frame count, so a sequence
 * number that wrapped (256 frames later) is treated as a new request, never as a duplicate. */
static const proto_dedup_t *dedup_find(const proto_dev_t *d, uint32_t now, uint8_t seq, uint8_t type)
{
    for (int i = 0; i < PROTO_DEDUP_SLOTS; i++) {
        const proto_dedup_t *e = &d->dedup[i];
        if (e->valid && e->seq == seq && e->type == type && (uint32_t)(now - e->at_ms) < PROTO_DEDUP_AGE_MS &&
            (uint32_t)(d->rx_count - e->at_count) < PROTO_DEDUP_FRAMES)
            return e;
    }
    return NULL;
}

static int16_t rd16(const uint8_t *p) { return (int16_t)(uint16_t)(p[0] | (p[1] << 8)); }

// Returns 0 on success, otherwise a PROTO_ERRC_* / PROTO_ERRC_UNSUPPORTED code.
static uint8_t apply(proto_dev_t *d, const proto_frame_t *f)
{
    const proto_ops_t *o = &d->ops;
    hidop_t r = HIDOP_OK;
    switch (f->type) {
    case 0x10: case 0x11: case 0x12: {
        if (f->len != 1) return PROTO_ERRC_BAD_PAYLOAD;
        uint8_t u = f->payload[0];
        if (u <= 3) return PROTO_ERRC_BAD_PAYLOAD;
        if (f->type == 0x10) r = o->key_down(o->ctx, u);
        else if (f->type == 0x11) r = o->key_up(o->ctx, u);
        else {
            r = o->key_down(o->ctx, u);
            if (r == HIDOP_OK) {
                r = o->key_up(o->ctx, u);
                if (r != HIDOP_OK) o->release_all(o->ctx);  // never leave a half-tapped key held
            }
        }
        break;
    }
    case 0x20: {
        if (f->len != 4) return PROTO_ERRC_BAD_PAYLOAD;
        int dx = rd16(f->payload), dy = rd16(f->payload + 2);
        while ((dx || dy) && r == HIDOP_OK) {
            int sx = dx > 127 ? 127 : dx < -127 ? -127 : dx;
            int sy = dy > 127 ? 127 : dy < -127 ? -127 : dy;
            r = o->mouse_move(o->ctx, (int8_t)sx, (int8_t)sy);
            dx -= sx; dy -= sy;
        }
        if (r != HIDOP_OK) d->dropped_motion++;
        break;
    }
    case 0x21: case 0x22: {
        if (f->len != 1 || f->payload[0] == 0 || (f->payload[0] & ~0x07)) return PROTO_ERRC_BAD_PAYLOAD;
        r = o->button(o->ctx, f->payload[0], f->type == 0x21);
        break;
    }
    case 0x23:
        if (f->len != 2) return PROTO_ERRC_BAD_PAYLOAD;
        r = o->wheel(o->ctx, (int8_t)f->payload[0], (int8_t)f->payload[1]);
        break;
    case 0x30:
        if (f->len != 0) return PROTO_ERRC_BAD_PAYLOAD;
        r = o->release_all(o->ctx);
        break;
    case 0x60: {
        if (f->len < 2 || f->payload[0] < 1 || f->payload[0] > PROTO_NAME_MAX || f->len != 1u + f->payload[0] ||
            !valid_name(f->payload + 1, f->payload[0]))
            return PROTO_ERRC_BAD_PAYLOAD;
        if (!o->set_name(o->ctx, (const char *)f->payload + 1, f->payload[0])) return PROTO_ERRC_BUSY;
        memcpy(d->name, f->payload + 1, f->payload[0]);
        d->name[f->payload[0]] = 0;
        return 0;
    }
    case 0x61:  // SET_BOOT_DRIVE: the adapter's read-only boot drive, off unless asked for
        if (f->len != 1 || f->payload[0] > 1) return PROTO_ERRC_BAD_PAYLOAD;
        if (!o->set_boot_drive(o->ctx, f->payload[0] == 1)) return PROTO_ERRC_BUSY;
        return 0;
    default:
        return PROTO_ERRC_UNSUPPORTED;
    }
    if (r != HIDOP_OK) return err_from(r);
    d->activity++;
    return 0;
}

void proto_dev_handle(proto_dev_t *d, const uint8_t *buf, size_t len, uint32_t now_ms, uint8_t *resp,
                      size_t resp_cap, size_t *resp_len)
{
    *resp_len = 0;
    proto_frame_t f;
    proto_err_t e = proto_decode(buf, len, &f);
    if (e == PROTO_ERR_BAD_CRC) { d->bad_crc++; return; }
    if (e == PROTO_ERR_BAD_VERSION || e == PROTO_ERR_BAD_FLAGS || e == PROTO_ERR_UNKNOWN_TYPE) {
        // CRC was valid, so seq/type are trustworthy enough to answer.
        uint8_t code = e == PROTO_ERR_BAD_VERSION ? PROTO_ERRC_BAD_VERSION
                     : e == PROTO_ERR_BAD_FLAGS   ? PROTO_ERRC_BAD_FLAGS : PROTO_ERRC_UNSUPPORTED;
        *resp_len = emit_error(buf[3], code, buf[1], resp, resp_cap);
        return;
    }
    if (e != PROTO_OK) return;  // truncated / length problems: nothing trustworthy to answer
    d->last_rx_ms = now_ms;
    d->rx_count++;

    // Responses and device→controller types are never valid inbound.
    if ((f.flags & PROTO_FLAG_RESPONSE) || f.type == 0x7F) {
        *resp_len = emit_error(f.seq, PROTO_ERRC_UNSUPPORTED, f.type, resp, resp_cap);
        return;
    }

    switch (f.type) {
    case 0x01: {  // HELLO
        if (f.len != 2) { *resp_len = emit_error(f.seq, PROTO_ERRC_BAD_PAYLOAD, f.type, resp, resp_cap); return; }
        if (f.payload[0] != PROTO_VERSION) { *resp_len = emit_error(f.seq, PROTO_ERRC_BAD_VERSION, f.type, resp, resp_cap); return; }
        proto_dev_session_start(d, now_ms);  // fresh dedup window; release is the controller's job (RELEASE_ALL)
        d->handshaken = true;
        uint8_t p[2 + 4 + 3 + 16 + 1 + PROTO_NAME_MAX];
        size_t fixed = 2 + 4 + 3 + 16 + 1;
        if (resp_cap < PROTO_OVERHEAD + fixed) {  // ATT MTU below 37: the handshake cannot be answered
            *resp_len = emit_error(f.seq, PROTO_ERRC_MTU_TOO_SMALL, f.type, resp, resp_cap);
            return;
        }
        size_t budget = resp_cap - PROTO_OVERHEAD - fixed;
        size_t nl = strlen(d->name);
        if (nl > budget) nl = budget;  // MTU-limited: truncate rather than fail the handshake
        p[0] = PROTO_VERSION; p[1] = 1;  /* minor 1: SET_BOOT_DRIVE, STATUS.boot_drive */
        uint32_t caps = PROTO_CAP_KEYBOARD | PROTO_CAP_MOUSE | PROTO_CAP_SCROLL | PROTO_CAP_KEEPALIVE | PROTO_CAP_BOOT_DRIVE;
        p[2] = (uint8_t)caps; p[3] = (uint8_t)(caps >> 8); p[4] = (uint8_t)(caps >> 16); p[5] = (uint8_t)(caps >> 24);
        memcpy(p + 6, d->fw, 3);
        memcpy(p + 9, d->uuid, 16);
        p[25] = (uint8_t)nl;
        memcpy(p + 26, d->name, nl);
        *resp_len = emit(0x01, PROTO_FLAG_RESPONSE, f.seq, p, fixed + nl, resp, resp_cap);
        return;
    }
    case 0x40:  // PING: echo
        if (f.len > 8) { *resp_len = emit_error(f.seq, PROTO_ERRC_BAD_PAYLOAD, f.type, resp, resp_cap); return; }
        *resp_len = emit(0x40, PROTO_FLAG_RESPONSE, f.seq, f.payload, f.len, resp, resp_cap);
        return;
    case 0x50: {  // STATUS
        if (f.len != 0) { *resp_len = emit_error(f.seq, PROTO_ERRC_BAD_PAYLOAD, f.type, resp, resp_cap); return; }
        uint8_t keys = 0, buttons = 0, p[12];
        d->ops.counts(d->ops.ctx, &keys, &buttons);
        p[0] = d->ops.mounted(d->ops.ctx); p[1] = keys; p[2] = buttons;
        for (int i = 0; i < 4; i++) { p[3 + i] = (uint8_t)(d->dropped_motion >> (8 * i)); p[7 + i] = (uint8_t)(d->bad_crc >> (8 * i)); }
        p[11] = d->ops.boot_drive(d->ops.ctx) ? 1 : 0;  // appended in minor 1; older controllers ignore the extra byte
        *resp_len = emit(0x50, PROTO_FLAG_RESPONSE, f.seq, p, sizeof p, resp, resp_cap);
        return;
    }
    default: break;
    }

    if (!d->handshaken) { *resp_len = emit_error(f.seq, PROTO_ERRC_NOT_READY, f.type, resp, resp_cap); return; }

    if (f.type == 0x20 && (f.flags & PROTO_FLAG_ACK_REQ)) {  // motion is never acknowledged
        *resp_len = emit_error(f.seq, PROTO_ERRC_BAD_FLAGS, f.type, resp, resp_cap);
        return;
    }
    if (f.flags & PROTO_FLAG_ACK_REQ) {
        const proto_dedup_t *dup = dedup_find(d, now_ms, f.seq, f.type);
        if (dup && dup->len <= resp_cap) {  // retry: replay the earlier answer, apply nothing
            memcpy(resp, dup->frame, dup->len);
            *resp_len = dup->len;
            return;
        }
    }
    uint8_t code = apply(d, &f);
    if (code) { *resp_len = emit_error(f.seq, code, f.type, resp, resp_cap); return; }
    if (f.flags & PROTO_FLAG_ACK_REQ) {
        *resp_len = emit(f.type, PROTO_FLAG_RESPONSE, f.seq, NULL, 0, resp, resp_cap);
        dedup_store(d, now_ms, f.seq, f.type, resp, *resp_len);
    }
}
