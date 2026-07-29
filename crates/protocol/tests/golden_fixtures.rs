use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionOutcome, ActionResult, ClearElementArguments,
    ClearElementResult, DeviceCapabilitiesResult, DeviceConnectResult, DeviceDisconnectResult,
    DeviceExecuteParams, DeviceExecuteResult, DeviceInfo, DeviceObserveResult, DeviceSelectParams,
    DeviceSelectResult, DevicesListResult, ErrorInfo, EventsClearResult, EventsListParams,
    EventsListResult, EventsStreamEventNotification, EventsStreamOpenParams,
    EventsStreamOpenResult, EventsStreamTerminalNotification, EventsSubscribeParams,
    EventsSubscribeResult, FindElementArguments, FindElementResult, HelloParams, HelloResult,
    ManualRecording, MediaStreamCaptureParams, MediaStreamCaptureResult, MediaStreamEndParams,
    MediaStreamEndResult, MediaStreamStartParams, MediaStreamStartResult, Observation,
    ProtocolVersion, RequestCancelParams, RequestCancelResult, RpcError, RpcRequest, RpcResponse,
    SessionCurrentResult, SessionEndParams, SessionEndResult, SessionExportParams,
    SessionExportResult, SessionStartResult, SessionTargetParams, SessionsListResult,
    SetElementValueArguments, SetElementValueResult, SystemDescribeResult, TapElementArguments,
    TapElementResult, TestEvent, TestEventPayload, UiSnapshot, UiSnapshotGetParams,
    UiSnapshotGetResult, VerdictRecordParams, VerdictRecordResult, WaitForElementArguments,
    WaitForElementResult, feature, negotiate_features, negotiate_protocol,
    supported_protocol_offer,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureManifest {
    manifest_version: u32,
    protocol_version: ProtocolVersion,
    fixture_paths_relative_to: String,
    schema_paths_relative_to: String,
    fixtures: Vec<FixtureEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FixtureEntry {
    id: String,
    path: String,
    schema: String,
    kind: FixtureKind,
    model: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum FixtureKind {
    HelloRequest,
    HelloResponse,
    SystemDescribeRequest,
    SystemDescribeResponse,
    DeviceInfo,
    Observation,
    ActionDefinition,
    ActionCall,
    ManualRecording,
    ActionResult,
    UiSnapshot,
    FindElementArguments,
    FindElementResult,
    TapElementArguments,
    TapElementResult,
    ClearElementArguments,
    ClearElementResult,
    SetElementValueArguments,
    SetElementValueResult,
    WaitForElementArguments,
    WaitForElementResult,
    ErrorInfo,
    DeviceConnectRequest,
    DeviceConnectResponse,
    DeviceDisconnectRequest,
    DeviceDisconnectResponse,
    DeviceCapabilitiesRequest,
    DeviceCapabilitiesResponse,
    DeviceObserveRequest,
    DeviceObserveResponse,
    DeviceExecuteRequest,
    DeviceExecuteResponse,
    DeviceSelectRequest,
    DeviceSelectResponse,
    DevicesListRequest,
    DevicesListResponse,
    RequestCancelRequest,
    RequestCancelResponse,
    SessionStartRequest,
    SessionStartResponse,
    SessionCurrentRequest,
    SessionCurrentResponse,
    SessionEndRequest,
    SessionEndResponse,
    SessionsListRequest,
    SessionsListResponse,
    SessionExportRequest,
    SessionExportResponse,
    EventsListRequest,
    EventsListResponse,
    EventsClearRequest,
    EventsClearResponse,
    EventsStreamOpenRequest,
    EventsStreamOpenResponse,
    EventsSubscribeRequest,
    EventsSubscribeResponse,
    MediaStreamCaptureRequest,
    MediaStreamCaptureResponse,
    MediaStreamEndRequest,
    MediaStreamEndResponse,
    MediaStreamStartRequest,
    MediaStreamStartResponse,
    UiSnapshotGetRequest,
    UiSnapshotGetResponse,
    VerdictRecordRequest,
    VerdictRecordResponse,
    EventsStreamEventNotification,
    EventsStreamTerminalNotification,
    WebSocketHelloRequest,
    WebSocketHelloResponse,
    RpcFailure,
    TestEvent,
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn read_manifest() -> FixtureManifest {
    let path = fixtures_root().join("manifest.json");
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("failed to deserialize {}: {error}", path.display()))
}

fn read_fixture(entry: &FixtureEntry) -> Value {
    let path = fixtures_root().join(&entry.path);
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read fixture {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("fixture {} is not valid JSON: {error}", path.display()))
}

fn expected_schema(kind: FixtureKind) -> &'static str {
    match kind {
        FixtureKind::HelloRequest => "protocol/schema/v1/system-hello-request.schema.json",
        FixtureKind::HelloResponse => "protocol/schema/v1/system-hello-response.schema.json",
        FixtureKind::SystemDescribeRequest => {
            "protocol/schema/v1/system-describe-request.schema.json"
        }
        FixtureKind::SystemDescribeResponse => {
            "protocol/schema/v1/system-describe-response.schema.json"
        }
        FixtureKind::DeviceInfo => "protocol/schema/v1/device-info.schema.json",
        FixtureKind::Observation => "protocol/schema/v1/observation.schema.json",
        FixtureKind::ActionDefinition => "protocol/schema/v1/action-definition.schema.json",
        FixtureKind::ActionCall => "protocol/schema/v1/action-call.schema.json",
        FixtureKind::ManualRecording => "protocol/schema/v1/manual-recording.schema.json",
        FixtureKind::ActionResult => "protocol/schema/v1/action-result.schema.json",
        FixtureKind::UiSnapshot => "protocol/schema/v1/ui-snapshot.schema.json",
        FixtureKind::FindElementArguments => {
            "protocol/schema/v1/find-element-arguments.schema.json"
        }
        FixtureKind::FindElementResult => "protocol/schema/v1/find-element-result.schema.json",
        FixtureKind::TapElementArguments => "protocol/schema/v1/tap-element-arguments.schema.json",
        FixtureKind::TapElementResult => "protocol/schema/v1/tap-element-result.schema.json",
        FixtureKind::ClearElementArguments => {
            "protocol/schema/v1/clear-element-arguments.schema.json"
        }
        FixtureKind::ClearElementResult => "protocol/schema/v1/clear-element-result.schema.json",
        FixtureKind::SetElementValueArguments => {
            "protocol/schema/v1/set-element-value-arguments.schema.json"
        }
        FixtureKind::SetElementValueResult => {
            "protocol/schema/v1/set-element-value-result.schema.json"
        }
        FixtureKind::WaitForElementArguments => {
            "protocol/schema/v1/wait-for-element-arguments.schema.json"
        }
        FixtureKind::WaitForElementResult => {
            "protocol/schema/v1/wait-for-element-result.schema.json"
        }
        FixtureKind::ErrorInfo => "protocol/schema/v1/error-info.schema.json",
        FixtureKind::DeviceConnectRequest => {
            "protocol/schema/v1/device-connect-request.schema.json"
        }
        FixtureKind::DeviceConnectResponse => {
            "protocol/schema/v1/device-connect-response.schema.json"
        }
        FixtureKind::DeviceDisconnectRequest => {
            "protocol/schema/v1/device-disconnect-request.schema.json"
        }
        FixtureKind::DeviceDisconnectResponse => {
            "protocol/schema/v1/device-disconnect-response.schema.json"
        }
        FixtureKind::DeviceCapabilitiesRequest => {
            "protocol/schema/v1/device-capabilities-request.schema.json"
        }
        FixtureKind::DeviceCapabilitiesResponse => {
            "protocol/schema/v1/device-capabilities-response.schema.json"
        }
        FixtureKind::DeviceObserveRequest => {
            "protocol/schema/v1/device-observe-request.schema.json"
        }
        FixtureKind::DeviceObserveResponse => {
            "protocol/schema/v1/device-observe-response.schema.json"
        }
        FixtureKind::DeviceExecuteRequest => {
            "protocol/schema/v1/device-execute-request.schema.json"
        }
        FixtureKind::DeviceExecuteResponse => {
            "protocol/schema/v1/device-execute-response.schema.json"
        }
        FixtureKind::DeviceSelectRequest => "protocol/schema/v1/device-select-request.schema.json",
        FixtureKind::DeviceSelectResponse => {
            "protocol/schema/v1/device-select-response.schema.json"
        }
        FixtureKind::DevicesListRequest => "protocol/schema/v1/devices-list-request.schema.json",
        FixtureKind::DevicesListResponse => "protocol/schema/v1/devices-list-response.schema.json",
        FixtureKind::RequestCancelRequest => {
            "protocol/schema/v1/request-cancel-request.schema.json"
        }
        FixtureKind::RequestCancelResponse => {
            "protocol/schema/v1/request-cancel-response.schema.json"
        }
        FixtureKind::SessionStartRequest => "protocol/schema/v1/session-start-request.schema.json",
        FixtureKind::SessionStartResponse => {
            "protocol/schema/v1/session-start-response.schema.json"
        }
        FixtureKind::SessionCurrentRequest => {
            "protocol/schema/v1/session-current-request.schema.json"
        }
        FixtureKind::SessionCurrentResponse => {
            "protocol/schema/v1/session-current-response.schema.json"
        }
        FixtureKind::SessionEndRequest => "protocol/schema/v1/session-end-request.schema.json",
        FixtureKind::SessionEndResponse => "protocol/schema/v1/session-end-response.schema.json",
        FixtureKind::SessionsListRequest => "protocol/schema/v1/sessions-list-request.schema.json",
        FixtureKind::SessionsListResponse => {
            "protocol/schema/v1/sessions-list-response.schema.json"
        }
        FixtureKind::SessionExportRequest => {
            "protocol/schema/v1/session-export-request.schema.json"
        }
        FixtureKind::SessionExportResponse => {
            "protocol/schema/v1/session-export-response.schema.json"
        }
        FixtureKind::EventsListRequest => "protocol/schema/v1/events-list-request.schema.json",
        FixtureKind::EventsListResponse => "protocol/schema/v1/events-list-response.schema.json",
        FixtureKind::EventsClearRequest => "protocol/schema/v1/events-clear-request.schema.json",
        FixtureKind::EventsClearResponse => "protocol/schema/v1/events-clear-response.schema.json",
        FixtureKind::EventsStreamOpenRequest => {
            "protocol/schema/v1/events-stream-open-request.schema.json"
        }
        FixtureKind::EventsStreamOpenResponse => {
            "protocol/schema/v1/events-stream-open-response.schema.json"
        }
        FixtureKind::EventsSubscribeRequest => {
            "protocol/schema/v1/events-subscribe-request.schema.json"
        }
        FixtureKind::EventsSubscribeResponse => {
            "protocol/schema/v1/events-subscribe-response.schema.json"
        }
        FixtureKind::MediaStreamCaptureRequest => {
            "protocol/schema/v1/media-stream-capture-request.schema.json"
        }
        FixtureKind::MediaStreamCaptureResponse => {
            "protocol/schema/v1/media-stream-capture-response.schema.json"
        }
        FixtureKind::MediaStreamEndRequest => {
            "protocol/schema/v1/media-stream-end-request.schema.json"
        }
        FixtureKind::MediaStreamEndResponse => {
            "protocol/schema/v1/media-stream-end-response.schema.json"
        }
        FixtureKind::MediaStreamStartRequest => {
            "protocol/schema/v1/media-stream-start-request.schema.json"
        }
        FixtureKind::MediaStreamStartResponse => {
            "protocol/schema/v1/media-stream-start-response.schema.json"
        }
        FixtureKind::UiSnapshotGetRequest => {
            "protocol/schema/v1/ui-snapshot-get-request.schema.json"
        }
        FixtureKind::UiSnapshotGetResponse => {
            "protocol/schema/v1/ui-snapshot-get-response.schema.json"
        }
        FixtureKind::VerdictRecordRequest => {
            "protocol/schema/v1/verdict-record-request.schema.json"
        }
        FixtureKind::VerdictRecordResponse => {
            "protocol/schema/v1/verdict-record-response.schema.json"
        }
        FixtureKind::EventsStreamEventNotification => {
            "protocol/schema/v1/events-stream-event-notification.schema.json"
        }
        FixtureKind::EventsStreamTerminalNotification => {
            "protocol/schema/v1/events-stream-terminal-notification.schema.json"
        }
        FixtureKind::WebSocketHelloRequest => "protocol/schema/v1/system-hello-request.schema.json",
        FixtureKind::WebSocketHelloResponse => {
            "protocol/schema/v1/system-hello-response.schema.json"
        }
        FixtureKind::RpcFailure => "protocol/schema/v1/rpc-response.schema.json",
        FixtureKind::TestEvent => "protocol/schema/v1/test-event.schema.json",
    }
}

fn assert_typed_round_trip<T>(entry: &FixtureEntry, value: &Value) -> T
where
    T: DeserializeOwned + Serialize,
{
    let typed: T = serde_json::from_value(value.clone()).unwrap_or_else(|error| {
        panic!(
            "fixture {} cannot deserialize as {}: {error}",
            entry.id,
            std::any::type_name::<T>()
        )
    });
    let restored = serde_json::to_value(&typed)
        .unwrap_or_else(|error| panic!("fixture {} cannot reserialize: {error}", entry.id));
    assert_eq!(
        restored, *value,
        "fixture {} changed after typed round trip",
        entry.id
    );
    typed
}

fn assert_no_params_request(entry: &FixtureEntry, value: &Value, method: &str) {
    let request: RpcRequest = serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("invalid {}: {error}", entry.id));
    assert_eq!(request.method, method);
    assert!(
        request.params.is_none(),
        "{} must omit params in the no-params baseline",
        entry.id
    );
    assert_eq!(
        serde_json::to_value(request).expect("reserialize no-params request"),
        *value,
        "fixture {} changed after RPC request round trip",
        entry.id
    );
}

