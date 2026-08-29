use std::collections::BTreeMap;

use crc32fast::hash;

const PRELUDE_BYTES: usize = 12;
const TRAILER_BYTES: usize = 4;
const MIN_MESSAGE_BYTES: usize = PRELUDE_BYTES + TRAILER_BYTES;
const MAX_MESSAGE_BYTES: usize = 16 * 1_024 * 1_024;

#[derive(Debug)]
pub struct EventFrame {
    pub headers: BTreeMap<String, String>,
    pub payload: Vec<u8>,
}

#[derive(Default)]
pub struct EventStreamDecoder {
    buffer: Vec<u8>,
}

impl EventStreamDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<EventFrame>, String> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_MESSAGE_BYTES * 2 {
            return Err("Bedrock event stream buffer exceeded its limit".to_string());
        }
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            if self.buffer.len() < PRELUDE_BYTES {
                break;
            }
            let total_len = read_u32(&self.buffer[0..4]) as usize;
            let headers_len = read_u32(&self.buffer[4..8]) as usize;
            if !(MIN_MESSAGE_BYTES..=MAX_MESSAGE_BYTES).contains(&total_len) {
                return Err(format!(
                    "invalid Bedrock event stream frame length {total_len}"
                ));
            }
            if headers_len > total_len.saturating_sub(MIN_MESSAGE_BYTES) {
                return Err("invalid Bedrock event stream header length".to_string());
            }
            if hash(&self.buffer[0..8]) != read_u32(&self.buffer[8..12]) {
                return Err("Bedrock event stream prelude CRC mismatch".to_string());
            }
            if self.buffer.len() < total_len {
                break;
            }
            let expected_crc = read_u32(&self.buffer[total_len - TRAILER_BYTES..total_len]);
            if hash(&self.buffer[..total_len - TRAILER_BYTES]) != expected_crc {
                return Err("Bedrock event stream message CRC mismatch".to_string());
            }
            let headers_end = PRELUDE_BYTES + headers_len;
            let headers = parse_headers(&self.buffer[PRELUDE_BYTES..headers_end])?;
            let payload = self.buffer[headers_end..total_len - TRAILER_BYTES].to_vec();
            self.buffer.drain(..total_len);
            frames.push(EventFrame { headers, payload });
        }
        Ok(frames)
    }

    pub fn finish(&self) -> Result<(), String> {
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err("Bedrock event stream ended with a partial frame".to_string())
        }
    }
}

fn parse_headers(bytes: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let mut headers = BTreeMap::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let name_len = *bytes
            .get(offset)
            .ok_or_else(|| "truncated Bedrock event header".to_string())?
            as usize;
        offset += 1;
        if name_len == 0 || offset + name_len + 1 > bytes.len() {
            return Err("invalid Bedrock event header name".to_string());
        }
        let name = std::str::from_utf8(&bytes[offset..offset + name_len])
            .map_err(|_| "non-UTF-8 Bedrock event header name".to_string())?
            .to_string();
        offset += name_len;
        let kind = bytes[offset];
        offset += 1;
        let value = match kind {
            0 => "true".to_string(),
            1 => "false".to_string(),
            2 => take(bytes, &mut offset, 1)?[0].to_string(),
            3 => i16::from_be_bytes(take_array(bytes, &mut offset)?).to_string(),
            4 => i32::from_be_bytes(take_array(bytes, &mut offset)?).to_string(),
            5 | 8 => i64::from_be_bytes(take_array(bytes, &mut offset)?).to_string(),
            6 => {
                let len = read_len(bytes, &mut offset)?;
                let value = take(bytes, &mut offset, len)?;
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value)
            }
            7 => {
                let len = read_len(bytes, &mut offset)?;
                std::str::from_utf8(take(bytes, &mut offset, len)?)
                    .map_err(|_| "non-UTF-8 Bedrock event header value".to_string())?
                    .to_string()
            }
            9 => hex::encode(take(bytes, &mut offset, 16)?),
            other => return Err(format!("unknown Bedrock event header type {other}")),
        };
        headers.insert(name, value);
    }
    Ok(headers)
}

fn read_len(bytes: &[u8], offset: &mut usize) -> Result<usize, String> {
    Ok(u16::from_be_bytes(take_array(bytes, offset)?) as usize)
}

fn take<'a>(bytes: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], String> {
    let end = offset
        .checked_add(len)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| "truncated Bedrock event header value".to_string())?;
    let value = &bytes[*offset..end];
    *offset = end;
    Ok(value)
}

fn take_array<const N: usize>(bytes: &[u8], offset: &mut usize) -> Result<[u8; N], String> {
    take(bytes, offset, N)?
        .try_into()
        .map_err(|_| "truncated Bedrock event header value".to_string())
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(
        bytes
            .try_into()
            .expect("event stream u32 slice has an exact length"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
        let mut headers = Vec::new();
        for (name, value) in [
            (":message-type", "event"),
            (":event-type", event_type),
            (":content-type", "application/json"),
        ] {
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
            headers.extend_from_slice(value.as_bytes());
        }
        let total_len = PRELUDE_BYTES + headers.len() + payload.len() + TRAILER_BYTES;
        let mut output = Vec::new();
        output.extend_from_slice(&(total_len as u32).to_be_bytes());
        output.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        output.extend_from_slice(&hash(&output).to_be_bytes());
        output.extend_from_slice(&headers);
        output.extend_from_slice(payload);
        output.extend_from_slice(&hash(&output).to_be_bytes());
        output
    }

    #[test]
    fn decodes_chunked_eventstream_frames_and_validates_crc() {
        let bytes = frame("messageStart", br#"{"role":"assistant"}"#);
        let mut decoder = EventStreamDecoder::default();
        assert!(decoder.push(&bytes[..7]).unwrap().is_empty());
        let frames = decoder.push(&bytes[7..]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].headers[":message-type"], "event");
        assert_eq!(frames[0].headers[":event-type"], "messageStart");
        assert_eq!(frames[0].payload, br#"{"role":"assistant"}"#);
        decoder.finish().unwrap();

        let mut corrupt = bytes;
        let index = corrupt.len() - 5;
        corrupt[index] ^= 1;
        assert!(EventStreamDecoder::default().push(&corrupt).is_err());
    }
}
