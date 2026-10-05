#include "ble_link.h"

#include <string.h>

#include "esp_bt.h"
#include "esp_log.h"
#include "esp_system.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "host/ble_hs.h"
#include "host/util/util.h"
#include "kvm_config.h"
#include "nimble/nimble_port.h"
#include "nimble/nimble_port_freertos.h"
#include "proto_dispatch.h"
#include "radio_diag.h"
#include "services/gap/ble_svc_gap.h"
#include "services/gatt/ble_svc_gatt.h"
#include "usb_hid.h"

void ble_store_config_init(void);

static const char *TAG = "ble";

/* UUIDs (protocol/SPEC.md "Transport"), stored little-endian for NimBLE.
 * service 7a1e0000-4b49-4d54-8000-6b766d697401, rx ...0001..., tx ...0002... */
static const ble_uuid128_t svc_uuid = BLE_UUID128_INIT(0x01, 0x74, 0x69, 0x6d, 0x76, 0x6b, 0x00, 0x80, 0x54, 0x4d, 0x49, 0x4b, 0x00, 0x00, 0x1e, 0x7a);
static const ble_uuid128_t rx_uuid = BLE_UUID128_INIT(0x01, 0x74, 0x69, 0x6d, 0x76, 0x6b, 0x00, 0x80, 0x54, 0x4d, 0x49, 0x4b, 0x01, 0x00, 0x1e, 0x7a);
static const ble_uuid128_t tx_uuid = BLE_UUID128_INIT(0x01, 0x74, 0x69, 0x6d, 0x76, 0x6b, 0x00, 0x80, 0x54, 0x4d, 0x49, 0x4b, 0x02, 0x00, 0x1e, 0x7a);

/* ---- shared state ---- */
static volatile uint16_t g_conn = BLE_HS_CONN_HANDLE_NONE;
static volatile bool g_encrypted;
static bool g_new_pairing;  /* host task: the current link is pairing a new bond (not re-encrypting a stored one) */
static volatile bool g_handshaken;
static volatile bool g_adv_active;
static volatile bool g_started;
static volatile bool g_fault;
static volatile int64_t g_window_until_us;  /* nonzero: pairing window open; value = when the timer asks to close it */
static volatile uint32_t g_activity;
static uint8_t g_own_addr_type;
static uint16_t g_tx_handle;
static esp_timer_handle_t g_window_timer;

/* ---- worker queue: NimBLE host task never blocks on USB sends ---- */
/* Frames carry the connection generation they arrived on. Session start/end are NOT queue items (a full
 * queue must never swallow a disconnect): the host task bumps g_gen and the worker reconciles it. */
typedef struct { uint32_t gen; uint16_t len; uint8_t data[PROTO_MAX_PAYLOAD + PROTO_OVERHEAD]; } qitem_t;
static volatile uint32_t g_gen;
static QueueHandle_t g_queue;
static proto_dev_t g_dev;

/* The window is host-task state: open from open_window() until ev_close()/pairing runs set_window(0) on the host
 * task, which is also when sm_bonding changes. So "window open" and "bonding allowed" can never disagree, even if
 * the deadline has passed and the close request is still queued behind other host work. */
static bool window_open(void) { return g_window_until_us != 0; }

/* NimBLE accepts SMP pairing as soon as a link exists, which can be before our CONNECT event, so the window must
 * also gate bonding inside the stack: outside it our side does not set the bonding flag, no keys are persisted,
 * and ENC_CHANGE (not bonded) disconnects the peer. Re-encrypting an existing bond is unaffected.
 * Host task only (see "host-task requests"): SMP reads sm_bonding on the same task, so they cannot interleave. */
static void set_window(int64_t until_us)
{
    g_window_until_us = until_us;
    ble_hs_cfg.sm_bonding = until_us != 0;
}

static void terminate(uint16_t conn)
{
    int rc = ble_gap_terminate(conn, BLE_ERR_REM_USER_CONN_TERM);
    if (rc != 0 && rc != BLE_HS_EALREADY && rc != BLE_HS_ENOTCONN) ESP_LOGE(TAG, "terminate handle %u rc=%d", conn, rc);
}

