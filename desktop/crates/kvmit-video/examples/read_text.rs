//! Dev tool: print the text the OS's recogniser reads on each picture, with where each line is (fractions of the frame)
//! and how long the reading took. `cargo run -p kvmit-video --example read_text -- <picture.png>...`
fn main() {
    for p in std::env::args().skip(1) {
        let frame = match kvmit_video::convert::load_reference(std::path::Path::new(&p)) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{p}: {e}");
                continue;
            }
        };
        let t = std::time::Instant::now();
        match kvmit_video::text::read_text(&frame) {
            Ok(lines) => {
                println!("== {p} ({}x{}, {} ms)", frame.width, frame.height, t.elapsed().as_millis());
                for l in lines {
                    println!("  [{:.2} {:.2} {:.2} {:.2}] {}", l.rect[0], l.rect[1], l.rect[2], l.rect[3], l.text);
                }
            }
            Err(e) => eprintln!("{p}: {e}"),
        }
    }
}
