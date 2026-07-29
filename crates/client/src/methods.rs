//! Typed DeviceRail RPC method markers.

use std::collections::BTreeSet;

use devicerail_protocol::{
    DeviceCapabilitiesResult, DeviceConnectResult, DeviceDisconnectResult, DeviceExecuteParams,
    DeviceExecuteResult, DeviceObserveResult, DeviceSelectParams, DeviceSelectResult,
    DevicesListResult, EventsClearResult, EventsListParams, EventsListResult,
    EventsStreamOpenParams, EventsStreamOpenResult, EventsSubscribeParams, EventsSubscribeResult,
    HelloParams, HelloResult, MediaStreamCaptureParams, MediaStreamCaptureResult,
    MediaStreamEndParams, MediaStreamEndResult, MediaStreamStartParams, MediaStreamStartResult,
    RequestCancelParams, RequestCancelResult, RpcParams, SessionCurrentResult, SessionEndParams,
    SessionEndResult, SessionExportParams, SessionExportResult, SessionStartResult,
    SessionTargetParams, SessionsListResult, SystemDescribeResult, UiSnapshotGetParams,
    UiSnapshotGetResult, VerdictRecordParams, VerdictRecordResult, feature,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::ClientError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodChannel {
    Bootstrap,
    Control,
    EventWebSocket,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodDescriptor {
    pub name: &'static str,
    pub channel: MethodChannel,
    pub required_feature: Option<&'static str>,
    pub supports_timeout: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoParams;

mod sealed {
    pub trait Sealed {}
}

pub trait RpcMethod: sealed::Sealed + Send + Sync + 'static {
    type Params: Send + Sync;
    type Result: DeserializeOwned + Send + 'static;

    const DESCRIPTOR: MethodDescriptor;

    fn encode_params(params: &Self::Params) -> Result<Option<RpcParams>, ClientError>;
}

pub trait ControlMethod: RpcMethod {}

fn required_params<T: Serialize>(params: &T) -> Result<Option<RpcParams>, ClientError> {
    let value = serde_json::to_value(params).map_err(ClientError::serialization)?;
    value_to_params(value).map(Some)
}

fn optional_params<T: Serialize>(params: &Option<T>) -> Result<Option<RpcParams>, ClientError> {
    params
        .as_ref()
        .map(required_params)
        .transpose()
        .map(Option::flatten)
}

fn value_to_params(value: Value) -> Result<RpcParams, ClientError> {
    match value {
        Value::Object(values) => Ok(RpcParams::Object(values)),
        Value::Array(values) => Ok(RpcParams::Array(values)),
        _ => Err(ClientError::protocol(
            "method parameters must serialize to a JSON object or array",
        )),
    }
}

macro_rules! no_params_method {
    ($type:ident, $name:literal, $result:ty, $channel:ident, $feature:expr, $timeout:expr) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $type;
        impl sealed::Sealed for $type {}
        impl RpcMethod for $type {
            type Params = NoParams;
            type Result = $result;
            const DESCRIPTOR: MethodDescriptor = MethodDescriptor {
                name: $name,
                channel: MethodChannel::$channel,
                required_feature: $feature,
                supports_timeout: $timeout,
            };
            fn encode_params(_: &Self::Params) -> Result<Option<RpcParams>, ClientError> {
                Ok(None)
            }
        }
    };
}

macro_rules! required_method {
    ($type:ident, $name:literal, $params:ty, $result:ty, $channel:ident, $feature:expr, $timeout:expr) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $type;
        impl sealed::Sealed for $type {}
        impl RpcMethod for $type {
            type Params = $params;
            type Result = $result;
            const DESCRIPTOR: MethodDescriptor = MethodDescriptor {
                name: $name,
                channel: MethodChannel::$channel,
                required_feature: $feature,
                supports_timeout: $timeout,
            };
            fn encode_params(params: &Self::Params) -> Result<Option<RpcParams>, ClientError> {
                required_params(params)
            }
        }
    };
}

macro_rules! optional_method {
    ($type:ident, $name:literal, $params:ty, $result:ty, $feature:expr) => {
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $type;
        impl sealed::Sealed for $type {}
        impl RpcMethod for $type {
            type Params = Option<$params>;
            type Result = $result;
            const DESCRIPTOR: MethodDescriptor = MethodDescriptor {
                name: $name,
                channel: MethodChannel::Control,
                required_feature: $feature,
                supports_timeout: false,
            };
            fn encode_params(params: &Self::Params) -> Result<Option<RpcParams>, ClientError> {
                optional_params(params)
            }
        }
    };
}

