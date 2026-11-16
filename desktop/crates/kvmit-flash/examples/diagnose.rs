//! Run the normal validated flasher without building the GUI:
//! cargo run -p kvmit-flash --example diagnose -- COM3 <firmware-dir> [baud]
//! Writes firmware, preserving settings. Never connect the board's native USB here.
use kvmit_flash::{flash, list_ports, usb_devices, check_no_native_adapter, FlashStage, Image, Options, ProgressCallbacks};

struct Progress;
impl ProgressCallbacks for Progress {
    fn stage(&mut self, stage: &FlashStage) { eprintln!("{stage}"); }
    fn init(&mut self, addr: u32, total: usize) { eprintln!("0x{addr:06x}: {total} blocks"); }
    fn update(&mut self, current: usize) { eprint!(" {current}"); }
    fn verifying(&mut self) { eprintln!(" verifying"); }
    fn finish(&mut self, skipped: bool) { eprintln!(" done (skipped={skipped})"); }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    check_no_native_adapter(&usb_devices()?)?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: diagnose <COM-port> <firmware-dir> [baud]; writes firmware, keeps settings".into());
    }
    let port = list_ports().into_iter().find(|p| p.name == args[0]).ok_or("USB serial port not found")?;
    let image = Image::from_build_dir(std::path::Path::new(&args[1]))?;
    let baud = args.get(2).map(|s| s.parse()).transpose()?;
    let report = flash(&port, &image, Options { baud, ..Options::default() }, &mut Progress)?;
    println!("flashed and verified: {report:?}");
    Ok(())
}
