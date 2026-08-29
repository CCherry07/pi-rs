use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use url::Url;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl AwsCredentials {
    pub fn validate(&self) -> Result<(), String> {
        if self.access_key_id.trim().is_empty()
            || self.secret_access_key.trim().is_empty()
            || self.access_key_id.contains(['\r', '\n'])
            || self.secret_access_key.contains(['\r', '\n'])
            || self
                .session_token
                .as_deref()
                .is_some_and(|token| token.trim().is_empty() || token.contains(['\r', '\n']))
        {
            return Err("invalid AWS credentials".to_string());
        }
        Ok(())
    }
}

pub fn sign_request(
    headers: &mut BTreeMap<String, String>,
    endpoint: &str,
    payload: &[u8],
    region: &str,
    credentials: &AwsCredentials,
    now: OffsetDateTime,
) -> Result<(), String> {
    credentials.validate()?;
    if region.trim().is_empty() || region.contains(['/', '\r', '\n']) {
        return Err("invalid AWS region".to_string());
    }
    let url = Url::parse(endpoint).map_err(|error| format!("invalid Bedrock endpoint: {error}"))?;
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err("Bedrock endpoint must use HTTP or HTTPS".to_string());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "Bedrock endpoint has no host".to_string())?;
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let payload_hash = sha256_hex(payload);
    let date = format!(
        "{:04}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day()
    );
    let amz_date = format!(
        "{date}T{:02}{:02}{:02}Z",
        now.hour(),
        now.minute(),
        now.second()
    );

    insert_header(headers, "Host", host);
    insert_header(headers, "x-amz-content-sha256", payload_hash.clone());
    insert_header(headers, "x-amz-date", amz_date.clone());
    if let Some(token) = &credentials.session_token {
        insert_header(headers, "x-amz-security-token", token.clone());
    } else {
        remove_header(headers, "x-amz-security-token");
    }
    remove_header(headers, "Authorization");

    let signed_names = [
        "content-type",
        "host",
        "x-amz-content-sha256",
        "x-amz-date",
        "x-amz-security-token",
        "x-amzn-bedrock-accept",
    ];
    let mut canonical_headers = BTreeMap::new();
    for (name, value) in headers.iter() {
        let lower = name.to_ascii_lowercase();
        if signed_names.contains(&lower.as_str()) {
            canonical_headers.insert(lower, normalize_header_value(value));
        }
    }
    for required in ["content-type", "host", "x-amz-content-sha256", "x-amz-date"] {
        if !canonical_headers.contains_key(required) {
            return Err(format!("missing required SigV4 header {required}"));
        }
    }
    let signed_headers = canonical_headers
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers = canonical_headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let canonical_request = format!(
        "POST\n{}\n{}\n{}\n{}\n{}",
        canonical_path(url.path()),
        canonical_query(&url),
        canonical_headers,
        signed_headers,
        payload_hash
    );
    let scope = format!("{date}/{region}/bedrock/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let date_key = hmac(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        date.as_bytes(),
    )?;
    let region_key = hmac(&date_key, region.as_bytes())?;
    let service_key = hmac(&region_key, b"bedrock")?;
    let signing_key = hmac(&service_key, b"aws4_request")?;
    let signature = hex::encode(hmac(&signing_key, string_to_sign.as_bytes())?);
    insert_header(
        headers,
        "Authorization",
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            credentials.access_key_id
        ),
    );
    Ok(())
}

fn canonical_path(path: &str) -> String {
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            segment => segments.push(segment),
        }
    }
    let mut normalized = String::new();
    if path.starts_with('/') {
        normalized.push('/');
    }
    normalized.push_str(&segments.join("/"));
    if !segments.is_empty() && path.ends_with('/') {
        normalized.push('/');
    }
    aws_encode(&normalized).replace("%2F", "/")
}

fn canonical_query(url: &Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(name, value)| (aws_encode(&name), aws_encode(&value)))
        .collect::<Vec<_>>();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn aws_encode(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn sha256_hex(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn hmac(key: &[u8], value: &[u8]) -> Result<Vec<u8>, String> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|_| "failed to initialize AWS SigV4 HMAC".to_string())?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn normalize_header_value(value: &str) -> String {
    value.split_ascii_whitespace().collect::<Vec<_>>().join(" ")
}

fn insert_header(
    headers: &mut BTreeMap<String, String>,
    name: impl AsRef<str>,
    value: impl Into<String>,
) {
    remove_header(headers, name.as_ref());
    headers.insert(name.as_ref().to_string(), value.into());
}

fn remove_header(headers: &mut BTreeMap<String, String>, name: &str) {
    if let Some(existing) = headers
        .keys()
        .find(|existing| existing.eq_ignore_ascii_case(name))
        .cloned()
    {
        headers.remove(&existing);
    }
}

#[cfg(test)]
mod tests {
    use time::macros::datetime;

    use super::*;

    #[test]
    fn signs_bedrock_request_with_session_credentials() {
        let mut headers = BTreeMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "x-amzn-bedrock-accept".to_string(),
                "application/vnd.amazon.eventstream".to_string(),
            ),
        ]);
        sign_request(
            &mut headers,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/test/converse-stream",
            br#"{"messages":[]}"#,
            "us-east-1",
            &AwsCredentials {
                access_key_id: "AKIDEXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                session_token: Some("session-token".to_string()),
            },
            datetime!(2015-08-30 12:36:00 UTC),
        )
        .unwrap();
        assert_eq!(headers["x-amz-date"], "20150830T123600Z");
        assert_eq!(
            headers["x-amz-content-sha256"],
            "5e4ce7b36ba37b78a5d5f9fd08e6b7b54ba6879d651aa46ec9e1d6fa24ebe30a"
        );
        assert_eq!(
            headers["Authorization"],
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token;x-amzn-bedrock-accept, Signature=9ccb8a16ddbb035fd83919e119b758bd7378e33ef7e802801b3f3cf7c7ce61b4"
        );

        let mut encoded_headers = BTreeMap::from([
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "x-amzn-bedrock-accept".to_string(),
                "application/vnd.amazon.eventstream".to_string(),
            ),
        ]);
        sign_request(
            &mut encoded_headers,
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/openai.gpt-oss-120b-1%3A0/converse-stream",
            br#"{"messages":[]}"#,
            "us-east-1",
            &AwsCredentials {
                access_key_id: "AKIDEXAMPLE".to_string(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
                session_token: Some("session-token".to_string()),
            },
            datetime!(2015-08-30 12:36:00 UTC),
        )
        .unwrap();
        assert_eq!(
            encoded_headers["Authorization"],
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/bedrock/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date;x-amz-security-token;x-amzn-bedrock-accept, Signature=ccc75875b2dee6b375618a05b4890cee26c8d6f101bc93bf74ed6c460b2bf7aa"
        );
    }
}
