// Protocol v1 command dispatcher (protocol/SPEC.md). Pure C: HID effects and device info are injected, so
// the whole state machine (handshake gate, dedup, errors, keepalive) is host-tested. Never logs payloads.
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "proto_frame.h"

#define PROTO_NAME_MAX 32
#define PROTO_KEEPALIVE_MS 5000
#define PROTO_DEDUP_SLOTS 16
#define PROTO_DEDUP_AGE_MS 1500   /* just above the controller's 4 x 350 ms retry window */
#define PROTO_DEDUP_FRAMES 128    /* a seq can only repeat legitimately within this many received frames */
#define PROTO_DEDUP_MAX_FRAME 16

enum { PROTO_ERRC_UNSUPPORTED = 1, PROTO_ERRC_BAD_VERSION, PROTO_ERRC_BAD_FLAGS, PROTO_ERRC_BAD_PAYLOAD,
       PROTO_ERRC_NOT_READY, PROTO_ERRC_HID_NOT_MOUNTED, PROTO_ERRC_BUSY, PROTO_ERRC_REFUSED, PROTO_ERRC_MTU_TOO_SMALL };

enum { PROTO_CAP_KEYBOARD = 1, PROTO_CAP_MOUSE = 2, PROTO_CAP_SCROLL = 4, PROTO_CAP_KEEPALIVE = 8 };

// HID operation results.
typedef enum { HIDOP_OK = 0, HIDOP_NOT_MOUNTED, HIDOP_BUSY, HIDOP_REFUSED, HIDOP_INVALID } hidop_t;

typedef struct {
    void *ctx;
    hidop_t (*key_down)(void *ctx, uint8_t usage);
    hidop_t (*key_up)(void *ctx, uint8_t usage);
    hidop_t (*release_all)(void *ctx);
    hidop_t (*mouse_move)(void *ctx, int8_t dx, int8_t dy);
    hidop_t (*button)(void *ctx, uint8_t mask, bool down);
    hidop_t (*wheel)(void *ctx, int8_t v, int8_t h);  // v = wheel, h = pan
    bool (*mounted)(void *ctx);
    bool (*any_held)(void *ctx);
    void (*counts)(void *ctx, uint8_t *keys, uint8_t *buttons);
    bool (*set_name)(void *ctx, const char *name, size_t len);  // persist; false on failure
} proto_ops_t;

typedef struct {
    uint8_t seq;
    uint8_t type;
    uint8_t len;
    bool valid;
    uint32_t at_ms;
    uint32_t at_count;
    uint8_t frame[PROTO_DEDUP_MAX_FRAME];
} proto_dedup_t;

typedef struct {
    proto_ops_t ops;
    uint8_t uuid[16];
    uint8_t fw[3];
    char name[PROTO_NAME_MAX + 1];
    // session
    bool handshaken;
    bool active;  // a session is open (started, not yet ended)
    uint32_t last_rx_ms;
    proto_dedup_t dedup[PROTO_DEDUP_SLOTS];
    uint8_t dedup_next;
    uint32_t rx_count;  // valid frames received this session (ages dedup entries)
    // counters (exposed via STATUS)
    uint32_t dropped_motion;
    uint32_t bad_crc;
    uint32_t activity;  // increments on every accepted input command (for LED activity flash)
} proto_dev_t;

void proto_dev_init(proto_dev_t *d, const proto_ops_t *ops, const uint8_t uuid[16], const uint8_t fw[3],
                    const char *name);
// New BLE connection (or disconnect): clears session, and on disconnect releases everything.
void proto_dev_session_start(proto_dev_t *d, uint32_t now_ms);
void proto_dev_session_end(proto_dev_t *d);
bool proto_dev_active(const proto_dev_t *d);
// Periodic: releases everything if input is held and no valid frame arrived within PROTO_KEEPALIVE_MS.
// Returns true if it released.
bool proto_dev_tick(proto_dev_t *d, uint32_t now_ms);
// Handle one received frame. Writes any response to resp and sets *resp_len (0 = none).
// resp_cap should be the notify budget (ATT_MTU - 3); HELLO's name is truncated to fit.
void proto_dev_handle(proto_dev_t *d, const uint8_t *buf, size_t len, uint32_t now_ms, uint8_t *resp,
                      size_t resp_cap, size_t *resp_len);