required_method!(
    SystemHello,
    "system.hello",
    HelloParams,
    HelloResult,
    Bootstrap,
    None,
    false
);
no_params_method!(
    SystemDescribe,
    "system.describe",
    SystemDescribeResult,
    Control,
    None,
    false
);
no_params_method!(
    DevicesList,
    "devices.list",
    DevicesListResult,
    Control,
    Some(feature::DEVICE_ROUTING_V1),
    false
);
required_method!(
    DeviceSelect,
    "device.select",
    DeviceSelectParams,
    DeviceSelectResult,
    Control,
    Some(feature::DEVICE_ROUTING_V1),
    false
);
no_params_method!(
    DeviceConnect,
    "device.connect",
    DeviceConnectResult,
    Control,
    None,
    true
);
no_params_method!(
    DeviceDisconnect,
    "device.disconnect",
    DeviceDisconnectResult,
    Control,
    None,
    true
);
no_params_method!(
    DeviceCapabilities,
    "device.capabilities",
    DeviceCapabilitiesResult,
    Control,
    None,
    true
);
no_params_method!(
    DeviceObserve,
    "device.observe",
    DeviceObserveResult,
    Control,
    None,
    true
);
required_method!(
    DeviceExecute,
    "device.execute",
    DeviceExecuteParams,
    DeviceExecuteResult,
    Control,
    None,
    true
);
required_method!(
    RequestCancel,
    "request.cancel",
    RequestCancelParams,
    RequestCancelResult,
    Control,
    Some(feature::REQUEST_CONTROL_V1),
    false
);
no_params_method!(
    SessionStart,
    "session.start",
    SessionStartResult,
    Control,
    None,
    false
);
no_params_method!(
    SessionCurrent,
    "session.current",
    SessionCurrentResult,
    Control,
    None,
    false
);
optional_method!(
    SessionEnd,
    "session.end",
    SessionEndParams,
    SessionEndResult,
    None
);
no_params_method!(
    SessionsList,
    "sessions.list",
    SessionsListResult,
    Control,
    Some(feature::EVENTS_SNAPSHOT_V1),
    false
);
optional_method!(
    SessionExport,
    "session.export",
    SessionExportParams,
    SessionExportResult,
    Some(feature::EVENTS_SNAPSHOT_V1)
);
optional_method!(
    EventsList,
    "events.list",
    EventsListParams,
    EventsListResult,
    Some(feature::EVENTS_SNAPSHOT_V1)
);
optional_method!(
    EventsClear,
    "events.clear",
    SessionTargetParams,
    EventsClearResult,
    Some(feature::EVENTS_SNAPSHOT_V1)
);
required_method!(
    EventsStreamOpen,
    "events.stream.open",
    EventsStreamOpenParams,
    EventsStreamOpenResult,
    Control,
    Some(feature::EVENTS_STREAM_V1),
    false
);
required_method!(
    EventsSubscribe,
    "events.subscribe",
    EventsSubscribeParams,
    EventsSubscribeResult,
    EventWebSocket,
    Some(feature::EVENTS_STREAM_V1),
    false
);
required_method!(
    MediaStreamStart,
    "media.stream.start",
    MediaStreamStartParams,
    MediaStreamStartResult,
    Control,
    Some(feature::MEDIA_STREAM_V1),
    false
);
required_method!(
    MediaStreamCapture,
    "media.stream.capture",
    MediaStreamCaptureParams,
    MediaStreamCaptureResult,
    Control,
    Some(feature::MEDIA_STREAM_V1),
    true
);
required_method!(
    MediaStreamEnd,
    "media.stream.end",
    MediaStreamEndParams,
    MediaStreamEndResult,
    Control,
    Some(feature::MEDIA_STREAM_V1),
    false
);
required_method!(
    UiSnapshotGet,
    "ui.snapshot.get",
    UiSnapshotGetParams,
    UiSnapshotGetResult,
    Control,
    Some(feature::OBSERVATION_UI_SNAPSHOT_V1),
    false
);
required_method!(
    VerdictRecord,
    "verdict.record",
    VerdictRecordParams,
    VerdictRecordResult,
    Control,
    Some(feature::VERDICT_RECORD_V1),
    false
);

macro_rules! control_methods {
    ($($type:ty),+ $(,)?) => {
        $(impl ControlMethod for $type {})+
    };
}

