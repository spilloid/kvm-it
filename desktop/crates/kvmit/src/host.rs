//! `Host` implementation that drives a real adapter (and optional capture) for the script engine.
//! Runs on a worker thread; blocks on the async device through a runtime handle.
use kvmit_ble::Device;
use kvmit_hid::Key;
use kvmit_script::{Frame, Host, RunEvent};
use kvmit_video::Capture;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Handle;

pub type ConfirmFn = Box<dyn FnMut(&str) -> bool + Send>;
pub type EventFn = Box<dyn FnMut(RunEvent) + Send>;

pub struct ScriptHost {
    pub rt: Handle,
    pub dev: Device,
    pub capture: Option<Arc<Mutex<Option<Capture>>>>,
    /// Directory reference images are resolved against (the script's folder).
    pub base_dir: PathBuf,
    pub cancel: Arc<AtomicBool>,
    pub confirm: ConfirmFn,
    pub on_event: EventFn,
}

fn e(err: kvmit_ble::LinkError) -> String {
    err.to_string()
}

impl Host for ScriptHost {
    fn key_down(&mut self, key: Key) -> Result<(), String> {
        self.rt.block_on(self.dev.key_down(key)).map_err(e)
    }
    fn key_up(&mut self, key: Key) -> Result<(), String> {
        self.rt.block_on(self.dev.key_up(key)).map_err(e)
    }
    fn release_all(&mut self) -> Result<(), String> {
        self.rt.block_on(self.dev.release_all()).map_err(e)
    }
    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        self.dev.mouse_move(dx, dy);
        Ok(())
    }
    fn mouse_button(&mut self, mask: u8, down: bool) -> Result<(), String> {
        self.rt.block_on(self.dev.button(mask, down)).map_err(e)
    }
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
    fn screen(&mut self) -> Option<Frame> {
        let cap = self.capture.as_ref()?.lock().ok()?;
        cap.as_ref()?.latest().map(|f| f.to_gray())
    }
    fn reference(&mut self, name: &str) -> Result<Frame, String> {
        let p = std::path::Path::new(name);
        if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(format!("reference image {name:?} must be a relative path inside the script's folder"));
        }
        kvmit_video::convert::load_reference(&self.base_dir.join(p))
    }
    fn confirm(&mut self, message: &str) -> bool {
        (self.confirm)(message)
    }
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
    fn event(&mut self, ev: RunEvent) {
        (self.on_event)(ev)
    }
}
