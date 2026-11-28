//! Nonfatal streaming UTF-8, matching frozen consumeSseStream's TextDecoder.
use std::borrow::Cow;

pub(super) struct LossyUtf8 {
    // At most three bytes of an incomplete scalar, never a complete line/event.
    incomplete: Vec<u8>,
}

impl LossyUtf8 {
    pub(super) fn new() -> Self {
        Self {
            incomplete: Vec::new(),
        }
    }

    pub(super) fn decode(&mut self, input: &[u8], eof: bool) -> Vec<u8> {
        let bytes = if self.incomplete.is_empty() {
            Cow::Borrowed(input)
        } else {
            let mut bytes = std::mem::take(&mut self.incomplete);
            bytes.extend_from_slice(input);
            Cow::Owned(bytes)
        };
        let mut remaining = bytes.as_ref();
        let mut output = Vec::new();
        loop {
            match std::str::from_utf8(remaining) {
                Ok(_) => {
                    output.extend_from_slice(remaining);
                    return output;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    output.extend_from_slice(&remaining[..valid]);
                    remaining = &remaining[valid..];
                    if let Some(invalid) = error.error_len() {
                        output.extend_from_slice("�".as_bytes());
                        remaining = &remaining[invalid..];
                    } else {
                        if eof {
                            output.extend_from_slice("�".as_bytes());
                        } else {
                            self.incomplete.extend_from_slice(remaining);
                        }
                        return output;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_utf8_decoder_buffers_incomplete_sequences_and_flushes_on_eof() {
        let mut decoder = LossyUtf8::new();
        assert!(decoder.decode(b"\xe2\x82", false).is_empty());
        assert_eq!(decoder.incomplete, b"\xe2\x82");
        assert_eq!(decoder.decode(b"\xac", false), "€".as_bytes());
        assert!(decoder.incomplete.is_empty());
        assert!(decoder.decode(b"\xf0\x9f\xa6", false).is_empty());
        assert_eq!(decoder.incomplete.len(), 3);
        assert_eq!(decoder.decode(b"", true), "�".as_bytes());
        assert!(decoder.incomplete.is_empty());
        assert!(decoder.decode(b"", true).is_empty());
    }
}