static bool have_bond(void)
{
    int n = 0;
    ble_store_util_count(BLE_STORE_OBJ_TYPE_OUR_SEC, &n);
    return n > 0;
}

led_link_t ble_link_led_state(void)
{
    if (g_fault) return LED_LINK_FAULT;
    if (!g_started) return LED_LINK_BOOTING;
    if (g_handshaken) return LED_LINK_CONNECTED;
    if (window_open()) return LED_LINK_PAIRING;
    return have_bond() ? LED_LINK_RECONNECT : LED_LINK_IDLE;
}

uint32_t ble_link_activity(void) { return g_activity; }

/* ---- advertising ---- */
static int gap_event(struct ble_gap_event *ev, void *arg);

static void adv_start(void)
{
    if (!g_started || g_conn != BLE_HS_CONN_HANDLE_NONE || g_adv_active) return;
    if (!window_open() && !have_bond()) {
        ESP_LOGI(TAG, "no trusted controller and pairing window closed: radio quiet (press BOOT to pair)");
        return;
    }
    char name[KVM_NAME_MAX + 1];
    kvm_config_name(name);
    ble_svc_gap_device_name_set(name);

    struct ble_hs_adv_fields f = {0};
    f.flags = BLE_HS_ADV_F_DISC_GEN | BLE_HS_ADV_F_BREDR_UNSUP;
    f.uuids128 = (ble_uuid128_t *)&svc_uuid;
    f.num_uuids128 = 1;
    f.uuids128_is_complete = 1;
    int rc = ble_gap_adv_set_fields(&f);
    struct ble_hs_adv_fields r = {0};
    r.name = (uint8_t *)name;
    r.name_len = strlen(name) > 29 ? 29 : strlen(name);  /* legacy scan response holds 31 bytes incl. 2 of header */
    r.name_is_complete = strlen(name) <= 29;
    if (rc == 0) rc = ble_gap_adv_rsp_set_fields(&r);
    if (rc != 0) { ESP_LOGE(TAG, "adv fields rc=%d", rc); return; }

    struct ble_gap_adv_params p = {0};
    p.conn_mode = BLE_GAP_CONN_MODE_UND;
    p.disc_mode = BLE_GAP_DISC_MODE_GEN;
    /* Faster while pairing is wanted, gentler while merely waiting for the known controller. */
    p.itvl_min = window_open() ? BLE_GAP_ADV_ITVL_MS(20) : BLE_GAP_ADV_ITVL_MS(200);
    p.itvl_max = window_open() ? BLE_GAP_ADV_ITVL_MS(30) : BLE_GAP_ADV_ITVL_MS(300);
    rc = ble_gap_adv_start(g_own_addr_type, NULL, BLE_HS_FOREVER, &p, gap_event, NULL);
    if (rc == BLE_HS_EALREADY) { g_adv_active = true; return; }  /* another task got there first */
    if (rc == BLE_HS_ENOMEM) return;  /* a link not yet reported by CONNECT holds the only slot; reconciled later */
    if (rc != 0) { ESP_LOGE(TAG, "adv start rc=%d", rc); return; }
    g_adv_active = true;
    ESP_LOGI(TAG, "advertising as \"%s\" (%s)", name, window_open() ? "pairing window open" : "known controller only");
}

static void adv_restart(void)
{
    if (g_adv_active) { ble_gap_adv_stop(); g_adv_active = false; }
    adv_start();
}

/* ---- host-task requests ----
 * The pairing window, sm_bonding and advertising are changed only on the NimBLE host task, the same task that runs
 * GAP events and SMP. Other tasks (BOOT button, esp_timer, worker) post an event instead of acting, so no policy
 * check can be interleaved with a window change or a pairing. */
static struct ble_npl_event g_ev_open, g_ev_close, g_ev_reset, g_ev_reconcile;
static volatile bool g_events_ready;
static volatile uint32_t g_open_req_s;

