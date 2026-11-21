use super::*;
use kvmit_layout::Layout;
use zeroize::Zeroizing;

fn prepare(text: String) -> Result<Zeroizing<String>, &'static str> {
    let text = Zeroizing::new(text);
    if text.is_empty() {
        return Err("Copy some text on this computer first.");
    }
    if text.len() > 32 * 1024 {
        return Err("Paste up to 32 KiB of text at a time. Use Files for longer content.");
    }
    let normalized = Zeroizing::new(text.replace("\r\n", "\n"));
    if !normalized.chars().all(|c| UsAnsi.stroke(c).is_some()) {
        return Err("This text contains characters the US keyboard layout cannot type. Use Files to preserve the original text.");
    }
    Ok(normalized)
}

impl App {
    pub(super) fn paste_clipboard(&mut self, value: Option<String>) {
        if self.files.preparing.load(Ordering::SeqCst) {
            self.set_notice("Wait for the target USB connection before pasting.");
            return;
        }
        if self.device().is_none() || self.run_handle.as_ref().is_some_and(|h| h.running()) {
            self.set_notice("Connect an adapter and finish the current workflow before pasting.");
            return;
        }
        if self.paste_active.load(Ordering::SeqCst) {
            return;
        }
        let value = value.or_else(|| arboard::Clipboard::new().ok()?.get_text().ok());
        let value = match value
            .ok_or("Copy text on this computer first.")
            .and_then(prepare)
        {
            Ok(s) => s,
            Err(reason) => {
                self.set_notice(reason);
                return;
            }
        };
        if let Some(tx) = self.pump() {
            let active = Arc::new(AtomicBool::new(true));
            if tx.send(Input::Paste(value, active.clone())).is_ok() {
                self.paste_active = active;
                self.set_notice("Typing clipboard text…");
            }
        }
    }
}

pub(super) async fn type_text(
    dev: &Device,
    text: &str,
    active: &AtomicBool,
    held: &[Key],
) -> Result<(), kvmit_ble::LinkError> {
    if !active.load(Ordering::SeqCst) {
        return Ok(());
    }
    dev.release_all().await?;
    let mut shift = false;
    for c in text.chars() {
        if !active.load(Ordering::SeqCst) {
            break;
        }
        let stroke = UsAnsi
            .stroke(c)
            .expect("clipboard preflight checked every character");
        if shift != stroke.shift {
            if stroke.shift {
                dev.key_down(Key::LEFT_SHIFT).await?;
            } else {
                dev.key_up(Key::LEFT_SHIFT).await?;
            }
            shift = stroke.shift;
        }
        dev.key_tap(stroke.key).await?;
    }
    dev.release_all().await?;
    if active.load(Ordering::SeqCst) {
        for key in held {
            dev.key_down(*key).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paste_preflights_the_whole_clipboard_and_normalizes_windows_newlines() {
        assert_eq!(prepare("A\r\nB\tC".into()).unwrap().as_str(), "A\nB\tC");
        assert!(prepare("valid prefix 🦀".into()).is_err());
        assert!(prepare("a".repeat(32769)).is_err());
    }
    #[test]
    fn shortcut_is_local_once_and_plain_paste_still_reaches_the_target() {
        use crate::syskeys::{Action, Event, Tracker};
        let mut t = Tracker::default();
        t.on_usage(Some(Key::LEFT_CTRL), true);
        let v = Key(0x19);
        assert_eq!(
            t.on_usage(Some(v), true),
            Action::Swallow(Some(Event::Down(v)))
        );
        t.on_usage(Some(v), false);
        t.on_usage(Some(Key::LEFT_SHIFT), true);
        assert_eq!(
            t.on_usage(Some(v), true),
            Action::Swallow(Some(Event::Paste))
        );
        assert_eq!(t.on_usage(Some(v), true), Action::Swallow(None));
        assert_eq!(t.on_usage(Some(v), false), Action::Swallow(None));
    }
}
