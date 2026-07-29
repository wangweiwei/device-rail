use serde::{Deserialize, Deserializer, Serializer};
use serde_json::Value;

/// Largest integer that round-trips through JavaScript and JSON `number`
/// implementations without loss.
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Interprets a JSON number using JSON Schema's mathematical `integer`
/// semantics and converts it to `u32` when it is finite and in range.
///
/// JSON Schema treats `1`, `1.0`, and `1e0` as the same integer instance.
/// `serde_json` deliberately keeps the latter representations as floating
/// point numbers, so relying only on [`Value::as_u64`] would make a Driver's
/// runtime parser narrower than its advertised schema.
pub fn json_integer_as_u32(value: &Value) -> Option<u32> {
    if let Some(integer) = value.as_u64() {
        return u32::try_from(integer).ok();
    }
    let float = value.as_f64()?;
    if float.is_finite() && float >= 0.0 && float <= f64::from(u32::MAX) && float.fract() == 0.0 {
        let integer = float as u32;
        (f64::from(integer) == float).then_some(integer)
    } else {
        None
    }
}

/// Interprets a JSON number using JSON Schema's mathematical `integer`
/// semantics and converts it to `i32` when it is finite and in range.
pub fn json_integer_as_i32(value: &Value) -> Option<i32> {
    if let Some(integer) = value.as_i64() {
        return i32::try_from(integer).ok();
    }
    if let Some(integer) = value.as_u64() {
        return i32::try_from(integer).ok();
    }
    let float = value.as_f64()?;
    if float.is_finite()
        && float >= f64::from(i32::MIN)
        && float <= f64::from(i32::MAX)
        && float.fract() == 0.0
    {
        let integer = float as i32;
        (f64::from(integer) == float).then_some(integer)
    } else {
        None
    }
}

pub fn serialize_js_safe_u64<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value <= MAX_SAFE_INTEGER {
        serializer.serialize_u64(*value)
    } else {
        Err(serde::ser::Error::custom(format!(
            "integer exceeds the cross-language safe limit {MAX_SAFE_INTEGER}"
        )))
    }
}

pub fn deserialize_js_safe_u64<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u64::deserialize(deserializer)?;
    if value <= MAX_SAFE_INTEGER {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(format!(
            "integer exceeds the cross-language safe limit {MAX_SAFE_INTEGER}"
        )))
    }
}

pub fn serialize_optional_js_safe_u64<S>(
    value: &Option<u64>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) if *value <= MAX_SAFE_INTEGER => serializer.serialize_some(value),
        Some(_) => Err(serde::ser::Error::custom(format!(
            "integer exceeds the cross-language safe limit {MAX_SAFE_INTEGER}"
        ))),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize_optional_js_safe_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u64>::deserialize(deserializer)?;
    match value {
        Some(value) if value > MAX_SAFE_INTEGER => Err(serde::de::Error::custom(format!(
            "integer exceeds the cross-language safe limit {MAX_SAFE_INTEGER}"
        ))),
        value => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    use super::{MAX_SAFE_INTEGER, json_integer_as_i32, json_integer_as_u32};

    #[derive(Debug, Deserialize, Serialize)]
    struct WireInteger {
        #[serde(
            serialize_with = "super::serialize_js_safe_u64",
            deserialize_with = "super::deserialize_js_safe_u64"
        )]
        value: u64,
    }

    #[test]
    fn wire_integer_accepts_the_limit_and_rejects_larger_values() {
        let valid: WireInteger =
            serde_json::from_value(json!({ "value": MAX_SAFE_INTEGER })).expect("safe integer");
        assert_eq!(valid.value, MAX_SAFE_INTEGER);
        assert!(
            serde_json::from_value::<WireInteger>(json!({ "value": MAX_SAFE_INTEGER + 1 }))
                .is_err()
        );
        assert!(
            serde_json::to_value(WireInteger {
                value: MAX_SAFE_INTEGER + 1,
            })
            .is_err()
        );
    }

    #[test]
    fn json_schema_integer_forms_convert_without_widening_the_range() {
        for representation in ["1", "1.0", "1e0", "-0.0"] {
            let value: Value = serde_json::from_str(representation).expect("JSON number");
            assert_eq!(
                json_integer_as_u32(&value),
                Some(if representation == "-0.0" { 0 } else { 1 })
            );
        }
        for representation in ["-1", "-1.0", "-1e0"] {
            let value: Value = serde_json::from_str(representation).expect("JSON number");
            assert_eq!(json_integer_as_i32(&value), Some(-1));
        }

        for representation in ["1.5", "-1", "4294967296", "4294967296.0"] {
            let value: Value = serde_json::from_str(representation).expect("JSON number");
            assert_eq!(json_integer_as_u32(&value), None);
        }
        for representation in ["1.5", "2147483648", "-2147483649", "2147483648.0"] {
            let value: Value = serde_json::from_str(representation).expect("JSON number");
            assert_eq!(json_integer_as_i32(&value), None);
        }
        assert_eq!(json_integer_as_u32(&json!("1")), None);
        assert_eq!(json_integer_as_i32(&Value::Null), None);
    }
}