static void post(struct ble_npl_event *ev)
{
    if (g_events_ready) ble_npl_eventq_put(nimble_port_get_dflt_eventq(), ev);
    else ESP_LOGW(TAG, "BLE not started yet: request ignored");
}

static void open_window(uint32_t seconds)
{
    set_window(esp_timer_get_time() + (int64_t)seconds * 1000000);
    esp_timer_stop(g_window_timer);
    esp_timer_start_once(g_window_timer, (uint64_t)seconds * 1000000);
    ESP_LOGI(TAG, "pairing window open for %u s", (unsigned)seconds);
    if (g_started && g_conn == BLE_HS_CONN_HANDLE_NONE) adv_restart();
}

static void ev_open(struct ble_npl_event *ev) { (void)ev; open_window(g_open_req_s); }

static void ev_close(struct ble_npl_event *ev)
{
    (void)ev;
    if (!window_open()) return;  /* already closed (paired) */
    if (esp_timer_get_time() < g_window_until_us) {
        ble_hs_cfg.sm_bonding = 1;  /* reopened since the timer fired: undo its early clear */
        return;
    }
    ESP_LOGI(TAG, "pairing window closed");
    set_window(0);
    /* Not connected + no bond: go quiet. A pairing already admitted keeps its bonding decision. */
    if (g_conn == BLE_HS_CONN_HANDLE_NONE) adv_restart();
}

static void ev_reset(struct ble_npl_event *ev)
{
    (void)ev;
    ble_store_clear();
    if (g_conn != BLE_HS_CONN_HANDLE_NONE) terminate(g_conn);
    open_window(CONFIG_KVMIT_PAIRING_WINDOW_S);
}

/* Advertising should be on exactly when started, idle, and (window open or a bond to reconnect). Repairs the case
 * NimBLE never reports (a link lost before CONNECT: no DISCONNECT) and stops advertising policy no longer allows. */
static void ev_reconcile(struct ble_npl_event *ev)
{
    (void)ev;
    if (!g_started || g_conn != BLE_HS_CONN_HANDLE_NONE) return;
    bool want = window_open() || have_bond();
    bool on = ble_gap_adv_active();
    if (want && !on) {
        g_adv_active = false;
        adv_start();
    } else if (!want && on) {
        ble_gap_adv_stop();
        g_adv_active = false;
        ESP_LOGI(TAG, "advertising stopped: no trusted controller and pairing window closed");
    }
}

/* Hard deadline: refuse bonding at the moment the window expires, not when the queued close reaches the host task.
 * Only ever clears the flag (the host task alone sets it, when it opens a window), so this cannot produce
 * "window closed, bonding allowed"; a stale clear after a reopen is undone by ev_close. NimBLE offers no hook at
 * SMP admission, so the remaining gap is the esp_timer dispatch latency. */
static void window_expired(void *arg)
{
    (void)arg;
    ble_hs_cfg.sm_bonding = 0;
    post(&g_ev_close);
}

void ble_link_open_pairing(uint32_t seconds)
{
    g_open_req_s = seconds;
    post(&g_ev_open);
}

void ble_link_trust_reset(void) { post(&g_ev_reset); }

/* Keep exactly one bond: after a successful new pairing, drop every other peer. */
static void keep_only(const ble_addr_t *keep)
{
    ble_addr_t peers[MYNEWT_VAL(BLE_STORE_MAX_BONDS)];
    int n = 0;
    if (ble_store_util_bonded_peers(peers, &n, MYNEWT_VAL(BLE_STORE_MAX_BONDS)) != 0) return;
    for (int i = 0; i < n; i++)
        if (ble_addr_cmp(&peers[i], keep) != 0) ble_store_util_delete_peer(&peers[i]);
}


