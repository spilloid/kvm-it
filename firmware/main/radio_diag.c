#include "radio_diag.h"

#include <string.h>

#include "esp_log.h"
#include "host/ble_hs.h"
#include "host/ble_store.h"
#include "sdkconfig.h"

#if CONFIG_KVMIT_RADIO_DIAG

static const char *TAG = "diag";

#define MAX_PEERS 48
static const uint8_t MARKER[] = "KVMITDIAG";

typedef struct {
    ble_addr_t addr;
    uint32_t n;
    int32_t sum;
    int8_t min, max;
    bool marker;
    char name[20];
} peer_t;

static peer_t g_peers[MAX_PEERS];
static int g_npeers;
static uint32_t g_dropped;
static radio_diag_done_fn g_done;

static const char *addr_str(const ble_addr_t *a, char *buf)
{
    static const char *types[] = {"pub", "rnd", "pub-id", "rnd-id"};
    const uint8_t *v = a->val;
    sprintf(buf, "%02X:%02X:%02X:%02X:%02X:%02X (%s)", v[5], v[4], v[3], v[2], v[1], v[0],
            a->type < 4 ? types[a->type] : "?");
    return buf;
}

static void log_bonds(void)
{
    ble_addr_t peers[MYNEWT_VAL(BLE_STORE_MAX_BONDS)];
    int n = 0;
    int rc = ble_store_util_bonded_peers(peers, &n, MYNEWT_VAL(BLE_STORE_MAX_BONDS));
    if (rc != 0) { ESP_LOGW(TAG, "bonded peers rc=%d", rc); return; }
    ESP_LOGI(TAG, "stored bonds: %d", n);
    char b[32];
    for (int i = 0; i < n; i++) ESP_LOGI(TAG, "  bond %d: %s", i, addr_str(&peers[i], b));
}

static bool has_marker(const uint8_t *d, uint8_t len)
{
    size_t m = sizeof MARKER - 1;
    for (size_t i = 0; i + m <= len; i++) if (memcmp(d + i, MARKER, m) == 0) return true;
    return false;
}

static void record(const struct ble_gap_disc_desc *d)
{
    peer_t *p = NULL;
    for (int i = 0; i < g_npeers; i++)
        if (ble_addr_cmp(&g_peers[i].addr, &d->addr) == 0) { p = &g_peers[i]; break; }
    if (!p) {
        if (g_npeers == MAX_PEERS) { g_dropped++; return; }
        p = &g_peers[g_npeers++];
        memset(p, 0, sizeof *p);
        p->addr = d->addr;
        p->min = 127;
        p->max = -128;
    }
    p->n++;
    p->sum += d->rssi;
    if (d->rssi < p->min) p->min = d->rssi;
    if (d->rssi > p->max) p->max = d->rssi;
    if (has_marker(d->data, d->length_data)) p->marker = true;
    if (!p->name[0]) {
        struct ble_hs_adv_fields f;
        if (ble_hs_adv_parse_fields(&f, d->data, d->length_data) == 0 && f.name_len) {
            size_t l = f.name_len < sizeof p->name - 1 ? f.name_len : sizeof p->name - 1;
            /* Untrusted radio input: printable ASCII only, so a name cannot inject log lines or escapes. */
            for (size_t i = 0; i < l; i++) p->name[i] = (f.name[i] >= 0x20 && f.name[i] < 0x7f) ? f.name[i] : '?';
            p->name[l] = 0;
        }
    }
}

static void report(void)
{
    ESP_LOGI(TAG, "scan done: %d advertisers%s", g_npeers, g_dropped ? " (table full, some dropped)" : "");
    char b[32];
    for (int i = 0; i < g_npeers; i++) {
        const peer_t *p = &g_peers[i];
        ESP_LOGI(TAG, "  %s n=%-4lu rssi min/avg/max %4d/%4ld/%4d %s%s", addr_str(&p->addr, b), (unsigned long)p->n,
                 p->min, (long)(p->sum / (int32_t)p->n), p->max, p->marker ? "<<< KVMITDIAG HOST " : "", p->name);
    }
    bool any = false;
    for (int i = 0; i < g_npeers; i++) any |= g_peers[i].marker;
    if (!any) ESP_LOGW(TAG, "no KVMITDIAG host heard (is the host advertising the marker?)");
}

static int disc_event(struct ble_gap_event *ev, void *arg)
{
    (void)arg;
    switch (ev->type) {
    case BLE_GAP_EVENT_DISC:
        record(&ev->disc);
        break;
    case BLE_GAP_EVENT_DISC_COMPLETE:
        report();
        if (g_done) g_done();
        break;
    default:
        break;
    }
    return 0;
}

void radio_diag_run(radio_diag_done_fn done)
{
    g_done = done;
    g_npeers = 0;
    g_dropped = 0;
    log_bonds();

    uint8_t own;
    int rc = ble_hs_id_infer_auto(0, &own);
    /* Passive, no duplicate filtering, 100 % duty on 1M: we want every report and its RSSI. */
    struct ble_gap_disc_params p = {
        .itvl = BLE_GAP_SCAN_ITVL_MS(100),
        .window = BLE_GAP_SCAN_WIN_MS(100),
        .filter_policy = BLE_HCI_SCAN_FILT_NO_WL,
        .limited = 0,
        .passive = 1,
        .filter_duplicates = 0,
    };
    if (rc == 0) {
        ESP_LOGI(TAG, "passive scan for %d s", CONFIG_KVMIT_RADIO_DIAG_SCAN_S);
        rc = ble_gap_disc(own, CONFIG_KVMIT_RADIO_DIAG_SCAN_S * 1000, &p, disc_event, NULL);
    }
    if (rc != 0) {
        ESP_LOGE(TAG, "scan start rc=%d", rc);
        if (done) done();
    }
}

#endif /* CONFIG_KVMIT_RADIO_DIAG */
