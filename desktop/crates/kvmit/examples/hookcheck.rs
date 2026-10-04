//! Hardware check for the OS-level keyboard grab (Windows): install it, wait, report how many key events it saw.
//! Usage: hookcheck [seconds=10]. Prints each event (HID usage) so a test can send known keys: run it only with
//! synthetic input, never while typing anything real. The library itself never logs keys.
use kvmit::syskeys::{Event, Grab};
use std::time::{Duration, Instant};

fn main() {
    if kvmit::syskeys::run_helper_if_requested() {
        return; // this exe doubles as the keyboard-grab helper
    }
    let secs: u64 = std::env::args().nth(1).map(|s| s.parse().expect("seconds")).unwrap_or(10);
    let Some(mut grab) = Grab::start() else {
        println!("no OS-level keyboard grab on this platform (or Windows refused the hook)");
        return;
    };
    println!("hook installed; capturing for {secs} s");
    let (mut down, mut up, mut release) = (0u32, 0u32, 0u32);
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        for e in grab.drain() {
            println!("  {e:?}");
            match e {
                Event::Down(_) => down += 1,
                Event::Up(_) => up += 1,
                Event::Release => release += 1,
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("events seen: {down} down, {up} up, {release} release chord(s)");
}
