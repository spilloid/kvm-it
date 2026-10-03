// Host tests: protocol dispatcher, LED language, BOOT-button gestures.
#include <stdio.h>
#include <string.h>
#include "../../main/button_logic.h"
#include "../../main/hid_state.h"
#include "../../main/led_pattern.h"
#include "../../main/proto_dispatch.h"

static int fails;
#define CHECK(c) do { if (!(c)) { printf("FAIL %s:%d %s\n", __FILE__, __LINE__, #c); fails++; } } while (0)

// ---- fake HID backed by the real hid_state ----
typedef struct { hid_state_t s; bool fail_up; bool mounted; int moves; int resets; int mx, my; char name[40]; } fake_t;
static hidop_t f_down(void *c, uint8_t u) { fake_t *f = c; if (!f->mounted) return HIDOP_NOT_MOUNTED;
    hid_state_result_t r = hid_state_key_down(&f->s, u); return r == HID_STATE_ROLLOVER ? HIDOP_REFUSED : r == HID_STATE_INVALID ? HIDOP_INVALID : HIDOP_OK; }
static hidop_t f_up(void *c, uint8_t u) { fake_t *f = c; if (!f->mounted) return HIDOP_NOT_MOUNTED; if (f->fail_up) return HIDOP_BUSY; hid_state_key_up(&f->s, u); return HIDOP_OK; }
static hidop_t f_rel(void *c) { fake_t *f = c; f->resets++; hid_state_release_all(&f->s); return HIDOP_OK; }
static hidop_t f_move(void *c, int8_t dx, int8_t dy) { fake_t *f = c; if (!f->mounted) return HIDOP_NOT_MOUNTED; f->moves++; f->mx += dx; f->my += dy; return HIDOP_OK; }
static hidop_t f_btn(void *c, uint8_t m, bool d) { fake_t *f = c; if (!f->mounted) return HIDOP_NOT_MOUNTED; d ? hid_state_button_down(&f->s, m) : hid_state_button_up(&f->s, m); return HIDOP_OK; }
static hidop_t f_wheel(void *c, int8_t v, int8_t h) { (void)v; (void)h; return ((fake_t *)c)->mounted ? HIDOP_OK : HIDOP_NOT_MOUNTED; }
static bool f_mounted(void *c) { return ((fake_t *)c)->mounted; }
static bool f_held(void *c) { return hid_state_any_held(&((fake_t *)c)->s); }
static void f_counts(void *c, uint8_t *k, uint8_t *b) { fake_t *f = c; *b = f->s.buttons; *k = 0; for (int i = 0; i < 6; i++) *k += f->s.keys[i] != 0; }
static bool f_name(void *c, const char *n, size_t l) { fake_t *f = c; memcpy(f->name, n, l); f->name[l] = 0; return true; }

static fake_t fk;
static proto_dev_t dev;
static uint32_t now;

static void setup(void)
{
    memset(&fk, 0, sizeof fk); fk.mounted = true; hid_state_init(&fk.s);
    proto_ops_t ops = {&fk, f_down, f_up, f_rel, f_move, f_btn, f_wheel, f_mounted, f_held, f_counts, f_name};
    uint8_t uuid[16] = {1, 2, 3}, fw[3] = {0, 1, 0};
    proto_dev_init(&dev, &ops, uuid, fw, "kvm-it-test");
    proto_dev_session_start(&dev, 0);
    now = 100;
}

static size_t send(uint8_t type, uint8_t flags, uint8_t seq, const uint8_t *p, size_t n, uint8_t *resp)
{
    uint8_t buf[300]; size_t w = 0, rl = 0;
    proto_encode(type, flags, seq, p, n, buf, sizeof buf, &w);
    proto_dev_handle(&dev, buf, w, now += 10, resp, 247, &rl);
    return rl;
}
static void hello(void) { uint8_t r[300], p[2] = {1, 0}; CHECK(send(0x01, 0, 0, p, 2, r) > 0); }
static int resp_err(const uint8_t *r, size_t n) { proto_frame_t f; if (proto_decode(r, n, &f) != PROTO_OK || f.type != 0x7F) return -1; return f.payload[0]; }

