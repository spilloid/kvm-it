/* Host-side unit tests for the pure-C firmware logic. Run: scripts/fw.sh test */
#include <assert.h>
#include <stdio.h>
#include <string.h>

#include "../../main/ascii_us.h"
#include "../../main/hid_state.h"

static void test_duplicates_are_noops(void)
{
    hid_state_t s; hid_state_init(&s);
    assert(hid_state_key_down(&s, 0x04) == HID_STATE_CHANGED);
    assert(hid_state_key_down(&s, 0x04) == HID_STATE_UNCHANGED);
    assert(hid_state_key_up(&s, 0x04) == HID_STATE_CHANGED);
    assert(hid_state_key_up(&s, 0x04) == HID_STATE_UNCHANGED);
    assert(!hid_state_any_held(&s));
}

static void test_modifiers(void)
{
    hid_state_t s; hid_state_init(&s);
    assert(hid_state_key_down(&s, 0xE0) == HID_STATE_CHANGED); /* LCtrl */
    assert(hid_state_key_down(&s, 0xE2) == HID_STATE_CHANGED); /* LAlt */
    assert(hid_state_key_down(&s, 0xE7) == HID_STATE_CHANGED); /* RGUI */
    assert(s.modifiers == (0x01 | 0x04 | 0x80));
    assert(hid_state_key_up(&s, 0xE2) == HID_STATE_CHANGED);
    assert(s.modifiers == (0x01 | 0x80));
}

static void test_rollover_refuses_seventh_key(void)
{
    hid_state_t s; hid_state_init(&s);
    for (uint8_t u = 0x04; u < 0x0A; u++) {
        assert(hid_state_key_down(&s, u) == HID_STATE_CHANGED);
    }
    hid_state_t before = s;
    assert(hid_state_key_down(&s, 0x0A) == HID_STATE_ROLLOVER);
    assert(memcmp(&before, &s, sizeof s) == 0);
    /* Modifiers do not consume key slots. */
    assert(hid_state_key_down(&s, 0xE1) == HID_STATE_CHANGED);
    /* Freeing a slot lets the refused key in. */
    assert(hid_state_key_up(&s, 0x04) == HID_STATE_CHANGED);
    assert(hid_state_key_down(&s, 0x0A) == HID_STATE_CHANGED);
}

static void test_invalid_usages(void)
{
    hid_state_t s; hid_state_init(&s);
    assert(hid_state_key_down(&s, 0x00) == HID_STATE_INVALID);
    assert(hid_state_key_down(&s, 0x03) == HID_STATE_INVALID);
    assert(hid_state_key_down(&s, 0xDE) == HID_STATE_INVALID);
    assert(hid_state_key_down(&s, 0xE8) == HID_STATE_INVALID);
    assert(hid_state_button_down(&s, 0) == HID_STATE_INVALID);
    assert(hid_state_button_down(&s, 0x08) == HID_STATE_INVALID);
    assert(!hid_state_any_held(&s));
}

static void test_buttons(void)
{
    hid_state_t s; hid_state_init(&s);
    assert(hid_state_button_down(&s, 0x01) == HID_STATE_CHANGED);
    assert(hid_state_button_down(&s, 0x01) == HID_STATE_UNCHANGED);
    assert(hid_state_button_down(&s, 0x04) == HID_STATE_CHANGED);
    assert(s.buttons == 0x05);
    assert(hid_state_button_up(&s, 0x01) == HID_STATE_CHANGED);
    assert(hid_state_button_up(&s, 0x01) == HID_STATE_UNCHANGED);
}

static void test_release_all(void)
{
    hid_state_t s; hid_state_init(&s);
    assert(!hid_state_release_all(&s));
    hid_state_key_down(&s, 0xE0);
    hid_state_key_down(&s, 0x06);
    hid_state_button_down(&s, 0x02);
    assert(hid_state_release_all(&s));
    assert(!hid_state_any_held(&s));
    assert(!hid_state_release_all(&s));
}

static void test_ascii_hello_from_kvm(void)
{
    /* Known vector: usage codes and shift flags for the self-test string. */
    static const struct { uint8_t usage; int shift; } expect[] = {
        {0x0B,1},{0x08,1},{0x0F,1},{0x0F,1},{0x12,1}, {0x2C,0},
        {0x09,1},{0x15,1},{0x12,1},{0x10,1}, {0x2C,0},
        {0x0E,1},{0x19,1},{0x10,1},
    };
    const char *text = "HELLO FROM KVM";
    for (size_t i = 0; text[i]; i++) {
        ascii_us_key_t k;
        assert(ascii_us_lookup(text[i], &k));
        assert(k.usage == expect[i].usage);
        assert(k.shift == expect[i].shift);
    }
    assert(strlen(text) == sizeof expect / sizeof expect[0]);
}

static void test_ascii_punctuation(void)
{
    struct { char c; uint8_t usage; int shift; } v[] = {
        {'1',0x1E,0},{'0',0x27,0},{'!',0x1E,1},{'@',0x1F,1},{')',0x27,1},
        {'-',0x2D,0},{'_',0x2D,1},{'=',0x2E,0},{'+',0x2E,1},{'[',0x2F,0},
        {'{',0x2F,1},{'\\',0x31,0},{'|',0x31,1},{';',0x33,0},{':',0x33,1},
        {'\'',0x34,0},{'"',0x34,1},{'`',0x35,0},{'~',0x35,1},{',',0x36,0},
        {'<',0x36,1},{'.',0x37,0},{'>',0x37,1},{'/',0x38,0},{'?',0x38,1},
        {'^',0x23,1},{'&',0x24,1},{'*',0x25,1},{'(',0x26,1},{'#',0x20,1},
        {'$',0x21,1},{'%',0x22,1},{'a',0x04,0},{'z',0x1D,0},{'\n',0x28,0},
    };
    for (size_t i = 0; i < sizeof v / sizeof v[0]; i++) {
        ascii_us_key_t k;
        assert(ascii_us_lookup(v[i].c, &k));
        assert(k.usage == v[i].usage);
        assert(k.shift == v[i].shift);
    }
    ascii_us_key_t k;
    assert(!ascii_us_lookup('\x01', &k));
    assert(!ascii_us_lookup((char)0xE9, &k));
}

int main(void)
{
    test_duplicates_are_noops();
    test_modifiers();
    test_rollover_refuses_seventh_key();
    test_invalid_usages();
    test_buttons();
    test_release_all();
    test_ascii_hello_from_kvm();
    test_ascii_punctuation();
    puts("firmware host tests: all passed");
    return 0;
}
