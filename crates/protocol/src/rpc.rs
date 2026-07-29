use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Visitor, ser::Error as _};
use serde_json::{Map, Value};

use crate::ErrorInfo;

pub const MAX_SAFE_INTEGER_ID: u64 = crate::MAX_SAFE_INTEGER;

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum JsonRpcVersion {
    #[serde(rename = "2.0")]
    V2,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(with = "RpcIdSchema"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RpcId {
    String(String),
    Number(u64),
}

/// A positive timeout in milliseconds that is safe to represent as a JSON
/// number in every supported client language.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct RequestTimeoutMs(
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 1_u64, max = 9_007_199_254_740_991_u64))
    )]
    u64,
);

impl RequestTimeoutMs {
    pub const MIN: u64 = 1;
    pub const MAX: u64 = crate::MAX_SAFE_INTEGER;

    pub const fn new(value: u64) -> Option<Self> {
        if value >= Self::MIN && value <= Self::MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for RequestTimeoutMs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "timeout must be between {} and {} milliseconds",
                Self::MIN,
                Self::MAX
            ))
        })
    }
}

pub(crate) fn deserialize_optional_timeout<'de, D>(
    deserializer: D,
) -> Result<Option<RequestTimeoutMs>, D::Error>
where
    D: Deserializer<'de>,
{
    RequestTimeoutMs::deserialize(deserializer).map(Some)
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(untagged)]
enum RpcIdSchema {
    String(String),
    Number(#[schemars(range(max = 9_007_199_254_740_991_u64))] u64),
}

impl Serialize for RpcId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::String(value) => serializer.serialize_str(value),
            Self::Number(value) if *value <= MAX_SAFE_INTEGER_ID => {
                serializer.serialize_u64(*value)
            }
            Self::Number(_) => Err(S::Error::custom(
                "numeric request id exceeds the JavaScript safe integer limit",
            )),
        }
    }
}

impl<'de> Deserialize<'de> for RpcId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RpcIdVisitor;

        impl<'de> Visitor<'de> for RpcIdVisitor {
            type Value = RpcId;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(
                    formatter,
                    "a string or a non-negative integer no larger than {MAX_SAFE_INTEGER_ID}"
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(RpcId::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(RpcId::String(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value <= MAX_SAFE_INTEGER_ID {
                    Ok(RpcId::Number(value))
                } else {
                    Err(E::custom(
                        "numeric request id exceeds the JavaScript safe integer limit",
                    ))
                }
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let value = u64::try_from(value)
                    .map_err(|_| E::custom("numeric request id must be non-negative"))?;
                self.visit_u64(value)
            }

            fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Err(E::custom("numeric request id must be an integer"))
            }
        }

        deserializer.deserialize_any(RpcIdVisitor)
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RpcParams {
    Object(Map<String, Value>),
    Array(Vec<Value>),
}

impl fmt::Debug for RpcParams {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object(values) => formatter
                .debug_struct("RpcParams::Object")
                .field("field_count", &values.len())
                .finish(),
            Self::Array(values) => formatter
                .debug_struct("RpcParams::Array")
                .field("item_count", &values.len())
                .finish(),
        }
    }
}

impl RpcParams {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Object(values) => values.is_empty(),
            Self::Array(values) => values.is_empty(),
        }
    }

    pub fn into_value(self) -> Value {
        match self {
            Self::Object(values) => Value::Object(values),
            Self::Array(values) => Value::Array(values),
        }
    }
}

fn deserialize_optional_params<'de, D>(deserializer: D) -> Result<Option<RpcParams>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Object(values) => Ok(Some(RpcParams::Object(values))),
        Value::Array(values) => Ok(Some(RpcParams::Array(values))),
        _ => Err(serde::de::Error::custom(
            "params must be an object or array when present",
        )),
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcRequest {
    pub jsonrpc: JsonRpcVersion,
    pub id: RpcId,
    pub method: String,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_timeout",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "schema", schemars(with = "RequestTimeoutMs"))]
    pub timeout_ms: Option<RequestTimeoutMs>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_params",
        skip_serializing_if = "Option::is_none"
    )]
    #[cfg_attr(feature = "schema", schemars(with = "RpcParams"))]
    pub params: Option<RpcParams>,
}