static void test_handshake_gate(void)
{
    setup(); uint8_t r[300], p[1] = {4};
    size_t n = send(0x10, PROTO_FLAG_ACK_REQ, 1, p, 1, r);
    CHECK(resp_err(r, n) == PROTO_ERRC_NOT_READY);
    CHECK(!hid_state_any_held(&fk.s));
    uint8_t pp[2] = {1, 0};
    n = send(0x01, 0, 2, pp, 2, r);
    proto_frame_t f; CHECK(proto_decode(r, n, &f) == PROTO_OK && f.type == 0x01 && (f.flags & PROTO_FLAG_RESPONSE));
    CHECK(f.payload[0] == 1 && f.payload[9] == 1 && f.payload[25] == strlen("kvm-it-test"));
    pp[0] = 9; n = send(0x01, 0, 3, pp, 2, r); CHECK(resp_err(r, n) == PROTO_ERRC_BAD_VERSION);
    n = send(0x40, 0, 4, p, 1, r); CHECK(n > 0 && resp_err(r, n) == -1);  // PING allowed pre-handshake
}

static void test_keys_and_ack_dedup(void)
{
    setup(); hello(); uint8_t r[300], p[1] = {4};
    size_t n = send(0x10, PROTO_FLAG_ACK_REQ, 7, p, 1, r);
    proto_frame_t f; CHECK(proto_decode(r, n, &f) == PROTO_OK && f.type == 0x10 && (f.flags & 2) && f.len == 0);
    CHECK(fk.s.keys[0] == 4);
    // release behind its back, then retry the same seq: must replay, not re-press
    hid_state_release_all(&fk.s);
    size_t n2 = send(0x10, PROTO_FLAG_ACK_REQ, 7, p, 1, r);
    CHECK(n2 == n); CHECK(fk.s.keys[0] == 0);
    p[0] = 0x02; n = send(0x10, PROTO_FLAG_ACK_REQ, 8, p, 1, r); CHECK(resp_err(r, n) == PROTO_ERRC_BAD_PAYLOAD);
    p[0] = 0x28; n = send(0x12, PROTO_FLAG_ACK_REQ, 9, p, 1, r); CHECK(n > 0 && !hid_state_any_held(&fk.s));  // tap
}

static void test_rollover_and_not_mounted(void)
{
    setup(); hello(); uint8_t r[300];
    for (uint8_t u = 4; u < 10; u++) send(0x10, PROTO_FLAG_ACK_REQ, u, &u, 1, r);
    uint8_t u7 = 10; size_t n = send(0x10, PROTO_FLAG_ACK_REQ, 20, &u7, 1, r);
    CHECK(resp_err(r, n) == PROTO_ERRC_REFUSED);
    fk.mounted = false; u7 = 4; n = send(0x11, PROTO_FLAG_ACK_REQ, 21, &u7, 1, r);
    CHECK(resp_err(r, n) == PROTO_ERRC_HID_NOT_MOUNTED);
}

static void test_mouse(void)
{
    setup(); hello(); uint8_t r[300]; uint8_t p[4] = {0x2c, 0x01, 0xfb, 0xff};  // dx=300, dy=-5
    CHECK(send(0x20, 0, 1, p, 4, r) == 0);
    CHECK(fk.mx == 300 && fk.my == -5 && fk.moves == 3);
    { size_t en = send(0x20, PROTO_FLAG_ACK_REQ, 2, p, 4, r); CHECK(resp_err(r, en) == PROTO_ERRC_BAD_FLAGS); }
    fk.mounted = false; send(0x20, 0, 3, p, 4, r); CHECK(dev.dropped_motion == 1);
    fk.mounted = true; uint8_t m = 0x08; size_t n = send(0x21, PROTO_FLAG_ACK_REQ, 4, &m, 1, r);
    CHECK(resp_err(r, n) == PROTO_ERRC_BAD_PAYLOAD);
    m = 0x01; send(0x21, PROTO_FLAG_ACK_REQ, 5, &m, 1, r); CHECK(fk.s.buttons == 1);
    send(0x30, PROTO_FLAG_ACK_REQ, 6, NULL, 0, r); CHECK(fk.s.buttons == 0);
}

static void test_keepalive_and_session_end(void)
{
    setup(); hello(); uint8_t r[300], p[1] = {4}; send(0x10, PROTO_FLAG_ACK_REQ, 1, p, 1, r);
    CHECK(!proto_dev_tick(&dev, now + 4000) && fk.s.keys[0] == 4);
    CHECK(proto_dev_tick(&dev, now + 5100) && !hid_state_any_held(&fk.s));
    send(0x10, PROTO_FLAG_ACK_REQ, 2, p, 1, r);
    int before = fk.resets; proto_dev_session_end(&dev); CHECK(fk.resets == before + 1 && !hid_state_any_held(&fk.s));
    CHECK(!dev.handshaken);
}