fn assert_typed_request<T>(entry: &FixtureEntry, value: &Value, method: &str)
where
    T: DeserializeOwned + Serialize,
{
    let request: RpcRequest = serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("invalid {}: {error}", entry.id));
    assert_eq!(request.method, method);
    let params = request
        .params
        .clone()
        .unwrap_or_else(|| panic!("{} must include params", entry.id))
        .into_value();
    assert_typed_round_trip::<T>(entry, &params);
    assert_eq!(
        serde_json::to_value(request).expect("reserialize typed request"),
        *value,
        "fixture {} changed after RPC request round trip",
        entry.id
    );
}

fn assert_typed_response<T>(entry: &FixtureEntry, value: &Value, method: &str)
where
    T: DeserializeOwned + Serialize,
{
    let response: RpcResponse = serde_json::from_value(value.clone())
        .unwrap_or_else(|error| panic!("invalid {}: {error}", entry.id));
    let result = response
        .result()
        .cloned()
        .unwrap_or_else(|| panic!("{method} fixture {} must be a success response", entry.id));
    assert_typed_round_trip::<T>(entry, &result);
    assert_eq!(
        serde_json::to_value(response).expect("reserialize typed response"),
        *value,
        "fixture {} changed after RPC response round trip",
        entry.id
    );
}