static int gap_event(struct ble_gap_event *ev, void *arg)
{
    (void)arg;
    struct ble_gap_conn_desc d;
    switch (ev->type) {
    case BLE_GAP_EVENT_CONNECT:
        g_adv_active = false;
        if (ev->connect.status != 0) { adv_start(); return 0; }
        g_conn = ev->connect.conn_handle;
        g_handshaken = false;
        if (ble_gap_conn_find(g_conn, &d) == 0) {
            /* NimBLE delivers CONNECT after the remote-feature exchange; encryption may already be up (its
             * ENC_CHANGE ran the bonding checks), so adopt it instead of resetting it. */
            g_encrypted = d.sec_state.encrypted && d.sec_state.bonded;
            /* sec_state.bonded is only set once encrypted, so ask the store directly. */
            ble_addr_t id = d.peer_id_addr;
            struct ble_store_key_sec k = {0};
            k.peer_addr = id;
            struct ble_store_value_sec v;
            bool bonded = ble_store_read_peer_sec(&k, &v) == 0;
            g_new_pairing = !bonded;
            if (!bonded && !window_open()) {
                ESP_LOGW(TAG, "unbonded peer outside pairing window: disconnecting");
                terminate(g_conn);
                return 0;
            }
            ESP_LOGI(TAG, "connected (%s, handle %u)", bonded ? "bonded peer" : "new peer, pairing", g_conn);
        }
        else g_encrypted = false;
        g_gen++;  /* new session; the worker resets the dispatcher before touching any frame of it */
        if (!g_encrypted) ble_gap_security_initiate(g_conn);
        return 0;
    case BLE_GAP_EVENT_DISCONNECT:
        ESP_LOGI(TAG, "disconnected (reason 0x%x)", ev->disconnect.reason);
        g_conn = BLE_HS_CONN_HANDLE_NONE;
        g_encrypted = false;
        g_new_pairing = false;
        g_handshaken = false;
        g_gen++;  /* worker releases all input for the old session */
        adv_start();
        return 0;
    case BLE_GAP_EVENT_ENC_CHANGE:
        if (ev->enc_change.status == 0 && ble_gap_conn_find(ev->enc_change.conn_handle, &d) == 0) {
            if (!d.sec_state.bonded) {
                /* Encrypted but not bonded (peer cleared its bonding flag): no persistent trust, refuse. */
                ESP_LOGW(TAG, "peer did not bond: disconnecting");
                terminate(ev->enc_change.conn_handle);
                return 0;
            }
            g_encrypted = true;
            /* Exactly one trusted controller, regardless of whether the window expired mid-pairing. */
            keep_only(&d.peer_id_addr);
            /* A new pairing uses up the window. The trusted controller merely reconnecting does not, so an app that
             * auto-reconnects at power-on leaves the power-on window available for pairing a new controller. If
             * encryption completed before CONNECT was delivered we cannot tell, so close it to be safe. */
            if (window_open() && (g_new_pairing || g_conn == BLE_HS_CONN_HANDLE_NONE)) {
                set_window(0);
                esp_timer_stop(g_window_timer);
                ESP_LOGI(TAG, "paired: pairing window closed");
            }
            ESP_LOGI(TAG, "link encrypted and bonded");
        } else if (ev->enc_change.status != 0) {
            ESP_LOGW(TAG, "encryption failed (status %d): disconnecting", ev->enc_change.status);
            terminate(ev->enc_change.conn_handle);
        }
        return 0;
    case BLE_GAP_EVENT_REPEAT_PAIRING:
        /* Peer lost its bond. Only replace ours while the physical window is open. */
        if (!window_open()) return BLE_GAP_REPEAT_PAIRING_IGNORE;
        if (ble_gap_conn_find(ev->repeat_pairing.conn_handle, &d) == 0) ble_store_util_delete_peer(&d.peer_id_addr);
        g_new_pairing = true;
        return BLE_GAP_REPEAT_PAIRING_RETRY;
    case BLE_GAP_EVENT_ADV_COMPLETE:
        g_adv_active = false;
        adv_start();
        return 0;
    case BLE_GAP_EVENT_MTU:
        ESP_LOGI(TAG, "MTU %u", ev->mtu.value);
        return 0;
    default:
        return 0;
    }
}

