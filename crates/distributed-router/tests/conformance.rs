use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use devicerail_core::{
    DeviceDriver as _, DeviceRuntime, DriverError, EvidenceStore, ExecutionControl,
    MemoryEventStore, OperationContext, ScreenshotPolicy, SessionEventStore, StartSession, now_ms,
};
use devicerail_distributed_router::{
    DISTRIBUTED_PROTOCOL_VERSION, HealthState, InventorySnapshot, LeaseTable, MemoryTelemetry,
    NodeId, PeerOperation, PeerResponse, PeerResult, PeerSecurity, PeerTransport,
    RemoteDeviceDescriptor, RemoteDeviceDriver, RemoteDriverConfig, TelemetrySink, TransportError,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_protocol::{
    ActionCall, ActionDefinition, ActionExecution, ActionOutcome, ActionProtection, ActionResult,
    AssetRef, CLEAR_ELEMENT_ACTION, DeviceId, DeviceInfo, FIND_ELEMENT_ACTION, Observation,
    Platform, SET_ELEMENT_VALUE_ACTION, TAP_ELEMENT_ACTION, TestEventPayload,
    UI_SNAPSHOT_FORMAT_VERSION, UI_SNAPSHOT_MEDIA_TYPE, UiContextKind, UiContextRef, UiNode,
    UiNodeRef, UiRect, UiSnapshot, UiSnapshotRef, Viewport, WAIT_FOR_ELEMENT_ACTION,
    is_semantic_action_name,
};
use serde_json::json;
use uuid::Uuid;

const NODE_EPOCH: u64 = 7;
const DEVICE_KEY: &str = "phone-1";
const EVIDENCE_ID: &str = "screen-1";
const UI_EVIDENCE_ID: &str = "ui-tree-1";
const ONE_PIXEL_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

#[derive(Clone, Copy, Debug)]
enum SemanticFault {
    MalformedOutput,
    MissingExecution,
    InvalidExecutionContext,
    WrongObservation,
    WrongContext,
    MissingStableNode,
}

struct FakePeer {
    node_id: NodeId,
    security: PeerSecurity,
    leases: LeaseTable,
    connected: Mutex<bool>,
    evidence: Vec<u8>,
    ui_evidence: Vec<u8>,
    fail_execute_after_send: AtomicBool,
    execute_calls: AtomicUsize,
    feature_flags: Mutex<Vec<(&'static str, bool, bool)>>,
    semantic_fault: Option<SemanticFault>,
    reused_ui_snapshot_mismatch: bool,
    ui_snapshot_during_omission: bool,
    ui_reference_byte_length: Option<u64>,
    ui_evidence_reads: AtomicUsize,
    ui_remote_sha256: Option<String>,
    ui_chunk_sha256: Option<String>,
    reused_ui_sha_mismatch: bool,
}

impl FakePeer {
    fn new() -> Self {
        Self {
            node_id: NodeId::parse("lab-a").expect("node id"),
            security: PeerSecurity::external_tunnel("test-ssh").expect("security"),
            leases: LeaseTable::new(NODE_EPOCH, [DEVICE_KEY.to_owned()]).expect("leases"),
            connected: Mutex::new(false),
            evidence: BASE64.decode(ONE_PIXEL_PNG).expect("fixture PNG"),
            ui_evidence: ui_snapshot_bytes(),
            fail_execute_after_send: AtomicBool::new(false),
            execute_calls: AtomicUsize::new(0),
            feature_flags: Mutex::new(Vec::new()),
            semantic_fault: None,
            reused_ui_snapshot_mismatch: false,
            ui_snapshot_during_omission: false,
            ui_reference_byte_length: None,
            ui_evidence_reads: AtomicUsize::new(0),
            ui_remote_sha256: None,
            ui_chunk_sha256: None,
            reused_ui_sha_mismatch: false,
        }
    }

    fn with_semantic_fault(mut self, fault: SemanticFault) -> Self {
        self.semantic_fault = Some(fault);
        self
    }

    fn with_ui_evidence(mut self, ui_evidence: Vec<u8>) -> Self {
        self.ui_evidence = ui_evidence;
        self
    }

    fn with_reused_ui_snapshot_mismatch(mut self) -> Self {
        self.reused_ui_snapshot_mismatch = true;
        self
    }

    fn with_ui_snapshot_during_omission(mut self) -> Self {
        self.ui_snapshot_during_omission = true;
        self
    }

    fn with_ui_reference_byte_length(mut self, byte_length: u64) -> Self {
        self.ui_reference_byte_length = Some(byte_length);
        self
    }

    fn with_ui_digests(mut self, remote: &str, chunk: &str) -> Self {
        self.ui_remote_sha256 = Some(remote.to_owned());
        self.ui_chunk_sha256 = Some(chunk.to_owned());
        self
    }

    fn with_reused_ui_sha_mismatch(mut self) -> Self {
        self.reused_ui_sha_mismatch = true;
        self
    }

    fn descriptor(&self) -> RemoteDeviceDescriptor {
        descriptor()
    }

    fn observation(
        &self,
        omission: Option<devicerail_protocol::ScreenshotOmissionReason>,
        ui_snapshots_enabled: bool,
    ) -> Observation {
        let observation_id = Uuid::from_u128(1);
        let force_ui_snapshot = self.ui_snapshot_during_omission && omission.is_some();
        let ui_snapshot = ui_snapshots_enabled.then(|| UiSnapshotRef {
            format_version: UI_SNAPSHOT_FORMAT_VERSION,
            context: ui_context(),
            node_count: 1,
            byte_length: self
                .ui_reference_byte_length
                .unwrap_or(self.ui_evidence.len() as u64),
            evidence: AssetRef {
                id: UI_EVIDENCE_ID.into(),
                media_type: UI_SNAPSHOT_MEDIA_TYPE.into(),
                uri: "peer:evidence/ui-tree-1".into(),
                sha256: self.ui_remote_sha256.clone(),
            },
        });
        Observation {
            id: observation_id,
            device_id: DeviceId::new(DEVICE_KEY),
            captured_at_ms: now_ms(),
            viewport: Viewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
            screenshot: omission.is_none().then(remote_asset),
            screenshot_omission: omission,
            ui_snapshot: if omission.is_none() || force_ui_snapshot {
                ui_snapshot
            } else {
                None
            },
            ui_snapshot_omission: if ui_snapshots_enabled && !force_ui_snapshot {
                omission.map(|omission| match omission {
                    devicerail_protocol::ScreenshotOmissionReason::Policy => {
                        devicerail_protocol::UiSnapshotOmissionReason::Policy
                    }
                    devicerail_protocol::ScreenshotOmissionReason::ProtectedAction => {
                        devicerail_protocol::UiSnapshotOmissionReason::ProtectedAction
                    }
                })
            } else {
                None
            },
            metadata: Default::default(),
        }
    }

    fn authorize(
        &self,
        request: &devicerail_distributed_router::PeerRequest,
    ) -> Result<(), TransportError> {
        let lease = request.lease.as_ref().ok_or(TransportError::Protocol)?;
        self.leases
            .authorize(lease, now_ms())
            .map_err(|_| TransportError::Protocol)
    }
}

#[async_trait]
impl PeerTransport for FakePeer {
    fn expected_node_id(&self) -> &NodeId {
        &self.node_id
    }

    fn security(&self) -> &PeerSecurity {
        &self.security
    }

    async fn request(
        &self,
        request: devicerail_distributed_router::PeerRequest,
        control: &ExecutionControl,
    ) -> Result<PeerResponse, TransportError> {
        if control.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if control.is_expired() {
            return Err(TransportError::TimedOut);
        }
        request.validate().map_err(|_| TransportError::Protocol)?;
        let result = match request.operation.clone() {
            PeerOperation::Hello => PeerResult::Hello {
                node_id: self.node_id.clone(),
                epoch: NODE_EPOCH,
                max_frame_bytes: devicerail_distributed_router::MAX_PEER_FRAME_BYTES as u32,
                capabilities: devicerail_distributed_router::PeerProtocolCapabilities::REQUIRED,
            },
            PeerOperation::Inventory => PeerResult::Inventory {
                inventory: InventorySnapshot {
                    node_id: self.node_id.clone(),
                    epoch: NODE_EPOCH,
                    revision: 1,
                    generated_at_ms: now_ms(),
                    devices: vec![self.descriptor()],
                },
            },
            PeerOperation::Health => PeerResult::Health {
                state: HealthState::Healthy,
                checked_at_ms: now_ms(),
            },
            PeerOperation::Capabilities { .. } => PeerResult::Capabilities {
                actions: capabilities(),
            },
            PeerOperation::LeaseAcquire {
                device_key,
                owner_id,
                ttl_ms,
            } => PeerResult::Lease {
                lease: self
                    .leases
                    .acquire(&device_key, &owner_id, ttl_ms, now_ms())
                    .map_err(|_| TransportError::Protocol)?,
            },
            PeerOperation::LeaseRenew { lease, ttl_ms } => {
                self.authorize(&request)?;
                PeerResult::Lease {
                    lease: self
                        .leases
                        .renew(&lease, ttl_ms, now_ms())
                        .map_err(|_| TransportError::Protocol)?,
                }
            }
            PeerOperation::LeaseRelease { lease } => {
                self.authorize(&request)?;
                self.leases
                    .release(&lease, now_ms())
                    .map_err(|_| TransportError::Protocol)?;
                PeerResult::Ack
            }
            PeerOperation::Connect { .. } => {
                self.authorize(&request)?;
                *self
                    .connected
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
                PeerResult::Device {
                    device: DeviceInfo {
                        id: DeviceId::new(DEVICE_KEY),
                        name: "Fixture phone".into(),
                        platform: Platform::Android,
                        os_version: Some("15".into()),
                        connected: true,
                    },
                }
            }
            PeerOperation::Disconnect { .. } => {
                self.authorize(&request)?;
                *self
                    .connected
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
                PeerResult::Ack
            }
            PeerOperation::Observe {
                screenshot_omission,
                ui_snapshots_enabled,
                semantic_actions_enabled,
                ..
            } => {
                self.feature_flags
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(("observe", ui_snapshots_enabled, semantic_actions_enabled));
                self.authorize(&request)?;
                if !*self
                    .connected
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                {
                    return Err(TransportError::Protocol);
                }
                PeerResult::Observation {
                    observation: Box::new(
                        self.observation(screenshot_omission, ui_snapshots_enabled),
                    ),
                }
            }
            PeerOperation::Execute {
                call,
                screenshot_omission,
                ui_snapshots_enabled,
                semantic_actions_enabled,
                ..
            } => {
                self.feature_flags
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(("execute", ui_snapshots_enabled, semantic_actions_enabled));
                self.execute_calls.fetch_add(1, Ordering::SeqCst);
                if self.fail_execute_after_send.load(Ordering::SeqCst) {
                    return Err(TransportError::FailedAfterSend);
                }
                self.authorize(&request)?;
                if !*self
                    .connected
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                {
                    return Err(TransportError::Protocol);
                }
                let started_at_ms = now_ms();
                let before = self.observation(screenshot_omission, ui_snapshots_enabled);
                let mut after = self.observation(screenshot_omission, ui_snapshots_enabled);
                if self.reused_ui_snapshot_mismatch && after.ui_snapshot.is_some() {
                    after.id = Uuid::from_u128(2);
                }
                if self.reused_ui_sha_mismatch
                    && let Some(snapshot) = after.ui_snapshot.as_mut()
                {
                    snapshot.evidence.sha256 = Some("0".repeat(64));
                }
                let semantic = is_semantic_action_name(&call.name);
                let mut node = UiNodeRef {
                    observation_id: Uuid::from_u128(1),
                    context: ui_context(),
                    stable_node_id: "root".into(),
                };
                if semantic {
                    match self.semantic_fault {
                        Some(SemanticFault::WrongObservation) => {
                            node.observation_id = Uuid::from_u128(2);
                        }
                        Some(SemanticFault::WrongContext) => {
                            node.context.document_epoch = "another-epoch".into();
                        }
                        Some(SemanticFault::MissingStableNode) => {
                            node.stable_node_id = "absent".into();
                        }
                        _ => {}
                    }
                }
                let mut output = match call.name.as_str() {
                    "inputSecret" => call.arguments.clone(),
                    FIND_ELEMENT_ACTION
                    | TAP_ELEMENT_ACTION
                    | CLEAR_ELEMENT_ACTION
                    | SET_ELEMENT_VALUE_ACTION => json!({"element": node}),
                    WAIT_FOR_ELEMENT_ACTION => json!({
                        "matched": true,
                        "condition": "present",
                        "element": node,
                    }),
                    _ => json!({"accepted": true}),
                };
                let mut execution = semantic.then(|| ActionExecution::NativeSemantic {
                    context: ui_context(),
                });
                if semantic {
                    match self.semantic_fault {
                        Some(SemanticFault::MalformedOutput) => {
                            output = json!({"element": {"unexpected": true}});
                        }
                        Some(SemanticFault::MissingExecution) => execution = None,
                        Some(SemanticFault::InvalidExecutionContext) => {
                            let mut context = ui_context();
                            context.context_kind = UiContextKind::Web;
                            execution = Some(ActionExecution::NativeSemantic { context });
                        }
                        _ => {}
                    }
                }
                PeerResult::Action {
                    result: Box::new(ActionResult {
                        call_id: call.id,
                        started_at_ms,
                        finished_at_ms: now_ms().max(started_at_ms),
                        output,
                        before: Some(before),
                        after: Some(after),
                        evidence: if screenshot_omission.is_none() {
                            vec![remote_asset()]
                        } else {
                            Vec::new()
                        },
                        execution,
                    }),
                }
            }
            PeerOperation::EvidenceRead {
                evidence_id,
                offset,
                max_bytes,
                ..
            } => {
                self.authorize(&request)?;
                if evidence_id == UI_EVIDENCE_ID {
                    self.ui_evidence_reads.fetch_add(1, Ordering::SeqCst);
                }
                let evidence = match evidence_id.as_str() {
                    EVIDENCE_ID => &self.evidence,
                    UI_EVIDENCE_ID => &self.ui_evidence,
                    _ => return Err(TransportError::Protocol),
                };
                if offset as usize > evidence.len() {
                    return Err(TransportError::Protocol);
                }
                let end = (offset as usize + max_bytes as usize).min(evidence.len());
                let chunk = &evidence[offset as usize..end];
                let sha256 = if evidence_id == UI_EVIDENCE_ID {
                    self.ui_chunk_sha256.clone()
                } else {
                    None
                };
                PeerResult::EvidenceChunk {
                    media_type: if evidence_id == EVIDENCE_ID {
                        "image/png".into()
                    } else {
                        UI_SNAPSHOT_MEDIA_TYPE.into()
                    },
                    evidence_id,
                    total_size: evidence.len() as u64,
                    offset,
                    data_base64: BASE64.encode(chunk),
                    sha256,
                    done: end == evidence.len(),
                }
            }
            PeerOperation::Cancel { .. } => PeerResult::Ack,
        };
        let response = PeerResponse::success(&request, NODE_EPOCH, result);
        assert_eq!(response.protocol_version, DISTRIBUTED_PROTOCOL_VERSION);
        Ok(response)
    }
}

fn descriptor() -> RemoteDeviceDescriptor {
    RemoteDeviceDescriptor {
        device_key: DEVICE_KEY.into(),
        name: "Fixture phone".into(),
        platform: Platform::Android,
        os_version: Some("15".into()),
    }
}

fn ui_context() -> UiContextRef {
    UiContextRef {
        context_kind: UiContextKind::Native,
        context_id: "NATIVE_APP".into(),
        document_epoch: "fixture-epoch-1".into(),
    }
}

fn ui_snapshot_bytes() -> Vec<u8> {
    serde_json::to_vec(&ui_snapshot()).expect("canonical UI Snapshot fixture")
}

fn ui_snapshot() -> UiSnapshot {
    UiSnapshot {
        format_version: UI_SNAPSHOT_FORMAT_VERSION,
        observation_id: Uuid::from_u128(1),
        context: ui_context(),
        root_stable_node_ids: vec!["root".into()],
        nodes: vec![UiNode {
            stable_node_id: "root".into(),
            parent_stable_node_id: None,
            role: "application".into(),
            name: Some("Fixture".into()),
            value: None,
            identifier: Some("fixture-root".into()),
            text: None,
            bounds: Some(UiRect {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
            }),
            enabled: Some(true),
            hittable: Some(true),
        }],
    }
}

fn capabilities() -> Vec<ActionDefinition> {
    let mut actions = vec![ActionDefinition {
        name: "tap".into(),
        description: "Tap one fixture point".into(),
        protection: ActionProtection::Standard,
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "required": ["x", "y"],
            "properties": {
                "x": {"type": "integer", "minimum": 0, "maximum": 1},
                "y": {"type": "integer", "minimum": 0, "maximum": 1}
            }
        }),
    }];
    actions.extend(
        [
            FIND_ELEMENT_ACTION,
            TAP_ELEMENT_ACTION,
            CLEAR_ELEMENT_ACTION,
            SET_ELEMENT_VALUE_ACTION,
            WAIT_FOR_ELEMENT_ACTION,
        ]
        .into_iter()
        .map(|name| ActionDefinition {
            name: name.into(),
            description: format!("Canonical fixture action {name}"),
            protection: ActionProtection::Standard,
            input_schema: canonical_semantic_schema(name),
        }),
    );
    actions.push(ActionDefinition {
        name: "inputSecret".into(),
        description: "Enter a protected value".into(),
        protection: ActionProtection::Protected,
        input_schema: json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "required": ["text"],
            "properties": {
                "text": {"type": "string", "minLength": 1, "maxLength": 128}
            }
        }),
    });
    actions
}

