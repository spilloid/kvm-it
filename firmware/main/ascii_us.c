#include "ascii_us.h"

#include <stddef.h>

/* Row for 0x20..0x7E: usage and shift flag. 0 usage = unmapped. */
static const struct { uint8_t usage; uint8_t shift; } TABLE[] = {
    /* 0x20 ' ' */ {0x2C, 0}, /* '!' */ {0x1E, 1}, /* '"' */ {0x34, 1}, /* '#' */ {0x20, 1},
    /* '$' */ {0x21, 1}, /* '%' */ {0x22, 1}, /* '&' */ {0x24, 1}, /* '\'' */ {0x34, 0},
    /* '(' */ {0x26, 1}, /* ')' */ {0x27, 1}, /* '*' */ {0x25, 1}, /* '+' */ {0x2E, 1},
    /* ',' */ {0x36, 0}, /* '-' */ {0x2D, 0}, /* '.' */ {0x37, 0}, /* '/' */ {0x38, 0},
    /* '0' */ {0x27, 0}, /* '1' */ {0x1E, 0}, /* '2' */ {0x1F, 0}, /* '3' */ {0x20, 0},
    /* '4' */ {0x21, 0}, /* '5' */ {0x22, 0}, /* '6' */ {0x23, 0}, /* '7' */ {0x24, 0},
    /* '8' */ {0x25, 0}, /* '9' */ {0x26, 0}, /* ':' */ {0x33, 1}, /* ';' */ {0x33, 0},
    /* '<' */ {0x36, 1}, /* '=' */ {0x2E, 0}, /* '>' */ {0x37, 1}, /* '?' */ {0x38, 1},
    /* '@' */ {0x1F, 1},
    /* 'A'..'Z' handled arithmetically below */
};

bool ascii_us_lookup(char c, ascii_us_key_t *out)
{
    unsigned char u = (unsigned char)c;
    if (u >= 'a' && u <= 'z') {
        *out = (ascii_us_key_t){.usage = (uint8_t)(0x04 + (u - 'a')), .shift = false};
        return true;
    }
    if (u >= 'A' && u <= 'Z') {
        *out = (ascii_us_key_t){.usage = (uint8_t)(0x04 + (u - 'A')), .shift = true};
        return true;
    }
    if (u >= 0x20 && u <= '@') {
        size_t i = u - 0x20;
        *out = (ascii_us_key_t){.usage = TABLE[i].usage, .shift = TABLE[i].shift != 0};
        return true;
    }
    switch (u) {
    case '[': *out = (ascii_us_key_t){0x2F, false}; return true;
    case '{': *out = (ascii_us_key_t){0x2F, true};  return true;
    case ']': *out = (ascii_us_key_t){0x30, false}; return true;
    case '}': *out = (ascii_us_key_t){0x30, true};  return true;
    case '\\': *out = (ascii_us_key_t){0x31, false}; return true;
    case '|': *out = (ascii_us_key_t){0x31, true};  return true;
    case '^': *out = (ascii_us_key_t){0x23, true};  return true;
    case '_': *out = (ascii_us_key_t){0x2D, true};  return true;
    case '`': *out = (ascii_us_key_t){0x35, false}; return true;
    case '~': *out = (ascii_us_key_t){0x35, true};  return true;
    case '\n': *out = (ascii_us_key_t){0x28, false}; return true;
    case '\t': *out = (ascii_us_key_t){0x2B, false}; return true;
    default: return false;
    }
}