/* ---- GATT ---- */
static int rx_access(uint16_t conn, uint16_t attr, struct ble_gatt_access_ctxt *ctxt, void *arg)
{
    (void)attr; (void)arg;
    if (ctxt->op != BLE_GATT_ACCESS_OP_WRITE_CHR || !g_encrypted) return BLE_ATT_ERR_INSUFFICIENT_AUTHEN;
    uint16_t len = OS_MBUF_PKTLEN(ctxt->om);
    if (conn != g_conn || len == 0 || len > PROTO_MAX_PAYLOAD + PROTO_OVERHEAD) return BLE_ATT_ERR_INVALID_ATTR_VALUE_LEN;
    qitem_t it = {.gen = g_gen, .len = len};
    if (os_mbuf_copydata(ctxt->om, 0, len, it.data) != 0) return BLE_ATT_ERR_UNLIKELY;
    xQueueSend(g_queue, &it, 0);  /* full queue = dropped; the controller's retry/accumulation covers it */
    return 0;
}

static int tx_access(uint16_t c, uint16_t a, struct ble_gatt_access_ctxt *x, void *arg)
{
    (void)c; (void)a; (void)x; (void)arg;
    return BLE_ATT_ERR_READ_NOT_PERMITTED;
}

static const struct ble_gatt_svc_def gatt_svcs[] = {
    {.type = BLE_GATT_SVC_TYPE_PRIMARY,
     .uuid = &svc_uuid.u,
     .characteristics = (struct ble_gatt_chr_def[]){
         {.uuid = &rx_uuid.u, .access_cb = rx_access,
          .flags = BLE_GATT_CHR_F_WRITE | BLE_GATT_CHR_F_WRITE_NO_RSP | BLE_GATT_CHR_F_WRITE_ENC},
         {.uuid = &tx_uuid.u, .access_cb = tx_access, .val_handle = &g_tx_handle, .flags = BLE_GATT_CHR_F_NOTIFY},
         {0}}},
    {0}};

/* ---- worker: owns proto_dev, so USB sends never run on the NimBLE host task ---- */
static hidop_t map(esp_err_t e)
{
    switch (e) {
    case ESP_OK: return HIDOP_OK;
    case ESP_ERR_INVALID_STATE: return HIDOP_NOT_MOUNTED;
    case ESP_ERR_TIMEOUT: return HIDOP_BUSY;
    case ESP_ERR_NO_MEM: return HIDOP_REFUSED;
    default: return HIDOP_INVALID;
    }
}
static hidop_t o_down(void *c, uint8_t u) { (void)c; return map(usb_hid_key_down(u)); }
static hidop_t o_up(void *c, uint8_t u) { (void)c; return map(usb_hid_key_up(u)); }
static hidop_t o_rel(void *c) { (void)c; usb_hid_release_all(); return HIDOP_OK; }  /* local state always cleared; retried by usb_hid */
static hidop_t o_move(void *c, int8_t x, int8_t y) { (void)c; return map(usb_hid_mouse_move(x, y)); }
static hidop_t o_btn(void *c, uint8_t m, bool d) { (void)c; return map(usb_hid_mouse_button(m, d)); }
static hidop_t o_wheel(void *c, int8_t v, int8_t h) { (void)c; return map(usb_hid_mouse_wheel(v, h)); }
static bool o_mounted(void *c) { (void)c; return usb_hid_mounted(); }
static bool o_held(void *c) { (void)c; return usb_hid_any_held(); }
static void o_counts(void *c, uint8_t *k, uint8_t *b) { (void)c; usb_hid_counts(k, b); }
static bool o_name(void *c, const char *n, size_t l) { (void)c; return kvm_config_set_name(n, l) == ESP_OK; }

/* The USB descriptor is fixed for a session, so a changed boot-drive setting takes effect by restarting the adapter, shortly after the
 * acknowledgement has been sent (a retried request is answered from the dedup window and never schedules a second restart). The target
 * sees the adapter re-plug, with the drive added or removed. Unchanged: nothing happens. */