control_methods!(
    SystemDescribe,
    DevicesList,
    DeviceSelect,
    DeviceConnect,
    DeviceDisconnect,
    DeviceCapabilities,
    DeviceObserve,
    DeviceExecute,
    RequestCancel,
    SessionStart,
    SessionCurrent,
    SessionEnd,
    SessionsList,
    SessionExport,
    EventsList,
    EventsClear,
    EventsStreamOpen,
    MediaStreamStart,
    MediaStreamCapture,
    MediaStreamEnd,
    UiSnapshotGet,
    VerdictRecord,
);

pub const METHOD_CATALOG: [MethodDescriptor; 24] = [
    SystemHello::DESCRIPTOR,
    SystemDescribe::DESCRIPTOR,
    DevicesList::DESCRIPTOR,
    DeviceSelect::DESCRIPTOR,
    DeviceConnect::DESCRIPTOR,
    DeviceDisconnect::DESCRIPTOR,
    DeviceCapabilities::DESCRIPTOR,
    DeviceObserve::DESCRIPTOR,
    DeviceExecute::DESCRIPTOR,
    RequestCancel::DESCRIPTOR,
    SessionStart::DESCRIPTOR,
    SessionCurrent::DESCRIPTOR,
    SessionEnd::DESCRIPTOR,
    SessionsList::DESCRIPTOR,
    SessionExport::DESCRIPTOR,
    EventsList::DESCRIPTOR,
    EventsClear::DESCRIPTOR,
    EventsStreamOpen::DESCRIPTOR,
    EventsSubscribe::DESCRIPTOR,
    MediaStreamStart::DESCRIPTOR,
    MediaStreamCapture::DESCRIPTOR,
    MediaStreamEnd::DESCRIPTOR,
    UiSnapshotGet::DESCRIPTOR,
    VerdictRecord::DESCRIPTOR,
];

pub(crate) fn validate_dynamic_features(
    descriptor: MethodDescriptor,
    params: Option<&RpcParams>,
    enabled: &BTreeSet<String>,
    timeout_requested: bool,
) -> Result<(), ClientError> {
    if let Some(feature) = descriptor.required_feature
        && !enabled.contains(feature)
    {
        return Err(ClientError::FeatureNotNegotiated {
            method: descriptor.name,
            feature,
        });
    }
    if timeout_requested && !descriptor.supports_timeout {
        return Err(ClientError::protocol(format!(
            "{} does not support timeoutMs",
            descriptor.name
        )));
    }

    let object = params.and_then(|params| match params {
        RpcParams::Object(object) => Some(object),
        RpcParams::Array(_) => None,
    });
    let action_timeout_requested = descriptor.name == "device.execute"
        && object.is_some_and(|object| object.contains_key("actionTimeoutMs"));
    if (timeout_requested || action_timeout_requested)
        && !enabled.contains(feature::REQUEST_CONTROL_V1)
    {
        return Err(ClientError::FeatureNotNegotiated {
            method: descriptor.name,
            feature: feature::REQUEST_CONTROL_V1,
        });
    }
    if descriptor.name == "session.export"
        && object.is_some_and(|object| {
            object.contains_key("afterSequence") || object.contains_key("limit")
        })
        && !enabled.contains(feature::SESSION_EXPORT_PAGE_V1)
    {
        return Err(ClientError::FeatureNotNegotiated {
            method: descriptor.name,
            feature: feature::SESSION_EXPORT_PAGE_V1,
        });
    }
    if descriptor.name == "device.execute"
        && object
            .and_then(|object| object.get("name"))
            .and_then(Value::as_str)
            .is_some_and(devicerail_protocol::is_semantic_action_name)
        && !enabled.contains(feature::DEVICE_SEMANTIC_ACTIONS_V1)
    {
        return Err(ClientError::FeatureNotNegotiated {
            method: descriptor.name,
            feature: feature::DEVICE_SEMANTIC_ACTIONS_V1,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{METHOD_CATALOG, MethodChannel};

    #[test]
    fn method_catalog_is_complete_and_unique() {
        let names = METHOD_CATALOG
            .iter()
            .map(|descriptor| descriptor.name)
            .collect::<BTreeSet<_>>();
        assert_eq!(names.len(), 24);
        assert_eq!(
            METHOD_CATALOG
                .iter()
                .filter(|item| item.channel == MethodChannel::Bootstrap)
                .count(),
            1
        );
        assert_eq!(
            METHOD_CATALOG
                .iter()
                .filter(|item| item.channel == MethodChannel::EventWebSocket)
                .count(),
            1
        );
        assert_eq!(
            METHOD_CATALOG
                .iter()
                .filter(|item| item.supports_timeout)
                .map(|item| item.name)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "device.capabilities",
                "device.connect",
                "device.disconnect",
                "device.execute",
                "device.observe",
                "media.stream.capture",
            ])
        );
    }
}
