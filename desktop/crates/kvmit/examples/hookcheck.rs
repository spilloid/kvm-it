//! Hardware check for the OS-level keyboard grab (Windows): install it, wait, and report how many key events it saw.
//! Usage: hookcheck [seconds=10]. Ctrl+Alt+Esc ends it early (the release chord).
//! It prints COUNTS only, never which keys: while the grab is on it sees everything typed, and the one rule this
//! project cannot bend is that typed input is never logged.
use kvmit::syskeys::{Event, Grab};
use std::time::{Duration, Instant};

fn main() {
    if kvmit::syskeys::run_helper_if_requested() {
        return; // this exe doubles as the keyboard-grab helper
    }
    let secs: u64 = std::env::args().nth(1).map(|s| s.parse().expect("seconds")).unwrap_or(10);
    let Some(mut grab) = Grab::start(|| {}) else {
        println!("no OS-level keyboard grab on this platform (or Windows refused the hook)");
        return;
    };
    println!("grab installed; capturing for up to {secs} s (Ctrl+Alt+Esc ends it)");
    let (mut down, mut up, mut released) = (0u32, 0u32, false);
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end && !released {
        let (events, saw_release) = kvmit::syskeys::until_release(grab.drain());
        for e in events {
            match e {
                Event::Down(_) => down += 1,
                Event::Up(_) => up += 1,
                Event::Release => {}
            }
        }
        released = saw_release;
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("events seen: {down} down, {up} up, release chord: {}", if released { "yes" } else { "no" });
}