static void restart_cb(void *arg) { (void)arg; esp_restart(); }
static bool o_set_boot_drive(void *c, bool enabled)
{
    (void)c;
    if (enabled == kvm_config_boot_drive()) return true; /* already so: no restart */
    static esp_timer_handle_t t;
    const esp_timer_create_args_t a = {.callback = restart_cb, .name = "bootdrv_restart"};
    if (!t && esp_timer_create(&a, &t) != ESP_OK) return false; /* before the commit: a failure here must leave the stored setting alone */
    if (kvm_config_set_boot_drive(enabled) != ESP_OK) return false;
    esp_timer_start_once(t, 500 * 1000);
    return true;
}
static bool o_boot_drive(void *c) { (void)c; return usb_hid_boot_drive(); }

static uint32_t ms(void) { return (uint32_t)(esp_timer_get_time() / 1000); }

static void worker(void *arg)
{
    (void)arg;
    static qitem_t it;
    uint8_t resp[PROTO_MAX_PAYLOAD + PROTO_OVERHEAD];
    uint32_t seen_gen = 0;
    uint32_t last_tick = ms();
    for (;;) {
        BaseType_t got = xQueueReceive(g_queue, &it, pdMS_TO_TICKS(50));
        /* Reconcile connection generation first: release the old session, start the new one, and (below) drop
         * any frame that arrived on an older connection. Runs even when the queue is full or flooded. */
        uint32_t gen = g_gen;
        if (gen != seen_gen) {
            if (proto_dev_active(&g_dev)) {
                proto_dev_session_end(&g_dev);
                ESP_LOGI(TAG, "session ended: released all input");
            }
            seen_gen = gen;
            if (g_conn != BLE_HS_CONN_HANDLE_NONE) proto_dev_session_start(&g_dev, ms());
            g_handshaken = false;
        }
        /* Watchdog on wall time, independent of traffic (a stream of invalid frames must not starve it). */
        if ((uint32_t)(ms() - last_tick) >= 250) {
            last_tick = ms();
            if (proto_dev_tick(&g_dev, last_tick)) ESP_LOGW(TAG, "keepalive timeout: released all input");
            post(&g_ev_reconcile);  /* advertising matches policy; runs on the host task */
        }
        if (got != pdTRUE || it.gen != seen_gen) continue;
        uint16_t conn = g_conn;
        if (conn == BLE_HS_CONN_HANDLE_NONE || !g_encrypted) continue;
        uint32_t before_act = g_dev.activity;
        size_t rl = 0;
        uint16_t budget = ble_att_mtu(conn);
        budget = budget > 3 ? budget - 3 : 20;
        if (budget > sizeof resp) budget = sizeof resp;
        proto_dev_handle(&g_dev, it.data, it.len, ms(), resp, budget, &rl);
        g_handshaken = g_dev.handshaken && g_encrypted && gen == g_gen;
        if (g_dev.activity != before_act) g_activity = g_dev.activity;
        if (rl) {
            struct os_mbuf *om = ble_hs_mbuf_from_flat(resp, rl);
            if (om && ble_gatts_notify_custom(conn, g_tx_handle, om) != 0) ESP_LOGW(TAG, "notify failed (len %u)", (unsigned)rl);
        }
    }
}

/* ---- host sync / boot ---- */
static void start_link(void)
{
    static bool boot_window_used;  /* the power-on window is offered once per chip boot, not per host re-sync */
    g_started = true;
    /* Plug-in is physical presence: a power-on offers a short pairing window, bonded or not, so a new controller
     * needs no BOOT press. With a bond stored, only a physical reset counts (power-on/plug-in, or the EN/RESET pin,
     * which the COM port's auto-reset also drives): a crash or watchdog reboot must not reopen pairing, or anyone
     * able to crash the adapter could. A bonded controller reconnects during or after the window as usual. */
    esp_reset_reason_t why = esp_reset_reason();
    bool physical = why == ESP_RST_POWERON || why == ESP_RST_EXT;
    if (CONFIG_KVMIT_BOOT_PAIRING_WINDOW_S > 0 && !boot_window_used && (physical || !have_bond())) {
        boot_window_used = true;
        open_window(CONFIG_KVMIT_BOOT_PAIRING_WINDOW_S);
    } else {
        if (!physical) ESP_LOGI(TAG, "reset reason %d is not a power-on: no pairing window (press BOOT to pair)", why);
        adv_start();
    }
}