fn canonical_semantic_schema(name: &str) -> serde_json::Value {
    let source = match name {
        FIND_ELEMENT_ACTION => {
            include_str!("../../../protocol/schema/v1/find-element-arguments.schema.json")
        }
        TAP_ELEMENT_ACTION => {
            include_str!("../../../protocol/schema/v1/tap-element-arguments.schema.json")
        }
        CLEAR_ELEMENT_ACTION => {
            include_str!("../../../protocol/schema/v1/clear-element-arguments.schema.json")
        }
        SET_ELEMENT_VALUE_ACTION => {
            include_str!("../../../protocol/schema/v1/set-element-value-arguments.schema.json")
        }
        WAIT_FOR_ELEMENT_ACTION => {
            include_str!("../../../protocol/schema/v1/wait-for-element-arguments.schema.json")
        }
        _ => panic!("no canonical schema fixture for {name}"),
    };
    serde_json::from_str(source).expect("generated semantic schema")
}

fn remote_asset() -> AssetRef {
    AssetRef {
        id: EVIDENCE_ID.into(),
        media_type: "image/png".into(),
        uri: "peer:evidence/screen-1".into(),
        sha256: None,
    }
}

fn fixture_driver() -> RemoteDeviceDriver {
    fixture_driver_with_peer(Arc::new(FakePeer::new()))
}

