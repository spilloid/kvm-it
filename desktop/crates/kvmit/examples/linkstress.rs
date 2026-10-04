//! Link stress test (hardware): how do acked requests behave while mouse motion is streaming?
//! Usage: linkstress <adapter-address> [motion_hz=125] [seconds=15] [video]
//! With `video`, the capture card is opened and decoded for the whole run, like the GUI does (isolates link behaviour
//! under video load from the GUI's own rendering).
//! The GUI streams motion at up to 125 frames/s while you move the mouse; key and button events are acked
//! requests on the same link, so they should stay fast. Prints latency and failure counts; exit code 1 on any failure.
use kvmit_ble::{backend, Device};
use kvmit_hid::parse_key;
use kvmit_video::{list_devices, Capture};
use std::time::{Duration, Instant};

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p) as usize]
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().expect("usage: linkstress <adapter-address> [motion_hz=125] [seconds=15]");
    let hz: u64 = args.next().map(|s| s.parse().expect("motion_hz")).unwrap_or(125);
    let secs: u64 = args.next().map(|s| s.parse().expect("seconds")).unwrap_or(15);

    let with_video = args.next().is_some_and(|a| a == "video");
    let cap = with_video.then(|| {
        let d = list_devices().into_iter().next().expect("no capture device");
        let c = Capture::open(&d.path).expect("open capture");
        println!("video: {} {}x{} @ {} fps", d.name, c.mode.width, c.mode.height, c.mode.fps);
        c
    });
    let mut conn = backend::connect(&id).await.expect("connect (is the adapter paired?)");
    let dev = Device::connect(conn.take_io()).await.expect("handshake");
    println!("connected to {}; motion {hz}/s for {secs}s", dev.info().name);

    let mover = (hz > 0).then(|| {
        let d = dev.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_micros(1_000_000 / hz));
            // Never cancels out inside one flush window: every tick carries real motion, like a moving hand
            // (+3 for 100 ticks, then -3 for 100), so the adapter sees a full `hz` frames a second.
            let (mut dx, mut n) = (3, 0u32);
            loop {
                tick.tick().await;
                d.mouse_move(dx, 1);
                n += 1;
                if n % 100 == 0 {
                    dx = -dx;
                }
            }
        })
    });

    // A key with no side effect on the target (Shift x5 would raise the Sticky Keys prompt).
    let key = parse_key("F24").or_else(|| parse_key("F13")).expect("F24/F13");
    let (mut lat, mut ping_lat) = (Vec::new(), Vec::new());
    let (mut fails, mut errs) = (0u32, Vec::<String>::new());
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        let t = Instant::now();
        match dev.key_tap(key).await {
            Ok(()) => lat.push(t.elapsed().as_secs_f64() * 1000.0),
            Err(e) => {
                fails += 1;
                errs.push(e.to_string());
            }
        }
        if let Ok(p) = dev.ping().await {
            ping_lat.push(p.as_secs_f64() * 1000.0);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if let Some(m) = mover {
        m.abort();
    }
    let n = lat.len();
    println!("key taps: {n} ok, {fails} failed");
    println!("  key tap latency ms  p50 {:.0}  p95 {:.0}  max {:.0}", pct(&mut lat, 0.5), pct(&mut lat, 0.95), pct(&mut lat, 1.0));
    println!("  ping latency ms     p50 {:.0}  p95 {:.0}  max {:.0}", pct(&mut ping_lat, 0.5), pct(&mut ping_lat, 0.95), pct(&mut ping_lat, 1.0));
    for e in errs.iter().take(3) {
        println!("  error: {e}");
    }
    if let Some(c) = &cap {
        println!("video frames decoded: {}", c.latest().map(|f| f.seq).unwrap_or(0));
    }
    dev.release_all().await.ok();
    dev.shutdown().await;
    conn.disconnect().await;
    if fails > 0 {
        std::process::exit(1);
    }
}