static void test_errors_and_status(void)
{
    setup(); hello(); uint8_t r[300], buf[32]; size_t w, rl;
    uint8_t good_crc_bad_ver[16]; proto_encode(0x10, 0, 1, (uint8_t[]){4}, 1, good_crc_bad_ver, 16, &w);
    buf[0] = 0; memcpy(buf, good_crc_bad_ver, w); buf[0] = 2;  // corrupts CRC -> silent drop
    proto_dev_handle(&dev, buf, w, now, r, 247, &rl); CHECK(rl == 0 && dev.bad_crc == 1);
    size_t n = send(0x55, 0, 3, NULL, 0, r); CHECK(resp_err(r, n) == PROTO_ERRC_UNSUPPORTED);  // unknown type is answered, never ignored
    n = send(0x50, 0, 4, NULL, 0, r); proto_frame_t f;
    CHECK(proto_decode(r, n, &f) == PROTO_OK && f.type == 0x50 && f.payload[0] == 1 && f.payload[7] == 1);
    uint8_t nm[6] = {4, 'd', 'e', 's', 'k'}; n = send(0x60, PROTO_FLAG_ACK_REQ, 5, nm, 5, r);
    CHECK(n > 0 && strcmp(fk.name, "desk") == 0);
    nm[0] = 3; n = send(0x60, PROTO_FLAG_ACK_REQ, 6, nm, 5, r); CHECK(resp_err(r, n) == PROTO_ERRC_BAD_PAYLOAD);
    // MTU-limited HELLO truncates the name instead of failing
    uint8_t pp[2] = {1, 0}, b2[40]; proto_encode(0x01, 0, 9, pp, 2, b2, sizeof b2, &w);
    proto_dev_handle(&dev, b2, w, now, r, 8 + 26 + 4, &rl);
    CHECK(proto_decode(r, rl, &f) == PROTO_OK && f.payload[25] == 4);
}

static void test_astra_regressions(void)
{
    uint8_t r[300], a = 4; size_t n;
    // #6 seq wrap must not replay a stale ack: KEY_UP seq=10, KEY_DOWN seq=11, 254 pings, KEY_UP seq=10 again
    setup(); hello();
    send(0x11, PROTO_FLAG_ACK_REQ, 10, &a, 1, r);
    send(0x10, PROTO_FLAG_ACK_REQ, 11, &a, 1, r);
    CHECK(fk.s.keys[0] == 4);
    for (int i = 0; i < 254; i++) send(0x40, 0, (uint8_t)(12 + i), NULL, 0, r);
    n = send(0x11, PROTO_FLAG_ACK_REQ, 10, &a, 1, r);
    CHECK(n > 0 && fk.s.keys[0] == 0);          // applied, not replayed
    // dedup also expires by time: same seq/type long after is a new request
    setup(); hello();
    send(0x10, PROTO_FLAG_ACK_REQ, 5, &a, 1, r); hid_state_release_all(&fk.s);
    now += 2000; send(0x10, PROTO_FLAG_ACK_REQ, 5, &a, 1, r);
    CHECK(fk.s.keys[0] == 4);
    // #7 half-failed tap must not leave the key held
    setup(); hello(); fk.fail_up = true; uint8_t k = 0x28;
    n = send(0x12, PROTO_FLAG_ACK_REQ, 3, &k, 1, r);
    CHECK(resp_err(r, n) == PROTO_ERRC_BUSY && !hid_state_any_held(&fk.s));
    // #10 a retry shortly after 15 other acked commands still replays (16 slots)
    setup(); hello(); k = 0x28; size_t first = send(0x12, PROTO_FLAG_ACK_REQ, 1, &k, 1, r); (void)first;
    int resets_before = fk.resets; (void)resets_before;
    for (uint8_t i = 0; i < 14; i++) send(0x30, PROTO_FLAG_ACK_REQ, (uint8_t)(2 + i), NULL, 0, r);
    int taps_before = fk.moves; (void)taps_before;
    hid_state_release_all(&fk.s);
    send(0x12, PROTO_FLAG_ACK_REQ, 1, &k, 1, r);
    CHECK(!hid_state_any_held(&fk.s));
    // #13 MTU too small for HELLO: explicit error, not silence
    setup(); { uint8_t pp[2] = {1, 0}, b[16]; size_t w, rl; proto_encode(0x01, 0, 1, pp, 2, b, sizeof b, &w);
        proto_dev_handle(&dev, b, w, now, r, 20, &rl); CHECK(resp_err(r, rl) == PROTO_ERRC_MTU_TOO_SMALL); }
    // #9 watchdog runs on time, independent of traffic: tick directly after invalid-frame flood
    setup(); hello(); send(0x10, PROTO_FLAG_ACK_REQ, 1, &a, 1, r);
    for (int i = 0; i < 100; i++) { uint8_t bad[8] = {1, 0x10, 0, 0, 0, 0, 0, 0}; size_t rl; proto_dev_handle(&dev, bad, 8, now + 100u * i, r, 247, &rl); }
    CHECK(proto_dev_tick(&dev, now + 6000) && !hid_state_any_held(&fk.s));
    // session end marks inactive
    CHECK(proto_dev_active(&dev)); proto_dev_session_end(&dev); CHECK(!proto_dev_active(&dev));
}