fn collect_json_paths(root: &Path, directory: &Path, output: &mut BTreeSet<String>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to list {}: {error}", directory.display()))
    {
        let entry = entry.expect("read fixture directory entry");
        let path = entry.path();
        if path.is_dir() {
            collect_json_paths(root, &path, output);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            let relative = path
                .strip_prefix(root)
                .expect("fixture path is below fixture root")
                .to_string_lossy()
                .replace('\\', "/");
            if relative != "manifest.json" {
                output.insert(relative);
            }
        }
    }
}

#[test]
fn manifest_is_complete_unique_and_resolvable() {
    let manifest = read_manifest();
    assert_eq!(manifest.manifest_version, 1);
    assert_eq!(manifest.protocol_version, ProtocolVersion::new(1, 5));
    assert_eq!(manifest.fixture_paths_relative_to, "manifestDirectory");
    assert_eq!(manifest.schema_paths_relative_to, "repositoryRoot");
    assert!(!manifest.fixtures.is_empty());

    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for entry in &manifest.fixtures {
        assert!(!entry.id.trim().is_empty(), "fixture id must not be empty");
        assert!(
            !entry.model.trim().is_empty(),
            "fixture {} must name its wire model",
            entry.id
        );
        assert!(
            !entry.schema.trim().is_empty(),
            "fixture {} must name its JSON Schema",
            entry.id
        );
        assert_eq!(
            entry.schema,
            expected_schema(entry.kind),
            "fixture {} points to the wrong schema for {:?}",
            entry.id,
            entry.kind
        );
        assert!(
            ids.insert(entry.id.clone()),
            "duplicate fixture id: {}",
            entry.id
        );
        assert!(
            paths.insert(entry.path.clone()),
            "duplicate fixture path: {}",
            entry.path
        );

        let relative_path = Path::new(&entry.path);
        assert!(
            !relative_path.is_absolute()
                && relative_path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "fixture {} has an unsafe path: {}",
            entry.id,
            entry.path
        );
        assert_eq!(
            relative_path
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("json"),
            "fixture {} must point to JSON",
            entry.id
        );
        let full_path = fixtures_root().join(relative_path);
        assert!(
            full_path.is_file(),
            "fixture {} does not exist: {}",
            entry.id,
            full_path.display()
        );
        let repository_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("protocol crate is below the repository root")
            .to_path_buf();
        let schema_relative_path = Path::new(&entry.schema);
        assert!(
            !schema_relative_path.is_absolute()
                && schema_relative_path
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))),
            "fixture {} has an unsafe schema path: {}",
            entry.id,
            entry.schema
        );
        assert!(
            entry.schema.ends_with(".schema.json"),
            "fixture {} must point to a JSON Schema",
            entry.id
        );
        let schema_path = repository_root.join(schema_relative_path);
        assert!(
            schema_path.is_file(),
            "fixture {} schema does not exist: {}",
            entry.id,
            schema_path.display()
        );
        let _ = read_fixture(entry);
    }

    let mut discovered_paths = BTreeSet::new();
    collect_json_paths(&fixtures_root(), &fixtures_root(), &mut discovered_paths);
    assert_eq!(
        paths, discovered_paths,
        "manifest paths must exactly match every JSON fixture on disk"
    );
}