fn fixture_driver_with_peer(peer: Arc<FakePeer>) -> RemoteDeviceDriver {
    let transport: Arc<dyn PeerTransport> = peer;
    let telemetry: Arc<dyn TelemetrySink> = Arc::new(MemoryTelemetry::default());
    RemoteDeviceDriver::from_contract(
        NodeId::parse("lab-a").expect("node"),
        NODE_EPOCH,
        descriptor(),
        "conformance-owner",
        RemoteDriverConfig::default(),
        transport,
        telemetry,
        capabilities(),
    )
    .expect("driver")
}

fn valid_call(action: &ActionDefinition) -> Result<ActionCall, String> {
    let arguments = match action.name.as_str() {
        "tap" => json!({"x": 1, "y": 1}),
        FIND_ELEMENT_ACTION => json!({"selector": {"role": "application"}}),
        TAP_ELEMENT_ACTION | CLEAR_ELEMENT_ACTION => {
            json!({"target": {"kind": "selector", "selector": {"role": "application"}}})
        }
        SET_ELEMENT_VALUE_ACTION => json!({
            "target": {"kind": "selector", "selector": {"role": "application"}},
            "value": "updated",
        }),
        WAIT_FOR_ELEMENT_ACTION => {
            json!({"selector": {"role": "application"}, "condition": "present"})
        }
        "inputSecret" => json!({"text": "fixture-secret"}),
        _ => return Err(format!("no fixture for {}", action.name)),
    };
    Ok(ActionCall {
        id: Uuid::new_v4(),
        name: action.name.clone(),
        arguments,
    })
}

