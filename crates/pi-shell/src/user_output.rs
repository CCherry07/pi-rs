//! Display/storage sanitization for user shell shorthand, not built-in tool output.
//!
//! Oracle: coding-agent/src/core/bash-executor.ts and src/utils/{ansi,shell}.ts.
//! The small parser below follows Pi's ANSI grammar (including its restricted
//! final bytes), rather than stripping arbitrary ESC-prefixed text.

#[derive(Default)]
pub(super) struct UserOutputSanitizer {
    pending_ansi: String,
    seen_text: bool,
}

impl UserOutputSanitizer {
    pub(super) fn push(&mut self, decoded: String) -> String {
        let mut text = self.prepend_pending(decoded);
        if let Some(start) = incomplete_ansi_suffix(&text) {
            self.pending_ansi = text.split_off(start);
        }
        sanitize(&text)
    }

    pub(super) fn finish(&mut self, decoded: String) -> String {
        // An incomplete sequence at EOF is processed as-is, like flushOutput
        // in Pi; ordinary text following an unfinished ESC is not discarded.
        sanitize(&self.prepend_pending(decoded))
    }

    fn prepend_pending(&mut self, mut decoded: String) -> String {
        if !self.seen_text && !decoded.is_empty() {
            self.seen_text = true;
            // TextDecoder's default BOM handling applies only at stream start.
            if decoded.starts_with('\u{feff}') {
                decoded.remove(0);
            }
        }
        if self.pending_ansi.is_empty() {
            return decoded;
        }
        self.pending_ansi.push_str(&decoded);
        std::mem::take(&mut self.pending_ansi)
    }
}

fn incomplete_ansi_suffix(text: &str) -> Option<usize> {
    // Pi bounds pending ANSI to the last 256 UTF-16 code units. Starting on the
    // next scalar when the window splits a surrogate cannot hide an ESC/CSI.
    let mut window_start = text.len();
    let mut units = 0;
    for (index, character) in text.char_indices().rev() {
        units += character.len_utf16();
        if units > 256 {
            break;
        }
        window_start = index;
    }
    for (offset, character) in text[window_start..].char_indices() {
        if !matches!(character, '\u{1b}' | '\u{9b}') {
            continue;
        }
        let start = window_start + offset;
        let suffix = &text[start..];
        if (suffix.starts_with("\u{1b}]") && osc_end(suffix).is_none())
            || csi_bases(suffix)
                .into_iter()
                .any(|base| parameter_ends(suffix, base).last() == Some(&suffix.len()))
        {
            return Some(start);
        }
    }
    None
}

fn sanitize(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    let mut no_osc_terminator = false;
    while offset < text.len() {
        let rest = &text[offset..];
        if rest.starts_with("\u{1b}]") && !no_osc_terminator {
            if let Some(end) = osc_end(rest) {
                offset += end;
                continue;
            }
            // No later OSC can find a terminator either. Avoid repeatedly
            // scanning a large chunk of unterminated OSC introducers.
            no_osc_terminator = true;
        }
        if let Some(end) = csi_end(rest) {
            offset += end;
            continue;
        }
        let character = rest.chars().next().expect("nonempty suffix");
        // sanitizeBinaryOutput, followed by removal of every CR. DEL and C1
        // controls not consumed by the ANSI grammar are intentionally retained.
        if !matches!(character, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{fff9}'..='\u{fffb}') {
            output.push(character);
        }
        offset += character.len_utf8();
    }
    output
}

fn osc_end(text: &str) -> Option<usize> {
    for (offset, character) in text[2..].char_indices() {
        let offset = offset + 2;
        if matches!(character, '\u{7}' | '\u{9c}') {
            return Some(offset + character.len_utf8());
        }
        if character == '\u{1b}' && text.as_bytes().get(offset + 1) == Some(&b'\\') {
            return Some(offset + 2);
        }
    }
    None
}

fn csi_end(text: &str) -> Option<usize> {
    for base in csi_bases(text) {
        for end in parameter_ends(text, base).into_iter().rev() {
            if text.as_bytes().get(end).is_some_and(|byte| {
                matches!(byte, b'0'..=b'9' | b'A'..=b'P' | b'R'..=b'T' | b'Z' | b'c' | b'f'..=b'n' | b'q'..=b'u' | b'y' | b'=' | b'>' | b'<' | b'~')
            }) {
                return Some(end + 1);
            }
        }
    }
    None
}

fn csi_bases(text: &str) -> Vec<usize> {
    let intro = if text.starts_with('\u{1b}') {
        1
    } else if text.starts_with('\u{9b}') {
        '\u{9b}'.len_utf8()
    } else {
        return Vec::new();
    };
    let bytes = text.as_bytes();
    let mut bases = Vec::new();
    let private = if intro == 1 && bytes.get(1) == Some(&b'[') {
        Some(2)
    } else if intro > 1 {
        Some(intro)
    } else {
        None
    };
    if let Some(private) = private
        && bytes
            .get(private)
            .is_some_and(|byte| matches!(byte, b'<' | b'=' | b'>'))
    {
        bases.push(private + 1);
    }
    let mut end = intro;
    while bytes
        .get(end)
        .is_some_and(|byte| matches!(byte, b'[' | b']' | b'(' | b')' | b'#' | b';' | b'?'))
    {
        end += 1;
    }
    // The intermediate sequence is greedy but can backtrack for a final byte.
    bases.extend((intro..=end).rev());
    bases
}

fn parameter_ends(text: &str, start: usize) -> Vec<usize> {
    // (digits{1,4} ([;:] digits{0,4})*)? -- retain the backtracking positions
    // because digits are also accepted as final bytes by Pi's ANSI regex.
    let bytes = text.as_bytes();
    let mut ends = vec![start];
    let mut offset = start;
    loop {
        for _ in 0..4 {
            if !bytes.get(offset).is_some_and(u8::is_ascii_digit) {
                break;
            }
            offset += 1;
            ends.push(offset);
        }
        if offset == start
            || !bytes
                .get(offset)
                .is_some_and(|byte| matches!(byte, b';' | b':'))
        {
            break;
        }
        offset += 1;
        ends.push(offset);
    }
    ends
}