#[test]
fn public_methods_have_one_request_and_response_fixture() {
    let manifest = read_manifest();
    let method_fixture_pairs = [
        (FixtureKind::HelloRequest, FixtureKind::HelloResponse),
        (
            FixtureKind::SystemDescribeRequest,
            FixtureKind::SystemDescribeResponse,
        ),
        (
            FixtureKind::DevicesListRequest,
            FixtureKind::DevicesListResponse,
        ),
        (
            FixtureKind::DeviceSelectRequest,
            FixtureKind::DeviceSelectResponse,
        ),
        (
            FixtureKind::DeviceConnectRequest,
            FixtureKind::DeviceConnectResponse,
        ),
        (
            FixtureKind::DeviceDisconnectRequest,
            FixtureKind::DeviceDisconnectResponse,
        ),
        (
            FixtureKind::DeviceCapabilitiesRequest,
            FixtureKind::DeviceCapabilitiesResponse,
        ),
        (
            FixtureKind::DeviceObserveRequest,
            FixtureKind::DeviceObserveResponse,
        ),
        (
            FixtureKind::DeviceExecuteRequest,
            FixtureKind::DeviceExecuteResponse,
        ),
        (
            FixtureKind::RequestCancelRequest,
            FixtureKind::RequestCancelResponse,
        ),
        (
            FixtureKind::SessionStartRequest,
            FixtureKind::SessionStartResponse,
        ),
        (
            FixtureKind::SessionCurrentRequest,
            FixtureKind::SessionCurrentResponse,
        ),
        (
            FixtureKind::SessionEndRequest,
            FixtureKind::SessionEndResponse,
        ),
        (
            FixtureKind::SessionsListRequest,
            FixtureKind::SessionsListResponse,
        ),
        (
            FixtureKind::SessionExportRequest,
            FixtureKind::SessionExportResponse,
        ),
        (
            FixtureKind::EventsListRequest,
            FixtureKind::EventsListResponse,
        ),
        (
            FixtureKind::EventsClearRequest,
            FixtureKind::EventsClearResponse,
        ),
        (
            FixtureKind::EventsStreamOpenRequest,
            FixtureKind::EventsStreamOpenResponse,
        ),
        (
            FixtureKind::EventsSubscribeRequest,
            FixtureKind::EventsSubscribeResponse,
        ),
        (
            FixtureKind::MediaStreamCaptureRequest,
            FixtureKind::MediaStreamCaptureResponse,
        ),
        (
            FixtureKind::MediaStreamEndRequest,
            FixtureKind::MediaStreamEndResponse,
        ),
        (
            FixtureKind::MediaStreamStartRequest,
            FixtureKind::MediaStreamStartResponse,
        ),
        (
            FixtureKind::UiSnapshotGetRequest,
            FixtureKind::UiSnapshotGetResponse,
        ),
        (
            FixtureKind::VerdictRecordRequest,
            FixtureKind::VerdictRecordResponse,
        ),
    ];

    assert_eq!(method_fixture_pairs.len(), 24);
    for kind in method_fixture_pairs
        .into_iter()
        .flat_map(|pair| [pair.0, pair.1])
    {
        assert_eq!(
            manifest
                .fixtures
                .iter()
                .filter(|entry| entry.kind == kind)
                .count(),
            1,
            "expected exactly one fixture for {kind:?}"
        );
    }
}