static void test_led(void)
{
    led_inputs_t in = {.link = LED_LINK_PAIRING, .usb_mounted = true};
    led_rgb_t on = led_pattern(&in, 0), off = led_pattern(&in, 200);
    CHECK(on.b > 0 && on.r == 0 && off.b == 0);                      // pairing = fast blue
    in.link = LED_LINK_IDLE; { led_rgb_t m = led_pattern(&in, 50); CHECK(m.r > 0 && m.b > 0 && m.g == 0); }
    in.link = LED_LINK_CONNECTED; CHECK(led_pattern(&in, 500).g > 0);  // connected = green
    in.last_input_ms = 1000; CHECK(led_pattern(&in, 1030).r > 0);       // input flash
    in.usb_mounted = false; CHECK(led_pattern(&in, 2050).r > 0 && led_pattern(&in, 2050).g > 0);  // amber blip
    in.reset_hold_ms = 10000; led_rgb_t y = led_pattern(&in, 3); CHECK(y.r > 0 && y.g > 0 && y.b == 0);
    in = (led_inputs_t){.link = LED_LINK_CONNECTED, .usb_mounted = true, .reset_flash_until_ms = 5000};
    CHECK(led_pattern(&in, 4000).r > 0 && led_pattern(&in, 4150).r == 0);
    in = (led_inputs_t){.link = LED_LINK_FAULT, .usb_mounted = true}; CHECK(led_pattern(&in, 0).r > 0);
    for (int l = 0; l <= LED_LINK_FAULT; l++) for (uint32_t t = 0; t < 20000; t += 37) {   // never bright
        in = (led_inputs_t){.link = (led_link_t)l, .usb_mounted = t % 3, .last_input_ms = t};
        led_rgb_t c = led_pattern(&in, t); CHECK(c.r <= 40 && c.g <= 40 && c.b <= 40); }
}

static void test_button(void)
{
    btn_t b; btn_init(&b); uint32_t t = 0; btn_event_t e;
    for (t = 0; t < 200; t += 5) btn_update(&b, t >= 50 && t < 60, t);                // 10 ms glitch: ignored
    CHECK(b.stable == false);
    btn_init(&b); btn_event_t got = BTN_EVENT_NONE;
    for (t = 0; t < 1000; t += 5) { e = btn_update(&b, t >= 100 && t < 400, t); if (e) got = e; }
    CHECK(got == BTN_EVENT_SHORT_PRESS);
    btn_init(&b); int resets = 0, shorts = 0;
    for (t = 0; t < 14000; t += 5) { e = btn_update(&b, t >= 100 && t < 12000, t);
        resets += e == BTN_EVENT_TRUST_RESET; shorts += e == BTN_EVENT_SHORT_PRESS; }
    CHECK(resets == 1 && shorts == 0);                                               // long hold: reset once, no short press on release
    btn_init(&b); shorts = 0;
    for (t = 0; t < 8000; t += 5) { e = btn_update(&b, t >= 100 && t < 5000, t); shorts += e == BTN_EVENT_SHORT_PRESS; }
    CHECK(shorts == 0);                                                              // 4.9 s hold: neither
    btn_init(&b); for (t = 0; t < 1000; t += 5) btn_update(&b, true, t); CHECK(btn_held_ms(&b, 1000) >= 900);
}

int main(void)
{
    test_handshake_gate(); test_keys_and_ack_dedup(); test_rollover_and_not_mounted(); test_mouse();
    test_keepalive_and_session_end(); test_errors_and_status(); test_astra_regressions(); test_led(); test_button();
    printf("logic tests: %s (%d failures)\n", fails ? "FAILED" : "all passed", fails);
    return fails != 0;
}
