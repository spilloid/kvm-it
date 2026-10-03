//! Script engine. Scripts are data (TOML) run by the controller; the firmware stays a dumb HID endpoint.
//! Nothing here touches files, the network or a shell. Run events never carry typed text or secrets.
pub mod duckyscript;
pub mod exec;
pub mod frame;
pub mod model;

pub use exec::{compile, preview, run, Host, Op, Preview, RunError, RunEvent, RunOptions, Vars};
pub use frame::Frame;
pub use model::{parse_duration, Script, Step, VarDef, WaitSpec};