#[test]
fn every_fixture_has_a_lossless_typed_round_trip() {
    for entry in read_manifest().fixtures {
        let value = read_fixture(&entry);
        match entry.kind {
            FixtureKind::HelloRequest => {
                assert_typed_request::<HelloParams>(&entry, &value, "system.hello");
            }
            FixtureKind::HelloResponse => {
                assert_typed_response::<HelloResult>(&entry, &value, "system.hello");
            }
            FixtureKind::SystemDescribeRequest => {
                assert_no_params_request(&entry, &value, "system.describe");
            }
            FixtureKind::SystemDescribeResponse => {
                assert_typed_response::<SystemDescribeResult>(&entry, &value, "system.describe");
            }
            FixtureKind::DeviceInfo => {
                assert_typed_round_trip::<DeviceInfo>(&entry, &value);
            }
            FixtureKind::Observation => {
                assert_typed_round_trip::<Observation>(&entry, &value);
            }
            FixtureKind::ActionDefinition => {
                assert_typed_round_trip::<ActionDefinition>(&entry, &value);
            }
            FixtureKind::ActionCall => {
                assert_typed_round_trip::<ActionCall>(&entry, &value);
            }
            FixtureKind::ManualRecording => {
                assert_typed_round_trip::<ManualRecording>(&entry, &value);
            }
            FixtureKind::ActionResult => {
                assert_typed_round_trip::<ActionResult>(&entry, &value);
            }
            FixtureKind::UiSnapshot => {
                assert_typed_round_trip::<UiSnapshot>(&entry, &value)
                    .validate()
                    .expect("canonical UI Snapshot fixture");
            }
            FixtureKind::FindElementArguments => {
                assert_typed_round_trip::<FindElementArguments>(&entry, &value)
                    .validate()
                    .expect("valid findElement arguments");
            }
            FixtureKind::FindElementResult => {
                assert_typed_round_trip::<FindElementResult>(&entry, &value)
                    .element
                    .validate()
                    .expect("valid findElement result");
            }
            FixtureKind::TapElementArguments => {
                assert_typed_round_trip::<TapElementArguments>(&entry, &value)
                    .validate()
                    .expect("valid tapElement arguments");
            }
            FixtureKind::TapElementResult => {
                assert_typed_round_trip::<TapElementResult>(&entry, &value)
                    .element
                    .validate()
                    .expect("valid tapElement result");
            }
            FixtureKind::ClearElementArguments => {
                assert_typed_round_trip::<ClearElementArguments>(&entry, &value)
                    .validate()
                    .expect("valid clearElement arguments");
            }
            FixtureKind::ClearElementResult => {
                assert_typed_round_trip::<ClearElementResult>(&entry, &value)
                    .element
                    .validate()
                    .expect("valid clearElement result");
            }
            FixtureKind::SetElementValueArguments => {
                assert_typed_round_trip::<SetElementValueArguments>(&entry, &value)
                    .validate()
                    .expect("valid setElementValue arguments");
            }
            FixtureKind::SetElementValueResult => {
                assert_typed_round_trip::<SetElementValueResult>(&entry, &value)
                    .element
                    .validate()
                    .expect("valid setElementValue result");
            }
            FixtureKind::WaitForElementArguments => {
                assert_typed_round_trip::<WaitForElementArguments>(&entry, &value)
                    .validate()
                    .expect("valid waitForElement arguments");
            }
            FixtureKind::WaitForElementResult => {
                assert_typed_round_trip::<WaitForElementResult>(&entry, &value)
                    .validate()
                    .expect("valid waitForElement result");
            }
            FixtureKind::ErrorInfo => {
                assert_typed_round_trip::<ErrorInfo>(&entry, &value);
            }
            FixtureKind::DeviceConnectRequest => {
                assert_no_params_request(&entry, &value, "device.connect");
            }
            FixtureKind::DeviceConnectResponse => {
                assert_typed_response::<DeviceConnectResult>(&entry, &value, "device.connect");
            }
            FixtureKind::DeviceDisconnectRequest => {
                assert_no_params_request(&entry, &value, "device.disconnect");
            }
            FixtureKind::DeviceDisconnectResponse => {
                assert_typed_response::<DeviceDisconnectResult>(
                    &entry,
                    &value,
                    "device.disconnect",
                );
            }
            FixtureKind::DeviceCapabilitiesRequest => {
                assert_no_params_request(&entry, &value, "device.capabilities");
            }
            FixtureKind::DeviceCapabilitiesResponse => {
                assert_typed_response::<DeviceCapabilitiesResult>(
                    &entry,
                    &value,
                    "device.capabilities",
                );
            }
            FixtureKind::DeviceObserveRequest => {
                assert_no_params_request(&entry, &value, "device.observe");
            }
            FixtureKind::DeviceObserveResponse => {
                assert_typed_response::<DeviceObserveResult>(&entry, &value, "device.observe");
            }
            FixtureKind::DeviceExecuteRequest => {
                assert_typed_request::<DeviceExecuteParams>(&entry, &value, "device.execute");
            }
            FixtureKind::DeviceExecuteResponse => {
                assert_typed_response::<DeviceExecuteResult>(&entry, &value, "device.execute");
            }
            FixtureKind::DeviceSelectRequest => {
                assert_typed_request::<DeviceSelectParams>(&entry, &value, "device.select");
            }
            FixtureKind::DeviceSelectResponse => {
                assert_typed_response::<DeviceSelectResult>(&entry, &value, "device.select");
            }
            FixtureKind::DevicesListRequest => {
                assert_no_params_request(&entry, &value, "devices.list");
            }
            FixtureKind::DevicesListResponse => {
                assert_typed_response::<DevicesListResult>(&entry, &value, "devices.list");
            }
            FixtureKind::RequestCancelRequest => {
                assert_typed_request::<RequestCancelParams>(&entry, &value, "request.cancel");
            }
            FixtureKind::RequestCancelResponse => {
                assert_typed_response::<RequestCancelResult>(&entry, &value, "request.cancel");
            }
            FixtureKind::SessionStartRequest => {
                assert_no_params_request(&entry, &value, "session.start");
            }
            FixtureKind::SessionStartResponse => {
                assert_typed_response::<SessionStartResult>(&entry, &value, "session.start");
            }
            FixtureKind::SessionCurrentRequest => {
                assert_no_params_request(&entry, &value, "session.current");
            }
            FixtureKind::SessionCurrentResponse => {
                assert_typed_response::<SessionCurrentResult>(&entry, &value, "session.current");
            }
            FixtureKind::SessionEndRequest => {
                assert_typed_request::<SessionEndParams>(&entry, &value, "session.end");
            }
            FixtureKind::SessionEndResponse => {
                assert_typed_response::<SessionEndResult>(&entry, &value, "session.end");
            }
            FixtureKind::SessionsListRequest => {
                assert_no_params_request(&entry, &value, "sessions.list");
            }
            FixtureKind::SessionsListResponse => {
                assert_typed_response::<SessionsListResult>(&entry, &value, "sessions.list");
            }
            FixtureKind::SessionExportRequest => {
                assert_typed_request::<SessionExportParams>(&entry, &value, "session.export");
            }
            FixtureKind::SessionExportResponse => {
                assert_typed_response::<SessionExportResult>(&entry, &value, "session.export");
            }
            FixtureKind::EventsListRequest => {
                assert_typed_request::<EventsListParams>(&entry, &value, "events.list");
            }
            FixtureKind::EventsListResponse => {
                assert_typed_response::<EventsListResult>(&entry, &value, "events.list");
            }
            FixtureKind::EventsClearRequest => {
                assert_typed_request::<SessionTargetParams>(&entry, &value, "events.clear");
            }
            FixtureKind::EventsClearResponse => {
                assert_typed_response::<EventsClearResult>(&entry, &value, "events.clear");
            }
            FixtureKind::EventsStreamOpenRequest => {
                assert_typed_request::<EventsStreamOpenParams>(
                    &entry,
                    &value,
                    "events.stream.open",
                );
            }
            FixtureKind::EventsStreamOpenResponse => {
                assert_typed_response::<EventsStreamOpenResult>(
                    &entry,
                    &value,
                    "events.stream.open",
                );
            }
            FixtureKind::EventsSubscribeRequest => {
                assert_typed_request::<EventsSubscribeParams>(&entry, &value, "events.subscribe");
            }
            FixtureKind::EventsSubscribeResponse => {
                assert_typed_response::<EventsSubscribeResult>(&entry, &value, "events.subscribe");
            }
            FixtureKind::MediaStreamCaptureRequest => {
                assert_typed_request::<MediaStreamCaptureParams>(
                    &entry,
                    &value,
                    "media.stream.capture",
                );
            }
            FixtureKind::MediaStreamCaptureResponse => {
                assert_typed_response::<MediaStreamCaptureResult>(
                    &entry,
                    &value,
                    "media.stream.capture",
                );
            }
            FixtureKind::MediaStreamEndRequest => {
                assert_typed_request::<MediaStreamEndParams>(&entry, &value, "media.stream.end");
            }
            FixtureKind::MediaStreamEndResponse => {
                assert_typed_response::<MediaStreamEndResult>(&entry, &value, "media.stream.end");
            }
            FixtureKind::MediaStreamStartRequest => {
                assert_typed_request::<MediaStreamStartParams>(
                    &entry,
                    &value,
                    "media.stream.start",
                );
            }
            FixtureKind::MediaStreamStartResponse => {
                assert_typed_response::<MediaStreamStartResult>(
                    &entry,
                    &value,
                    "media.stream.start",
                );
            }
            FixtureKind::UiSnapshotGetRequest => {
                assert_typed_request::<UiSnapshotGetParams>(&entry, &value, "ui.snapshot.get");
            }
            FixtureKind::UiSnapshotGetResponse => {
                assert_typed_response::<UiSnapshotGetResult>(&entry, &value, "ui.snapshot.get");
            }
            FixtureKind::VerdictRecordRequest => {
                assert_typed_request::<VerdictRecordParams>(&entry, &value, "verdict.record");
            }
            FixtureKind::VerdictRecordResponse => {
                assert_typed_response::<VerdictRecordResult>(&entry, &value, "verdict.record");
            }
            FixtureKind::EventsStreamEventNotification => {
                assert_typed_round_trip::<EventsStreamEventNotification>(&entry, &value);
            }
            FixtureKind::EventsStreamTerminalNotification => {
                assert_typed_round_trip::<EventsStreamTerminalNotification>(&entry, &value);
            }
            FixtureKind::WebSocketHelloRequest => {
                assert_typed_request::<HelloParams>(&entry, &value, "system.hello");
            }
            FixtureKind::WebSocketHelloResponse => {
                assert_typed_response::<HelloResult>(&entry, &value, "system.hello");
            }
            FixtureKind::RpcFailure => {
                let response: RpcResponse = serde_json::from_value(value.clone())
                    .unwrap_or_else(|error| panic!("invalid {}: {error}", entry.id));
                let error_value = value
                    .get("error")
                    .cloned()
                    .expect("failure fixture must contain error");
                assert_typed_round_trip::<RpcError>(&entry, &error_value);
                assert!(
                    response.error().is_some(),
                    "fixture {} must be a failure",
                    entry.id
                );
                assert_eq!(
                    serde_json::to_value(response).expect("reserialize failure response"),
                    value,
                    "fixture {} changed after RPC failure round trip",
                    entry.id
                );
            }
            FixtureKind::TestEvent => {
                assert_typed_round_trip::<TestEvent>(&entry, &value);
            }
        }
    }
}

