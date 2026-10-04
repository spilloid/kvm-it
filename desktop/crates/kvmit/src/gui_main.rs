//! `kvmit-gui`: the graphical app as its own executable. On Windows it is a GUI-subsystem program, so double-clicking
//! it opens no console window; because nothing is then there to print an error to, a failed start is shown in a
//! dialog instead of vanishing. `kvmit gui` (the CLI binary) starts the same app.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    if kvmit::syskeys::run_helper_if_requested() {
        return; // started as the keyboard-grab helper (Windows)
    }
    if let Err(e) = kvmit::gui::run_gui() {
        let text = e.to_string();
        let hint = if text.to_lowercase().contains("opengl") {
            "\n\nThis computer's graphics driver does not provide OpenGL 2.0 or newer. Update the graphics driver; in a virtual machine without a GPU, use a software OpenGL (see the kvm-it docs)."
        } else {
            ""
        };
        fatal(&format!("kvm-it could not start:\n\n{text}{hint}"));
    }
}

#[cfg(windows)]
fn fatal(msg: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(msg), wide("kvm-it"));
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR);
    }
    std::process::exit(1);
}

#[cfg(not(windows))]
fn fatal(msg: &str) {
    eprintln!("{msg}");
    std::process::exit(1);
}
