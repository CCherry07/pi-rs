//! Stack-safe JSON decoding and encoding for explicitly deep wire boundaries.
//!
//! Most product JSON continues to use serde_json's default recursion guard. Callers that
//! intentionally accept deeper values must opt into these helpers with a finite structural depth.

use serde::{Deserialize, Serialize};

/// Decodes JSON after rejecting documents whose object/array nesting exceeds `max_depth`.
///
/// The explicit structural bound is checked before serde's recursion guard is disabled. The
/// stacker adapter then moves recursive deserialization frames off the caller's finite native
/// stack. A root object or array has depth one; a scalar has depth zero.
pub fn from_slice_with_max_depth<'de, T>(
    input: &'de [u8],
    max_depth: usize,
) -> Result<T, serde_json::Error>
where
    T: Deserialize<'de>,
{
    check_depth(input, max_depth)?;
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    deserializer.disable_recursion_limit();
    let value = T::deserialize(serde_stacker::Deserializer::new(&mut deserializer))?;
    deserializer.end()?;
    Ok(value)
}

/// String variant of [`from_slice_with_max_depth`].
pub fn from_str_with_max_depth<'de, T>(
    input: &'de str,
    max_depth: usize,
) -> Result<T, serde_json::Error>
where
    T: Deserialize<'de>,
{
    from_slice_with_max_depth(input.as_bytes(), max_depth)
}

/// Encodes JSON through a stack-growing serializer and rejects output beyond `max_depth`.
///
/// Encoding precedes the structural check because serde values do not expose a common tree API.
/// Recursive serializer frames are stack-safe during that pass, and callers still receive no
/// over-limit document.
pub fn to_vec_with_max_depth<T>(value: &T, max_depth: usize) -> Result<Vec<u8>, serde_json::Error>
where
    T: ?Sized + Serialize,
{
    let mut encoded = Vec::new();
    let mut serializer = serde_json::Serializer::new(&mut encoded);
    value.serialize(serde_stacker::Serializer::new(&mut serializer))?;
    check_depth(&encoded, max_depth)?;
    Ok(encoded)
}

/// String variant of [`to_vec_with_max_depth`].
pub fn to_string_with_max_depth<T>(value: &T, max_depth: usize) -> Result<String, serde_json::Error>
where
    T: ?Sized + Serialize,
{
    // serde_json emits UTF-8 by construction.
    Ok(String::from_utf8(to_vec_with_max_depth(value, max_depth)?).expect("JSON is UTF-8"))
}

/// Converts a serializable value through the same bounded, stack-safe JSON seam.
pub fn to_value_with_max_depth<T>(
    value: &T,
    max_depth: usize,
) -> Result<serde_json::Value, serde_json::Error>
where
    T: ?Sized + Serialize,
{
    let encoded = to_vec_with_max_depth(value, max_depth)?;
    from_slice_with_max_depth(&encoded, max_depth)
}

fn check_depth(input: &[u8], max_depth: usize) -> Result<(), serde_json::Error> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in input {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > max_depth {
                    return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                        "JSON nesting depth exceeds the limit of {max_depth}"
                    )));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn nested_json(depth: usize) -> String {
        format!("{}0{}", "[".repeat(depth), "]".repeat(depth))
    }

    #[test]
    fn accepts_the_exact_bound_and_rejects_the_next_container() {
        let accepted = nested_json(512);
        let value: Value = from_str_with_max_depth(&accepted, 512).unwrap();
        assert_eq!(to_string_with_max_depth(&value, 512).unwrap(), accepted);

        let error = from_str_with_max_depth::<Value>(&nested_json(513), 512).unwrap_err();
        assert!(error.to_string().contains("limit of 512"));
        assert!(to_string_with_max_depth(&value, 511).is_err());
    }

    #[test]
    fn strings_and_malformed_prefixes_cannot_confuse_the_depth_scan() {
        let value: Value = from_str_with_max_depth(r#"{"text":"[[[{{{\"}"}"#, 1).unwrap();
        assert_eq!(value, json!({"text": "[[[{{{\"}"}));
        let scalar: Value = from_str_with_max_depth(r#""[{}]""#, 0).unwrap();
        assert_eq!(scalar, "[{}]");

        // An unmatched closer never wraps around the unsigned depth counter. The document is
        // still rejected even when followed by an otherwise deeply nested value.
        let malformed = format!("]{}", nested_json(600));
        assert!(from_str_with_max_depth::<Value>(&malformed, 512).is_err());
    }
}