#[test]
fn driver_failure_fixture_uses_the_driver_envelope_code() {
    let entry = read_manifest()
        .fixtures
        .into_iter()
        .find(|entry| entry.id == "rpc.failure.v1")
        .expect("driver failure fixture");
    let response: RpcResponse =
        serde_json::from_value(read_fixture(&entry)).expect("typed failure response");
    let error = response.error().expect("failure error");
    assert_eq!(error.code, -32000);
    assert_eq!(error.data.code, "device_unavailable");
}

fn read_event_fixtures() -> Vec<(String, TestEvent)> {
    read_manifest()
        .fixtures
        .into_iter()
        .filter(|entry| entry.kind == FixtureKind::TestEvent)
        .map(|entry| {
            let event = serde_json::from_value(read_fixture(&entry))
                .unwrap_or_else(|error| panic!("invalid {}: {error}", entry.id));
            (entry.id, event)
        })
        .collect()
}

#[test]
fn event_fixtures_cover_every_payload_and_action_outcome() {
    let mut variants = BTreeMap::new();
    let mut outcomes = BTreeMap::new();

    for (_, event) in read_event_fixtures() {
        let variant = match event.payload {
            TestEventPayload::SessionStarted => "sessionStarted",
            TestEventPayload::SessionEnded { .. } => "sessionEnded",
            TestEventPayload::ObservationCaptured { .. } => "observationCaptured",
            TestEventPayload::ActionStarted { .. } => "actionStarted",
            TestEventPayload::ActionCompleted { outcome, .. } => {
                let outcome = match outcome {
                    ActionOutcome::Succeeded { .. } => "succeeded",
                    ActionOutcome::Failed { .. } => "failed",
                    ActionOutcome::Cancelled { .. } => "cancelled",
                    ActionOutcome::TimedOut { .. } => "timedOut",
                };
                *outcomes.entry(outcome).or_insert(0_u8) += 1;
                "actionCompleted"
            }
            TestEventPayload::MediaStreamStarted { .. } => "mediaStreamStarted",
            TestEventPayload::MediaFrameCaptured { .. } => "mediaFrameCaptured",
            TestEventPayload::MediaStreamEnded { .. } => "mediaStreamEnded",
            TestEventPayload::VerdictRecorded { .. } => "verdictRecorded",
            TestEventPayload::Error { .. } => "error",
        };
        *variants.entry(variant).or_insert(0_u8) += 1;
    }

    assert_eq!(
        variants,
        BTreeMap::from([
            ("actionCompleted", 4),
            ("actionStarted", 4),
            ("error", 1),
            ("mediaFrameCaptured", 1),
            ("mediaStreamEnded", 1),
            ("mediaStreamStarted", 1),
            ("observationCaptured", 1),
            ("sessionEnded", 1),
            ("sessionStarted", 1),
            ("verdictRecorded", 1),
        ])
    );
    assert_eq!(
        outcomes,
        BTreeMap::from([
            ("cancelled", 1),
            ("failed", 1),
            ("succeeded", 1),
            ("timedOut", 1),
        ])
    );
}