impl fmt::Debug for RpcRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RpcRequest")
            .field("jsonrpc", &self.jsonrpc)
            .field("id", &self.id)
            .field("method", &self.method)
            .field("timeout_ms", &self.timeout_ms)
            .field("has_params", &self.params.is_some())
            .finish()
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestCancelParams {
    pub request_id: RpcId,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RequestCancelStatus {
    Requested,
    AlreadyRequested,
    NotFound,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestCancelResult {
    pub request_id: RpcId,
    pub status: RequestCancelStatus,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RpcError {
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = -2_147_483_648_i64, max = 2_147_483_647_i64))
    )]
    pub code: i32,
    pub message: String,
    pub data: ErrorInfo,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(with = "RpcResponseSchema"))]
#[derive(Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RpcResponse {
    Success {
        jsonrpc: JsonRpcVersion,
        id: RpcId,
        result: Value,
    },
    Failure {
        jsonrpc: JsonRpcVersion,
        id: Option<RpcId>,
        error: RpcError,
    },
}

impl fmt::Debug for RpcResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success { jsonrpc, id, .. } => formatter
                .debug_struct("RpcResponse::Success")
                .field("jsonrpc", jsonrpc)
                .field("id", id)
                .field("result", &"<redacted>")
                .finish(),
            Self::Failure { jsonrpc, id, error } => formatter
                .debug_struct("RpcResponse::Failure")
                .field("jsonrpc", jsonrpc)
                .field("id", id)
                .field("rpc_code", &error.code)
                .field("error_code", &error.data.code)
                .finish(),
        }
    }
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(untagged)]
enum RpcResponseSchema {
    Success(RpcSuccessSchema),
    Failure(RpcFailureSchema),
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RpcSuccessSchema {
    jsonrpc: JsonRpcVersion,
    id: RpcId,
    result: Value,
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RpcFailureSchema {
    jsonrpc: JsonRpcVersion,
    id: NullableRpcIdSchema,
    error: RpcError,
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(untagged)]
enum NullableRpcIdSchema {
    Id(RpcId),
    Null(()),
}

impl<'de> Deserialize<'de> for RpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct SuccessWire {
            jsonrpc: JsonRpcVersion,
            id: RpcId,
            result: Value,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum NullableRpcId {
            Id(RpcId),
            Null(()),
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct FailureWire {
            jsonrpc: JsonRpcVersion,
            id: NullableRpcId,
            error: RpcError,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ResponseWire {
            Success(SuccessWire),
            Failure(FailureWire),
        }

        match ResponseWire::deserialize(deserializer)? {
            ResponseWire::Success(response) => Ok(Self::Success {
                jsonrpc: response.jsonrpc,
                id: response.id,
                result: response.result,
            }),
            ResponseWire::Failure(response) => Ok(Self::Failure {
                jsonrpc: response.jsonrpc,
                id: match response.id {
                    NullableRpcId::Id(id) => Some(id),
                    NullableRpcId::Null(()) => None,
                },
                error: response.error,
            }),
        }
    }
}

impl RpcResponse {
    pub fn success(id: RpcId, result: Value) -> Self {
        Self::Success {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result,
        }
    }

    pub fn failure(id: Option<RpcId>, error: RpcError) -> Self {
        Self::Failure {
            jsonrpc: JsonRpcVersion::V2,
            id,
            error,
        }
    }

    pub fn result(&self) -> Option<&Value> {
        match self {
            Self::Success { result, .. } => Some(result),
            Self::Failure { .. } => None,
        }
    }

    pub fn error(&self) -> Option<&RpcError> {
        match self {
            Self::Success { .. } => None,
            Self::Failure { error, .. } => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        JsonRpcVersion, MAX_SAFE_INTEGER_ID, RequestCancelParams, RequestCancelResult,
        RequestCancelStatus, RequestTimeoutMs, RpcId, RpcParams, RpcRequest, RpcResponse,
    };

    #[test]
    fn request_requires_json_rpc_two_and_a_safe_id() {
        let request: RpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": MAX_SAFE_INTEGER_ID,
            "method": "system.hello"
        }))
        .expect("valid request");
        assert_eq!(request.jsonrpc, JsonRpcVersion::V2);
        assert_eq!(request.id, RpcId::Number(MAX_SAFE_INTEGER_ID));
        assert!(request.params.is_none());

        for invalid in [
            json!({ "jsonrpc": "1.0", "id": 1, "method": "x" }),
            json!({ "jsonrpc": "2.0", "id": null, "method": "x" }),
            json!({ "jsonrpc": "2.0", "id": -1, "method": "x" }),
            json!({ "jsonrpc": "2.0", "id": 1.5, "method": "x" }),
            json!({ "jsonrpc": "2.0", "id": MAX_SAFE_INTEGER_ID + 1, "method": "x" }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "x", "params": null }),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "x", "params": 42 }),
        ] {
            assert!(serde_json::from_value::<RpcRequest>(invalid).is_err());
        }

        assert!(serde_json::to_value(RpcId::Number(MAX_SAFE_INTEGER_ID + 1)).is_err());
    }

    #[test]
    fn request_timeout_accepts_only_positive_safe_integers() {
        let request: RpcRequest = serde_json::from_value(json!({
            "jsonrpc": "2.0",
            "id": "bounded",
            "method": "device.observe",
            "timeoutMs": RequestTimeoutMs::MAX
        }))
        .expect("maximum safe timeout");
        assert_eq!(
            request.timeout_ms.map(RequestTimeoutMs::get),
            Some(RequestTimeoutMs::MAX)
        );

        for timeout in [json!(null), json!(0), json!(RequestTimeoutMs::MAX + 1)] {
            let invalid = json!({
                "jsonrpc": "2.0",
                "id": "invalid-timeout",
                "method": "device.observe",
                "timeoutMs": timeout
            });
            assert!(serde_json::from_value::<RpcRequest>(invalid).is_err());
        }
        assert!(RequestTimeoutMs::new(0).is_none());
        assert!(RequestTimeoutMs::new(RequestTimeoutMs::MAX + 1).is_none());
    }

    #[test]
    fn old_request_json_round_trips_without_new_fields() {
        let old = json!({
            "jsonrpc": "2.0",
            "id": "old-client",
            "method": "device.execute",
            "params": {
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "tap",
                "arguments": { "x": 10, "y": 20 }
            }
        });
        let request: RpcRequest = serde_json::from_value(old.clone()).expect("old request");
        assert!(request.timeout_ms.is_none());
        assert_eq!(
            serde_json::to_value(request).expect("serialize old request"),
            old
        );

        let unknown = json!({
            "jsonrpc": "2.0",
            "id": "strict",
            "method": "device.observe",
            "deadlineMs": 100
        });
        assert!(serde_json::from_value::<RpcRequest>(unknown).is_err());
    }

    #[test]
    fn cancellation_models_are_strict_and_use_camel_case_statuses() {
        let params: RequestCancelParams = serde_json::from_value(json!({
            "requestId": "execute-1"
        }))
        .expect("cancel params");
        let result = RequestCancelResult {
            request_id: params.request_id,
            status: RequestCancelStatus::AlreadyRequested,
        };
        assert_eq!(
            serde_json::to_value(result).expect("cancel result"),
            json!({
                "requestId": "execute-1",
                "status": "alreadyRequested"
            })
        );
        assert!(
            serde_json::from_value::<RequestCancelParams>(json!({
                "requestId": "execute-1",
                "unknown": true
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RequestCancelResult>(json!({
                "requestId": "execute-1",
                "status": "requested",
                "unknown": true
            }))
            .is_err()
        );
    }

    #[test]
    fn response_shape_cannot_contain_both_result_and_error() {
        let response = RpcResponse::success(RpcId::String("request-1".to_owned()), json!({}));
        let value = serde_json::to_value(response).expect("serialize response");
        assert!(value.get("result").is_some());
        assert!(value.get("error").is_none());
        assert_eq!(value["jsonrpc"], "2.0");

        let restored: RpcResponse = serde_json::from_value(value).expect("deserialize response");
        assert_eq!(restored.result(), Some(&Value::Object(Default::default())));

        let invalid = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {},
            "error": {
                "code": -32603,
                "message": "internal",
                "data": {
                    "code": "internal_error",
                    "message": "internal",
                    "retryable": false,
                    "details": null
                }
            }
        });
        assert!(serde_json::from_value::<RpcResponse>(invalid).is_err());
    }

    #[test]
    fn rpc_debug_views_do_not_render_raw_params() {
        const SENTINEL: &str = "DEVICERAIL_RPC_SECRET_SENTINEL";
        let params = RpcParams::Object(serde_json::Map::from_iter([(
            "arguments".to_owned(),
            json!({ "text": SENTINEL }),
        )]));
        let request = RpcRequest {
            jsonrpc: JsonRpcVersion::V2,
            id: RpcId::Number(1),
            method: "device.execute".to_owned(),
            timeout_ms: None,
            params: Some(params.clone()),
        };
        assert!(!format!("{params:?}").contains(SENTINEL));
        assert!(!format!("{request:?}").contains(SENTINEL));
        let response = RpcResponse::success(
            RpcId::Number(1),
            json!({ "endpoint": format!("ws://127.0.0.1/v/{SENTINEL}") }),
        );
        assert!(!format!("{response:?}").contains(SENTINEL));
    }
}
