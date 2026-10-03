// Runs the shared golden vectors (protocol/vectors.txt) through the C codec.
// Usage: test_proto_frame <path-to-vectors.txt>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../../main/proto_frame.h"

static size_t unhex(const char *s, uint8_t *out, size_t cap)
{
    if (strcmp(s, "-") == 0) return 0;
    size_t n = strlen(s) / 2;
    if (n > cap) { fprintf(stderr, "vector too long\n"); exit(2); }
    for (size_t i = 0; i < n; i++) { unsigned v; sscanf(s + 2 * i, "%2x", &v); out[i] = (uint8_t)v; }
    return n;
}

int main(int argc, char **argv)
{
    if (argc < 2) { fprintf(stderr, "usage: %s vectors.txt\n", argv[0]); return 2; }
    FILE *f = fopen(argv[1], "r");
    if (!f) { perror("vectors"); return 2; }
    if (proto_crc16((const uint8_t *)"123456789", 9) != 0x29B1) { puts("FAIL crc check value"); return 1; }

    char line[1024];
    int valid = 0, invalid = 0, fail = 0;
    while (fgets(line, sizeof line, f)) {
        if (line[0] == '#' || line[0] == '\n') continue;
        uint8_t raw[300], want_payload[300], re[300];
        char kind, name[64], hex[700], pay[700], errname[32];
        unsigned type, flags, seq;
        if (line[0] == 'V' && sscanf(line, "%c %63s %699s %u %u %u %699s", &kind, name, hex, &type, &flags, &seq, pay) == 7) {
            size_t n = unhex(hex, raw, sizeof raw), pn = unhex(pay, want_payload, sizeof want_payload);
            proto_frame_t fr;
            proto_err_t e = proto_decode(raw, n, &fr);
            if (e != PROTO_OK || fr.type != type || fr.flags != flags || fr.seq != seq || fr.len != pn ||
                memcmp(fr.payload, want_payload, pn) != 0) {
                printf("FAIL valid %s (%s)\n", name, proto_err_name(e)); fail++; continue;
            }
            size_t w = 0;
            if (proto_encode((uint8_t)type, (uint8_t)flags, (uint8_t)seq, want_payload, pn, re, sizeof re, &w) != PROTO_OK ||
                w != n || memcmp(re, raw, n) != 0) {
                printf("FAIL re-encode %s\n", name); fail++; continue;
            }
            valid++;
        } else if (line[0] == 'I' && sscanf(line, "%c %63s %699s %31s", &kind, name, hex, errname) == 4) {
            size_t n = unhex(hex, raw, sizeof raw);
            proto_frame_t fr;
            proto_err_t e = proto_decode(raw, n, &fr);
            if (strcmp(proto_err_name(e), errname) != 0) {
                printf("FAIL invalid %s: got %s want %s\n", name, proto_err_name(e), errname); fail++; continue;
            }
            invalid++;
        } else {
            printf("FAIL unparsable line: %s", line); fail++;
        }
    }
    fclose(f);
    printf("proto_frame: %d valid, %d invalid vectors passed, %d failed\n", valid, invalid, fail);
    return (fail || valid == 0 || invalid == 0) ? 1 : 0;
}