#[test]
fn action_started_fixtures_cover_standard_and_explicitly_redacted_calls() {
    let mut standard = 0_u8;
    let mut redacted = 0_u8;

    for (fixture_id, event) in read_event_fixtures() {
        let TestEventPayload::ActionStarted { call } = event.payload else {
            continue;
        };
        if call.arguments_redacted {
            assert!(
                call.arguments.is_null(),
                "fixture {fixture_id} marks arguments redacted but still carries a value"
            );
            redacted += 1;
        } else {
            standard += 1;
        }
    }

    assert_eq!(standard, 3);
    assert_eq!(redacted, 1);
}

#[test]
fn event_fixtures_form_one_coherent_session_stream() {
    let mut fixtures = read_event_fixtures();
    fixtures.sort_by_key(|(_, event)| event.sequence);
    assert!(!fixtures.is_empty(), "event fixtures must not be empty");

    let session_id = fixtures[0].1.session_id.clone();
    let mut event_ids = BTreeSet::new();
    let mut started_calls = BTreeMap::new();
    let mut completed_calls = BTreeSet::new();
    let mut previous_at_ms = None;

    for (index, (fixture_id, event)) in fixtures.iter().enumerate() {
        assert_eq!(
            event.session_id, session_id,
            "fixture {fixture_id} belongs to a different session"
        );
        assert_eq!(
            event.sequence.get(),
            (index + 1) as u64,
            "fixture {fixture_id} breaks the continuous one-based sequence"
        );
        assert!(
            event_ids.insert(event.event_id.clone()),
            "fixture {fixture_id} reuses an event id"
        );
        if let Some(previous) = previous_at_ms {
            assert!(
                event.at_ms >= previous,
                "fixture {fixture_id} moves the event clock backwards"
            );
        }
        previous_at_ms = Some(event.at_ms);

        match &event.payload {
            TestEventPayload::ActionStarted { call } => {
                assert!(
                    started_calls
                        .insert(call.id, (event.request_id.clone(), event.device_id.clone()))
                        .is_none(),
                    "fixture {fixture_id} starts the same action twice"
                );
            }
            TestEventPayload::ActionCompleted { call_id, .. } => {
                let context = started_calls
                    .get(call_id)
                    .unwrap_or_else(|| panic!("fixture {fixture_id} completes an unknown action"));
                assert_eq!(
                    &(event.request_id.clone(), event.device_id.clone()),
                    context,
                    "fixture {fixture_id} must preserve request/device correlation"
                );
                assert!(
                    completed_calls.insert(*call_id),
                    "fixture {fixture_id} completes the same action twice"
                );
            }
            _ => {}
        }
    }

    assert!(
        matches!(&fixtures[0].1.payload, TestEventPayload::SessionStarted),
        "the stream must start with sessionStarted"
    );
    assert!(
        matches!(
            &fixtures.last().expect("event fixtures").1.payload,
            TestEventPayload::SessionEnded { .. }
        ),
        "the stream must end with sessionEnded"
    );
    assert_eq!(
        completed_calls.len(),
        started_calls.len(),
        "every started action must have exactly one terminal event"
    );
    assert!(
        fixtures.iter().any(|(_, event)| event.request_id.is_some())
            && fixtures.iter().any(|(_, event)| event.request_id.is_none()),
        "fixtures must cover present and omitted requestId"
    );
    assert!(
        fixtures.iter().any(|(_, event)| event.device_id.is_some())
            && fixtures.iter().any(|(_, event)| event.device_id.is_none()),
        "fixtures must cover present and omitted deviceId"
    );
}

