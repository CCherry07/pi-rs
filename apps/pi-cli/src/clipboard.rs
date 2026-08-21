use std::io;

use base64::Engine as _;

pub(crate) trait ClipboardWriter {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
}

#[derive(Default)]
pub(crate) struct SystemClipboard {
    native: Option<arboard::Clipboard>,
}

impl ClipboardWriter for SystemClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        let native_result = (|| {
            if self.native.is_none() {
                self.native = Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
            }
            self.native
                .as_mut()
                .expect("clipboard initialized")
                .set_text(text.to_string())
                .map_err(|error| error.to_string())
        })();
        if native_result.is_ok() {
            return Ok(());
        }

        let mut stdout = io::stdout().lock();
        write_osc52(&mut stdout, text).map_err(|osc52_error| {
            format!(
                "native clipboard failed ({}); OSC 52 failed ({osc52_error})",
                native_result.expect_err("checked above")
            )
        })
    }
}

fn write_osc52(writer: &mut impl io::Write, text: &str) -> io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    write!(writer, "\x1b]52;c;{encoded}\x07")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_fallback_encodes_unicode_for_the_host_terminal() {
        let mut output = Vec::new();

        write_osc52(&mut output, "复制").unwrap();

        assert_eq!(output, b"\x1b]52;c;5aSN5Yi2\x07");
    }
}