static void on_sync(void)
{
    int rc = ble_hs_util_ensure_addr(0);
    if (rc == 0) rc = ble_hs_id_infer_auto(0, &g_own_addr_type);
    if (rc != 0) { ESP_LOGE(TAG, "address setup rc=%d", rc); g_fault = true; return; }
    ESP_LOGI(TAG, "BLE ready, trusted controller %s", have_bond() ? "stored" : "none");
#if CONFIG_KVMIT_RADIO_DIAG
    radio_diag_run(start_link);
#else
    start_link();
#endif
}

static void on_reset(int reason) { ESP_LOGE(TAG, "host reset, reason %d", reason); }

static void host_task(void *arg)
{
    (void)arg;
    nimble_port_run();
    nimble_port_freertos_deinit();
}

esp_err_t ble_link_start(void)
{
    g_queue = xQueueCreate(8, sizeof(qitem_t));
    if (!g_queue) return ESP_ERR_NO_MEM;
    const esp_timer_create_args_t ta = {.callback = window_expired, .name = "pair_window"};
    ESP_ERROR_CHECK(esp_timer_create(&ta, &g_window_timer));

    proto_ops_t ops = {NULL, o_down, o_up, o_rel, o_move, o_btn, o_wheel, o_mounted, o_held, o_counts, o_name, o_set_boot_drive, o_boot_drive};
    uint8_t uuid[16], fw[3] = {0, 2, 0};
    char name[KVM_NAME_MAX + 1];
    kvm_config_uuid(uuid);
    kvm_config_name(name);
    proto_dev_init(&g_dev, &ops, uuid, fw, name);

    esp_err_t err = nimble_port_init();
    if (err != ESP_OK) { g_fault = true; return err; }
    ble_npl_event_init(&g_ev_open, ev_open, NULL);
    ble_npl_event_init(&g_ev_close, ev_close, NULL);
    ble_npl_event_init(&g_ev_reset, ev_reset, NULL);
    ble_npl_event_init(&g_ev_reconcile, ev_reconcile, NULL);
    g_events_ready = true;
    /* Laptops with weak LE scanning (observed: Surface Laptop 4) miss low-power adverts; use full power. */
    esp_ble_tx_power_set(ESP_BLE_PWR_TYPE_ADV, ESP_PWR_LVL_P9);
    esp_ble_tx_power_set(ESP_BLE_PWR_TYPE_DEFAULT, ESP_PWR_LVL_P9);
    ble_hs_cfg.sync_cb = on_sync;
    ble_hs_cfg.reset_cb = on_reset;
    ble_hs_cfg.sm_io_cap = BLE_SM_IO_CAP_NO_IO;  /* Just Works: no display/keypad on this board */
    ble_hs_cfg.sm_bonding = 0;  /* set only while the pairing window is open: set_window() */
    ble_hs_cfg.sm_mitm = 0;
    ble_hs_cfg.sm_sc = 1;
    ble_hs_cfg.sm_our_key_dist = BLE_SM_PAIR_KEY_DIST_ENC | BLE_SM_PAIR_KEY_DIST_ID;
    ble_hs_cfg.sm_their_key_dist = BLE_SM_PAIR_KEY_DIST_ENC | BLE_SM_PAIR_KEY_DIST_ID;
    ble_att_set_preferred_mtu(247);

    ble_svc_gap_init();
    ble_svc_gatt_init();
    int rc = ble_gatts_count_cfg(gatt_svcs);
    if (rc == 0) rc = ble_gatts_add_svcs(gatt_svcs);
    if (rc != 0) { g_fault = true; return ESP_FAIL; }
    ble_store_config_init();

    if (xTaskCreate(worker, "kvm_worker", 4096, NULL, 5, NULL) != pdPASS) return ESP_ERR_NO_MEM;
    nimble_port_freertos_init(host_task);
    return ESP_OK;
}