#[test]
fn hello_request_and_response_are_one_coherent_exchange() {
    let manifest = read_manifest();
    let request_entries = manifest
        .fixtures
        .iter()
        .filter(|entry| entry.kind == FixtureKind::HelloRequest)
        .collect::<Vec<_>>();
    let response_entries = manifest
        .fixtures
        .iter()
        .filter(|entry| entry.kind == FixtureKind::HelloResponse)
        .collect::<Vec<_>>();
    assert_eq!(
        request_entries.len(),
        1,
        "exactly one hello request fixture"
    );
    assert_eq!(
        response_entries.len(),
        1,
        "exactly one hello response fixture"
    );
    let request_entry = request_entries[0];
    let response_entry = response_entries[0];

    let request: RpcRequest =
        serde_json::from_value(read_fixture(request_entry)).expect("typed hello request");
    let request_id = request.id.clone();
    let params: HelloParams =
        serde_json::from_value(request.params.expect("hello request params").into_value())
            .expect("typed hello params");
    let response: RpcResponse =
        serde_json::from_value(read_fixture(response_entry)).expect("typed hello response");
    let (response_id, result) = match response {
        RpcResponse::Success { id, result, .. } => (id, result),
        RpcResponse::Failure { .. } => panic!("hello fixture must be successful"),
    };
    let result: HelloResult = serde_json::from_value(result).expect("typed hello result");

    assert_eq!(
        response_id, request_id,
        "request and response id must match"
    );
    let server_protocol = supported_protocol_offer();
    assert_eq!(
        result.protocol.selected,
        negotiate_protocol(&params.protocol, &server_protocol)
            .expect("fixture protocol offers must be compatible")
    );
    let available_features = BTreeSet::from([
        feature::ACTION_PROTECTED_V1.to_owned(),
        feature::DEVICE_ROUTING_V1.to_owned(),
        feature::DEVICE_SEMANTIC_ACTIONS_V1.to_owned(),
        feature::EVENTS_SNAPSHOT_V1.to_owned(),
        feature::EVENTS_STREAM_V1.to_owned(),
        feature::MEDIA_STREAM_V1.to_owned(),
        feature::OBSERVATION_UI_SNAPSHOT_V1.to_owned(),
        feature::REQUEST_CONTROL_V1.to_owned(),
        feature::SESSION_EXPORT_PAGE_V1.to_owned(),
        feature::VERDICT_RECORD_V1.to_owned(),
    ]);
    assert_eq!(
        result.features,
        negotiate_features(&params.features, &available_features)
            .expect("fixture features must be compatible")
    );
}

#[test]
fn routing_requests_and_responses_are_coherent() {
    let manifest = read_manifest();
    let fixture = |id: &str| {
        manifest
            .fixtures
            .iter()
            .find(|entry| entry.id == id)
            .unwrap_or_else(|| panic!("missing routing fixture {id}"))
    };

    let list_request: RpcRequest =
        serde_json::from_value(read_fixture(fixture("rpc.devices-list.request.v1")))
            .expect("typed devices.list request");
    assert_eq!(list_request.method, "devices.list");
    assert!(list_request.params.is_none());
    let list_response: RpcResponse =
        serde_json::from_value(read_fixture(fixture("rpc.devices-list.response.v1")))
            .expect("typed devices.list response");
    let (list_response_id, list_result) = match list_response {
        RpcResponse::Success { id, result, .. } => (id, result),
        RpcResponse::Failure { .. } => panic!("devices.list fixture must be successful"),
    };
    assert_eq!(list_response_id, list_request.id);
    let list_result: DevicesListResult =
        serde_json::from_value(list_result).expect("typed devices.list result");
    assert!(
        list_result
            .devices
            .windows(2)
            .all(|pair| pair[0].id < pair[1].id),
        "listed devices must use stable DeviceId order"
    );
    let initially_selected = list_result
        .selected_device_id
        .as_ref()
        .expect("fixture has a selected device");
    assert!(
        list_result
            .devices
            .iter()
            .any(|device| &device.id == initially_selected),
        "selectedDeviceId must identify a listed device"
    );

    let select_request: RpcRequest =
        serde_json::from_value(read_fixture(fixture("rpc.device-select.request.v1")))
            .expect("typed device.select request");
    let select_params: DeviceSelectParams = serde_json::from_value(
        select_request
            .params
            .clone()
            .expect("device.select params")
            .into_value(),
    )
    .expect("typed device.select params");
    let select_response: RpcResponse =
        serde_json::from_value(read_fixture(fixture("rpc.device-select.response.v1")))
            .expect("typed device.select response");
    let (select_response_id, select_result) = match select_response {
        RpcResponse::Success { id, result, .. } => (id, result),
        RpcResponse::Failure { .. } => panic!("device.select fixture must be successful"),
    };
    assert_eq!(select_response_id, select_request.id);
    let select_result: DeviceSelectResult =
        serde_json::from_value(select_result).expect("typed device.select result");
    assert_eq!(select_result.device.id, select_params.device_id);
    assert!(
        list_result
            .devices
            .iter()
            .any(|device| device == &select_result.device),
        "device.select must return the selected listed device"
    );
}
