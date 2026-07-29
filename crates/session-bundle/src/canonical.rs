//! Deterministic JSON encoding used by Session Bundle v1.
//!
//! This is deliberately a small, format-local canonicalization scheme rather
//! than an implementation of RFC 8785.  Object keys are sorted recursively,
//! JSON is compact UTF-8, and the document has exactly one trailing line feed.

use std::fmt;

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

/// A canonical JSON encoding or decoding failure.
#[derive(Debug)]
pub enum CanonicalJsonError {
    /// The value could not be represented as JSON or the input was not JSON.
    Json(serde_json::Error),
    /// The input was valid JSON, but its bytes were not the canonical encoding.
    NonCanonical,
}

impl fmt::Display for CanonicalJsonError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "invalid JSON: {error}"),
            Self::NonCanonical => formatter.write_str("JSON is not canonically encoded"),
        }
    }
}

impl std::error::Error for CanonicalJsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::NonCanonical => None,
        }
    }
}

impl From<serde_json::Error> for CanonicalJsonError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Encode a serializable value using the Session Bundle canonical JSON form.
pub fn to_canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalJsonError> {
    let value = serde_json::to_value(value)?;
    let mut output = Vec::new();
    write_value(&value, &mut output)?;
    output.push(b'\n');
    Ok(output)
}

/// Decode a typed value and require byte-for-byte canonical encoding.
///
/// Re-encoding the typed value also makes ignored unknown fields, duplicate
/// keys, alternate number spellings, and non-canonical whitespace fail closed.
pub fn from_canonical_slice<T>(input: &[u8]) -> Result<T, CanonicalJsonError>
where
    T: DeserializeOwned + Serialize,
{
    let value: T = serde_json::from_slice(input)?;
    let canonical = to_canonical_bytes(&value)?;
    if canonical != input {
        return Err(CanonicalJsonError::NonCanonical);
    }
    Ok(value)
}

/// Parse an untyped JSON document and require byte-for-byte canonical encoding.
pub fn verify_canonical_slice(input: &[u8]) -> Result<Value, CanonicalJsonError> {
    from_canonical_slice(input)
}

fn write_value(value: &Value, output: &mut Vec<u8>) -> Result<(), serde_json::Error> {
    match value {
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_value(value, output)?;
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                serde_json::to_writer(&mut *output, key)?;
                output.push(b':');
                // `key` came from the map immediately above.
                write_value(&values[key], output)?;
            }
            output.push(b'}');
        }
        _ => serde_json::to_writer(output, value)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    #[test]
    fn recursively_sorts_keys_and_appends_one_line_feed() {
        let value = json!({
            "z": {"b": 2, "a": 1},
            "a": [{"d": 4, "c": 3}, "line\nfeed"]
        });

        let encoded = to_canonical_bytes(&value).expect("encode canonical JSON");

        assert_eq!(
            encoded,
            br#"{"a":[{"c":3,"d":4},"line\nfeed"],"z":{"a":1,"b":2}}
"#
        );
        assert_eq!(encoded.last(), Some(&b'\n'));
        assert_ne!(encoded.get(encoded.len().saturating_sub(2)), Some(&b'\n'));
    }

    #[test]
    fn canonical_round_trip_succeeds() {
        let encoded = br#"{"a":1,"b":[true,null]}
"#;
        let value = verify_canonical_slice(encoded).expect("canonical JSON");
        assert_eq!(value, json!({"a": 1, "b": [true, null]}));
    }

    #[test]
    fn whitespace_unsorted_keys_and_missing_line_feed_fail() {
        for input in [
            br#"{"b":2,"a":1}
"#
            .as_slice(),
            br#"{"a": 1}
"#
            .as_slice(),
            br#"{"a":1}"#.as_slice(),
            br#"{"a":1}

"#
            .as_slice(),
        ] {
            assert!(matches!(
                verify_canonical_slice(input),
                Err(CanonicalJsonError::NonCanonical)
            ));
        }
    }

    #[derive(Debug, Deserialize, PartialEq, Serialize)]
    struct KnownFields {
        a: u64,
    }

    #[test]
    fn typed_decode_rejects_ignored_unknown_fields() {
        let input = br#"{"a":1,"unknown":2}
"#;
        assert!(matches!(
            from_canonical_slice::<KnownFields>(input),
            Err(CanonicalJsonError::NonCanonical)
        ));
    }

    #[test]
    fn duplicate_keys_fail_canonical_check() {
        let input = br#"{"a":1,"a":2}
"#;
        assert!(matches!(
            verify_canonical_slice(input),
            Err(CanonicalJsonError::NonCanonical)
        ));
    }
}