fn evidence_store() -> Arc<dyn EvidenceStore> {
    let root = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(
        FileEvidenceStore::new(root, FileEvidenceStoreConfig::default()).expect("evidence store"),
    )
}

devicerail_core::driver_conformance_test!(
    remote_driver_conforms_to_shared_contract,
    fixture_driver,
    valid_call,
    evidence_store(),
);

#[tokio::test]
async fn stable_namespaced_id_never_exposes_an_ambiguous_device_key() {
    let driver = fixture_driver();
    assert_eq!(driver.id(), &DeviceId::new("remote:lab-a:phone-1"));
    assert!(!driver.device_info().await.connected);
}

#[test]
fn semantic_capabilities_require_the_exact_generated_schema() {
    let peer = Arc::new(FakePeer::new());
    let transport: Arc<dyn PeerTransport> = peer;
    let telemetry: Arc<dyn TelemetrySink> = Arc::new(MemoryTelemetry::default());
    let mut actions = capabilities();
    let find = actions
        .iter_mut()
        .find(|action| action.name == FIND_ELEMENT_ACTION)
        .expect("findElement capability");
    find.input_schema["title"] = json!("PeerSpecificFindElementArguments");

    let error = match RemoteDeviceDriver::from_contract(
        NodeId::parse("lab-a").expect("node"),
        NODE_EPOCH,
        descriptor(),
        "conformance-owner",
        RemoteDriverConfig::default(),
        transport,
        telemetry,
        actions,
    ) {
        Ok(_) => panic!("non-canonical semantic schema must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(error, DriverError::Protocol(_)));
}

#[tokio::test]
async fn every_semantic_argument_dto_is_validated_locally_before_peer_dispatch() {
    let peer = Arc::new(FakePeer::new());
    let driver = Arc::new(fixture_driver_with_peer(Arc::clone(&peer)));
    let events = Arc::new(MemoryEventStore::default());
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store());
    runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("session");
    for (name, arguments) in [
        (FIND_ELEMENT_ACTION, json!({"selector": {}})),
        (
            TAP_ELEMENT_ACTION,
            json!({"target": {"kind": "selector", "selector": {}}}),
        ),
        (
            CLEAR_ELEMENT_ACTION,
            json!({"target": {"kind": "selector", "selector": {}}}),
        ),
        (
            SET_ELEMENT_VALUE_ACTION,
            json!({"target": {"kind": "selector", "selector": {}}, "value": "x"}),
        ),
        (WAIT_FOR_ELEMENT_ACTION, json!({"selector": {}})),
    ] {
        let error = runtime
            .execute(
                &OperationContext::new(session.id.clone(), None)
                    .with_ui_snapshots_enabled(true)
                    .with_semantic_actions_enabled(true),
                ActionCall {
                    id: Uuid::new_v4(),
                    name: name.to_owned(),
                    arguments,
                },
            )
            .await
            .expect_err("empty selector must fail local semantic validation");
        let public = error.to_error_info();
        assert_eq!(public.code, "invalid_arguments", "{name}");
        assert_eq!(public.details.expect("details")["action"], name, "{name}");
    }
    assert_eq!(peer.execute_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn malformed_semantic_results_fail_closed_before_success_is_recorded() {
    for fault in [
        SemanticFault::MalformedOutput,
        SemanticFault::MissingExecution,
        SemanticFault::InvalidExecutionContext,
        SemanticFault::WrongObservation,
        SemanticFault::WrongContext,
        SemanticFault::MissingStableNode,
    ] {
        let peer = Arc::new(FakePeer::new().with_semantic_fault(fault));
        let driver = Arc::new(fixture_driver_with_peer(peer));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::with_evidence(
            Arc::clone(&driver),
            Arc::clone(&events),
            evidence_store(),
        );
        runtime
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect");
        let session = events
            .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
            .await
            .expect("session");
        let error = runtime
            .execute(
                &OperationContext::new(session.id.clone(), None)
                    .with_ui_snapshots_enabled(true)
                    .with_semantic_actions_enabled(true),
                ActionCall {
                    id: Uuid::new_v4(),
                    name: FIND_ELEMENT_ACTION.into(),
                    arguments: json!({"selector": {"role": "application"}}),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_error_info().code, "protocol_error", "{fault:?}");

        let recorded = events
            .list_after(&session.id, None)
            .await
            .expect("recorded events");
        assert!(
            recorded.iter().any(|event| matches!(
                &event.payload,
                TestEventPayload::ActionCompleted {
                    outcome: ActionOutcome::Failed { .. },
                    ..
                }
            )),
            "{fault:?} must record a failed terminal event"
        );
        assert!(
            !recorded.iter().any(|event| matches!(
                &event.payload,
                TestEventPayload::ActionCompleted {
                    outcome: ActionOutcome::Succeeded { .. },
                    ..
                }
            )),
            "{fault:?} must never record success"
        );
    }
}

#[tokio::test]
async fn malformed_or_mismatched_ui_snapshot_bodies_never_record_an_observation() {
    let mut wrong_observation = ui_snapshot();
    wrong_observation.observation_id = Uuid::from_u128(2);
    let mut wrong_context = ui_snapshot();
    wrong_context.context.document_epoch = "wrong-epoch".into();
    let mut wrong_count = ui_snapshot();
    wrong_count.root_stable_node_ids.push("second-root".into());
    wrong_count.nodes.push(UiNode {
        stable_node_id: "second-root".into(),
        parent_stable_node_id: None,
        role: "group".into(),
        name: None,
        value: None,
        identifier: None,
        text: None,
        bounds: None,
        enabled: None,
        hittable: None,
    });
    let mut invalid_preorder = ui_snapshot();
    invalid_preorder.nodes.push(UiNode {
        stable_node_id: "orphan".into(),
        parent_stable_node_id: Some("missing-parent".into()),
        role: "group".into(),
        name: None,
        value: None,
        identifier: None,
        text: None,
        bounds: None,
        enabled: None,
        hittable: None,
    });
    let cases = [
        ("malformed JSON", b"{".to_vec()),
        (
            "wrong observation",
            serde_json::to_vec(&wrong_observation).expect("fixture"),
        ),
        (
            "wrong context",
            serde_json::to_vec(&wrong_context).expect("fixture"),
        ),
        (
            "wrong node count",
            serde_json::to_vec(&wrong_count).expect("fixture"),
        ),
        (
            "invalid preorder",
            serde_json::to_vec(&invalid_preorder).expect("fixture"),
        ),
    ];

    for (case, bytes) in cases {
        let peer = Arc::new(FakePeer::new().with_ui_evidence(bytes));
        let driver = Arc::new(fixture_driver_with_peer(peer));
        let events = Arc::new(MemoryEventStore::default());
        let runtime = DeviceRuntime::with_evidence(
            Arc::clone(&driver),
            Arc::clone(&events),
            evidence_store(),
        );
        runtime
            .connect(&ExecutionControl::unbounded())
            .await
            .expect("connect");
        let session = events
            .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
            .await
            .expect("session");
        let error = runtime
            .observe(
                &OperationContext::new(session.id.clone(), None).with_ui_snapshots_enabled(true),
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_error_info().code, "protocol_error", "{case}");
        let recorded = events
            .list_after(&session.id, None)
            .await
            .expect("recorded events");
        assert!(
            !recorded.iter().any(|event| matches!(
                &event.payload,
                TestEventPayload::ObservationCaptured { .. }
            )),
            "{case} must not record ObservationCaptured"
        );
    }
}

#[tokio::test]
async fn repeated_ui_evidence_is_revalidated_for_every_observation_reference() {
    let peer = Arc::new(FakePeer::new().with_reused_ui_snapshot_mismatch());
    let driver = Arc::new(fixture_driver_with_peer(peer));
    let events = Arc::new(MemoryEventStore::default());
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store());
    runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("session");
    let error = runtime
        .execute(
            &OperationContext::new(session.id.clone(), None).with_ui_snapshots_enabled(true),
            ActionCall {
                id: Uuid::new_v4(),
                name: "tap".into(),
                arguments: json!({"x": 1, "y": 1}),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.to_error_info().code, "protocol_error");
    let recorded = events
        .list_after(&session.id, None)
        .await
        .expect("recorded events");
    assert!(recorded.iter().any(|event| matches!(
        &event.payload,
        TestEventPayload::ActionCompleted {
            outcome: ActionOutcome::Failed { .. },
            ..
        }
    )));
    assert!(!recorded.iter().any(|event| matches!(
        &event.payload,
        TestEventPayload::ActionCompleted {
            outcome: ActionOutcome::Succeeded { .. },
            ..
        }
    )));
}

#[tokio::test]
async fn evidence_digest_claims_are_consistent_on_first_import_and_reuse() {
    let remote_digest = "1".repeat(64);
    let chunk_digest = "2".repeat(64);
    let conflict_peer =
        Arc::new(FakePeer::new().with_ui_digests(remote_digest.as_str(), chunk_digest.as_str()));
    let conflict_driver = Arc::new(fixture_driver_with_peer(Arc::clone(&conflict_peer)));
    let conflict_events = Arc::new(MemoryEventStore::default());
    let conflict_runtime = DeviceRuntime::with_evidence(
        Arc::clone(&conflict_driver),
        Arc::clone(&conflict_events),
        evidence_store(),
    );
    conflict_runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = conflict_events
        .start_session(StartSession::new(
            None,
            Some(conflict_driver.id().clone()),
            now_ms(),
        ))
        .await
        .expect("session");
    conflict_runtime
        .observe(&OperationContext::new(session.id, None).with_ui_snapshots_enabled(true))
        .await
        .expect_err("AssetRef and chunk digest conflict must fail");
    assert_eq!(conflict_peer.ui_evidence_reads.load(Ordering::SeqCst), 1);

    let reuse_peer = Arc::new(FakePeer::new().with_reused_ui_sha_mismatch());
    let reuse_driver = Arc::new(fixture_driver_with_peer(Arc::clone(&reuse_peer)));
    let reuse_events = Arc::new(MemoryEventStore::default());
    let reuse_runtime = DeviceRuntime::with_evidence(
        Arc::clone(&reuse_driver),
        Arc::clone(&reuse_events),
        evidence_store(),
    );
    reuse_runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = reuse_events
        .start_session(StartSession::new(
            None,
            Some(reuse_driver.id().clone()),
            now_ms(),
        ))
        .await
        .expect("session");
    reuse_runtime
        .execute(
            &OperationContext::new(session.id, None).with_ui_snapshots_enabled(true),
            ActionCall {
                id: Uuid::new_v4(),
                name: "tap".into(),
                arguments: json!({"x": 1, "y": 1}),
            },
        )
        .await
        .expect_err("reused evidence id with a new digest must fail");
    assert_eq!(reuse_peer.ui_evidence_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ui_omission_and_declared_length_fail_at_the_earliest_remote_boundary() {
    let policy_peer = Arc::new(FakePeer::new().with_ui_snapshot_during_omission());
    let policy_driver = Arc::new(fixture_driver_with_peer(Arc::clone(&policy_peer)));
    let policy_events = Arc::new(MemoryEventStore::default());
    let policy_runtime = DeviceRuntime::with_evidence(
        Arc::clone(&policy_driver),
        Arc::clone(&policy_events),
        evidence_store(),
    );
    policy_runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = policy_events
        .start_session(StartSession::new(
            None,
            Some(policy_driver.id().clone()),
            now_ms(),
        ))
        .await
        .expect("session");
    policy_runtime
        .observe(
            &OperationContext::new(session.id, None)
                .with_screenshot_policy(ScreenshotPolicy::Omit)
                .with_ui_snapshots_enabled(true),
        )
        .await
        .expect_err("policy omission cannot carry a UI Snapshot");
    assert_eq!(policy_peer.ui_evidence_reads.load(Ordering::SeqCst), 0);

    let actual_length = ui_snapshot_bytes().len() as u64;
    let length_peer =
        Arc::new(FakePeer::new().with_ui_reference_byte_length(actual_length.saturating_add(1)));
    let length_driver = Arc::new(fixture_driver_with_peer(Arc::clone(&length_peer)));
    let length_events = Arc::new(MemoryEventStore::default());
    let length_runtime = DeviceRuntime::with_evidence(
        Arc::clone(&length_driver),
        Arc::clone(&length_events),
        evidence_store(),
    );
    length_runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = length_events
        .start_session(StartSession::new(
            None,
            Some(length_driver.id().clone()),
            now_ms(),
        ))
        .await
        .expect("session");
    length_runtime
        .observe(&OperationContext::new(session.id, None).with_ui_snapshots_enabled(true))
        .await
        .expect_err("first chunk totalSize must match byteLength");
    assert_eq!(length_peer.ui_evidence_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn operation_scoped_ui_feature_gates_are_forwarded_without_downgrade() {
    let peer = Arc::new(FakePeer::new());
    let driver = Arc::new(fixture_driver_with_peer(Arc::clone(&peer)));
    let events = Arc::new(MemoryEventStore::default());
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store());
    runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("session");
    let context = OperationContext::new(session.id, None)
        .with_ui_snapshots_enabled(true)
        .with_semantic_actions_enabled(true);

    let observation = runtime.observe(&context).await.expect("remote observation");
    let snapshot = observation
        .ui_snapshot
        .expect("normalized UI Snapshot reference survived the peer hop");
    assert_eq!(snapshot.context, ui_context());
    assert_eq!(snapshot.node_count, 1);
    assert_eq!(snapshot.byte_length, peer.ui_evidence.len() as u64);
    assert_eq!(snapshot.evidence.media_type, UI_SNAPSHOT_MEDIA_TYPE);
    runtime
        .execute(
            &context,
            ActionCall {
                id: Uuid::new_v4(),
                name: "tap".into(),
                arguments: json!({"x": 1, "y": 1}),
            },
        )
        .await
        .expect("remote action");

    assert_eq!(
        *peer
            .feature_flags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![("observe", true, true), ("execute", true, true)]
    );
}

#[tokio::test]
async fn execute_is_never_retried_and_ambiguous_delivery_is_explicit() {
    let peer = Arc::new(FakePeer::new());
    peer.fail_execute_after_send.store(true, Ordering::SeqCst);
    let driver = Arc::new(fixture_driver_with_peer(Arc::clone(&peer)));
    let events = Arc::new(MemoryEventStore::default());
    let evidence = evidence_store();
    let runtime = DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence);
    runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("session");
    let error = runtime
        .execute(
            &OperationContext::new(session.id, None),
            ActionCall {
                id: Uuid::new_v4(),
                name: "tap".into(),
                arguments: json!({"x": 1, "y": 1}),
            },
        )
        .await
        .expect_err("ambiguous execute must fail");
    let public = error.to_error_info();
    assert_eq!(public.code, "platform_error");
    assert_eq!(
        public.details.expect("details")["platformCode"],
        "remote_execute_outcome_unknown"
    );
    assert_eq!(peer.execute_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn protected_remote_output_and_evidence_are_fail_closed() {
    let driver = Arc::new(fixture_driver());
    let events = Arc::new(MemoryEventStore::default());
    let runtime =
        DeviceRuntime::with_evidence(Arc::clone(&driver), Arc::clone(&events), evidence_store());
    runtime
        .connect(&ExecutionControl::unbounded())
        .await
        .expect("connect");
    let session = events
        .start_session(StartSession::new(None, Some(driver.id().clone()), now_ms()))
        .await
        .expect("session");
    let result = runtime
        .execute(
            &OperationContext::new(session.id, None),
            ActionCall {
                id: Uuid::new_v4(),
                name: "inputSecret".into(),
                arguments: json!({"text": "REMOTE-SECRET-SENTINEL"}),
            },
        )
        .await
        .expect("protected execute");
    assert_eq!(result.output, json!({"accepted": true}));
    assert!(result.evidence.is_empty());
    assert!(
        !serde_json::to_string(&result)
            .expect("result")
            .contains("REMOTE-SECRET-SENTINEL")
    );
}
