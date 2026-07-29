use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use async_trait::async_trait;
use devicerail_core::{CancellationReason, ExecutionControl, ExecutionController, now_ms};
use devicerail_distributed_router::{
    HealthState, InventorySnapshot, MemoryTelemetry, NdjsonPeerTransport, NodeId, NodeRouter,
    PeerOperation, PeerProtocolCapabilities, PeerRequest, PeerResponse, PeerResult, PeerSecurity,
    PeerTransport, RemoteDeviceDescriptor, RemoteDriverConfig, RemoteNode, RouteError,
    RouterConfig, ShardedPeerTransport, TelemetrySink, TransportError,
};
use devicerail_protocol::{ActionDefinition, ActionProtection, DeviceId, Platform};
use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufStream};

struct ConcurrentShardTransport {
    node_id: NodeId,
    security: PeerSecurity,
    barrier: Arc<tokio::sync::Barrier>,
    requests: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl PeerTransport for ConcurrentShardTransport {
    fn expected_node_id(&self) -> &NodeId {
        &self.node_id
    }

    fn security(&self) -> &PeerSecurity {
        &self.security
    }

    async fn request(
        &self,
        request: PeerRequest,
        _: &ExecutionControl,
    ) -> Result<PeerResponse, TransportError> {
        let PeerOperation::Capabilities { device_key } = &request.operation else {
            return Err(TransportError::Protocol);
        };
        self.requests
            .lock()
            .expect("shard request log")
            .push(device_key.clone());
        self.barrier.wait().await;
        Ok(PeerResponse::success(
            &request,
            7,
            PeerResult::Capabilities {
                actions: vec![ActionDefinition {
                    name: "tap".to_owned(),
                    description: "Tap".to_owned(),
                    protection: ActionProtection::Standard,
                    input_schema: json!({ "type": "object" }),
                }],
            },
        ))
    }
}

#[derive(Clone)]
struct DiscoveryTransport {
    node_id: NodeId,
    security: PeerSecurity,
    epoch: u64,
    revision: u64,
    generated_at_ms: u64,
    checked_at_ms: u64,
    state: HealthState,
    platform: Platform,
    protocol_capabilities: PeerProtocolCapabilities,
}

impl DiscoveryTransport {
    fn new(node_id: &str, epoch: u64, revision: u64, platform: Platform) -> Self {
        Self {
            node_id: NodeId::parse(node_id).expect("node"),
            security: PeerSecurity::external_tunnel(format!("ssh-{node_id}")).expect("security"),
            epoch,
            revision,
            generated_at_ms: now_ms(),
            checked_at_ms: now_ms(),
            state: HealthState::Healthy,
            platform,
            protocol_capabilities: PeerProtocolCapabilities::REQUIRED,
        }
    }

    fn descriptor(&self) -> RemoteDeviceDescriptor {
        RemoteDeviceDescriptor {
            device_key: "phone-1".into(),
            name: format!("Phone at {}", self.node_id),
            platform: self.platform.clone(),
            os_version: Some("1".into()),
        }
    }
}

#[async_trait]
impl PeerTransport for DiscoveryTransport {
    fn expected_node_id(&self) -> &NodeId {
        &self.node_id
    }

    fn security(&self) -> &PeerSecurity {
        &self.security
    }

    async fn request(
        &self,
        request: PeerRequest,
        _: &ExecutionControl,
    ) -> Result<PeerResponse, TransportError> {
        request.validate().map_err(|_| TransportError::Protocol)?;
        let result = match request.operation {
            PeerOperation::Hello => PeerResult::Hello {
                node_id: self.node_id.clone(),
                epoch: self.epoch,
                max_frame_bytes: devicerail_distributed_router::MAX_PEER_FRAME_BYTES as u32,
                capabilities: self.protocol_capabilities,
            },
            PeerOperation::Inventory => PeerResult::Inventory {
                inventory: InventorySnapshot {
                    node_id: self.node_id.clone(),
                    epoch: self.epoch,
                    revision: self.revision,
                    generated_at_ms: self.generated_at_ms,
                    devices: vec![self.descriptor()],
                },
            },
            PeerOperation::Health => PeerResult::Health {
                state: self.state,
                checked_at_ms: self.checked_at_ms,
            },
            PeerOperation::Capabilities { .. } => PeerResult::Capabilities {
                actions: vec![ActionDefinition {
                    name: "tap".into(),
                    description: "Tap".into(),
                    protection: ActionProtection::Standard,
                    input_schema: json!({
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "type": "object",
                        "additionalProperties": false
                    }),
                }],
            },
            _ => return Err(TransportError::Protocol),
        };
        Ok(PeerResponse::success(&request, self.epoch, result))
    }
}

async fn discover(transport: DiscoveryTransport) -> Arc<RemoteNode> {
    let transport: Arc<dyn PeerTransport> = Arc::new(transport);
    let telemetry: Arc<dyn TelemetrySink> = Arc::new(MemoryTelemetry::default());
    RemoteNode::discover(
        transport,
        Some(telemetry),
        RouterConfig::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("discover")
}

#[tokio::test]
async fn discovery_rejects_peers_that_cannot_preserve_v2_ui_contracts() {
    let mut old_semantics = DiscoveryTransport::new("old-peer", 7, 1, Platform::Ios);
    old_semantics.protocol_capabilities.semantic_actions_v1 = false;
    let transport: Arc<dyn PeerTransport> = Arc::new(old_semantics);
    assert!(matches!(
        RemoteNode::discover(
            transport,
            None,
            RouterConfig::default(),
            &ExecutionControl::unbounded(),
        )
        .await,
        Err(RouteError::UnsupportedCapabilities)
    ));
}

#[tokio::test]
async fn two_nodes_with_the_same_remote_key_route_to_distinct_stable_ids() {
    let router = NodeRouter::new(RouterConfig::default());
    router
        .upsert(discover(DiscoveryTransport::new("lab-a", 7, 1, Platform::Android)).await)
        .await
        .expect("node a");
    router
        .upsert(discover(DiscoveryTransport::new("lab-b", 11, 1, Platform::Android)).await)
        .await
        .expect("node b");

    let inventory = router.inventory(now_ms()).await.expect("inventory");
    assert_eq!(
        inventory
            .iter()
            .map(|device| device.id.clone())
            .collect::<Vec<_>>(),
        vec![
            DeviceId::new("remote:lab-a:phone-1"),
            DeviceId::new("remote:lab-b:phone-1")
        ]
    );
    let driver = router
        .route_driver(
            &DeviceId::new("remote:lab-b:phone-1"),
            "daemon-owner",
            RemoteDriverConfig::default(),
            now_ms(),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("route");
    assert_eq!(
        devicerail_core::DeviceDriver::id(&driver),
        &DeviceId::new("remote:lab-b:phone-1")
    );
    router
        .remove_node(&NodeId::parse("lab-a").expect("node a"))
        .await
        .expect("remove node");
    assert_eq!(
        router.inventory(now_ms()).await.expect("remaining").len(),
        1
    );
}

#[tokio::test]
async fn stale_epoch_inventory_and_identity_drift_are_rejected() {
    let router = NodeRouter::new(RouterConfig::default());
    router
        .upsert(discover(DiscoveryTransport::new("lab-a", 7, 1, Platform::Android)).await)
        .await
        .expect("initial");
    assert_eq!(
        router
            .upsert(discover(DiscoveryTransport::new("lab-a", 6, 2, Platform::Android)).await)
            .await,
        Err(RouteError::StaleEpoch)
    );
    assert_eq!(
        router
            .upsert(discover(DiscoveryTransport::new("lab-a", 7, 2, Platform::Ios)).await)
            .await,
        Err(RouteError::IdentityDrift)
    );

    let mut skewed = DiscoveryTransport::new("skewed", 1, 1, Platform::Android);
    skewed.generated_at_ms = 1;
    skewed.checked_at_ms = 9_007_199_254_740_991;
    let transport: Arc<dyn PeerTransport> = Arc::new(skewed);
    RemoteNode::discover(
        transport,
        None,
        RouterConfig::default(),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect("remote wall-clock skew must not determine local freshness");
}

#[tokio::test]
async fn duplex_ndjson_reuses_the_stream_and_validates_response_correlation() {
    let node = NodeId::parse("lab-a").expect("node");
    let security = PeerSecurity::external_tunnel("ssh-lab").expect("security");
    let (client, server) = tokio::io::duplex(64 * 1024);
    let transport = NdjsonPeerTransport::new(client, node.clone(), security);
    let server_task = tokio::spawn({
        let node = node.clone();
        async move {
            let mut stream = BufStream::new(server);
            for _ in 0..2 {
                let mut line = Vec::new();
                stream.read_until(b'\n', &mut line).await.expect("read");
                let request: PeerRequest = serde_json::from_slice(&line).expect("request");
                let result = match request.operation {
                    PeerOperation::Hello => PeerResult::Hello {
                        node_id: node.clone(),
                        epoch: 7,
                        max_frame_bytes: devicerail_distributed_router::MAX_PEER_FRAME_BYTES as u32,
                        capabilities:
                            devicerail_distributed_router::PeerProtocolCapabilities::REQUIRED,
                    },
                    PeerOperation::Health => PeerResult::Health {
                        state: HealthState::Healthy,
                        checked_at_ms: now_ms(),
                    },
                    _ => panic!("unexpected operation"),
                };
                let response = PeerResponse::success(&request, 7, result);
                let mut bytes = serde_json::to_vec(&response).expect("serialize");
                bytes.push(b'\n');
                stream.write_all(&bytes).await.expect("write");
                stream.flush().await.expect("flush");
            }
        }
    });
    transport
        .request(
            PeerRequest::new(node.clone(), None, PeerOperation::Hello),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("hello");
    transport
        .request(
            PeerRequest::new(node, Some(7), PeerOperation::Health),
            &ExecutionControl::unbounded(),
        )
        .await
        .expect("health");
    server_task.await.expect("server");
    assert!(transport.is_open().await);
}

#[tokio::test]
async fn ndjson_classifies_a_local_v1_request_as_unsupported_before_send() {
    let node = NodeId::parse("legacy-local").expect("node");
    let security = PeerSecurity::external_tunnel("ssh-legacy-local").expect("security");
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let transport = NdjsonPeerTransport::new(client, node.clone(), security);
    let mut request = PeerRequest::new(node, None, PeerOperation::Hello);
    request.protocol_version = 1;

    let error = PeerTransport::request(transport.as_ref(), request, &ExecutionControl::unbounded())
        .await
        .expect_err("v1 request must be rejected locally");
    assert_eq!(error, TransportError::UnsupportedVersion);
    let mut byte = [0_u8; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(25), server.read_exact(&mut byte))
            .await
            .is_err(),
        "unsupported request must not reach the wire"
    );
}

#[tokio::test]
async fn ndjson_reports_a_v1_peer_as_an_explicit_version_error() {
    let node = NodeId::parse("legacy-lab").expect("node");
    let security = PeerSecurity::external_tunnel("ssh-legacy").expect("security");
    let (client, server) = tokio::io::duplex(64 * 1024);
    let transport = NdjsonPeerTransport::new(client, node.clone(), security);
    let server_task = tokio::spawn({
        let node = node.clone();
        async move {
            let mut stream = BufStream::new(server);
            let mut line = Vec::new();
            stream.read_until(b'\n', &mut line).await.expect("read");
            let request: PeerRequest = serde_json::from_slice(&line).expect("request");
            let response = serde_json::json!({
                "protocolVersion": 1,
                "requestId": request.request_id,
                "nodeId": node,
                "nodeEpoch": 7,
                "ok": true,
                "result": {
                    "kind": "hello",
                    "nodeId": "legacy-lab",
                    "epoch": 7,
                    "maxFrameBytes": 1048576
                },
                "error": null
            });
            let mut bytes = serde_json::to_vec(&response).expect("serialize v1 response");
            bytes.push(b'\n');
            stream.write_all(&bytes).await.expect("write");
            stream.flush().await.expect("flush");
        }
    });
    let error = PeerTransport::request(
        transport.as_ref(),
        PeerRequest::new(node, None, PeerOperation::Hello),
        &ExecutionControl::unbounded(),
    )
    .await
    .expect_err("v1 response must be rejected");
    assert_eq!(error, TransportError::UnsupportedVersion);
    server_task.await.expect("legacy server");
}

#[tokio::test]
async fn bounded_transport_shards_remove_cross_device_head_of_line_blocking() {
    let node = NodeId::parse("lab-sharded").expect("node");
    let security = PeerSecurity::external_tunnel("ssh-sharded").expect("security");
    let barrier = Arc::new(tokio::sync::Barrier::new(3));
    let first_log = Arc::new(StdMutex::new(Vec::new()));
    let second_log = Arc::new(StdMutex::new(Vec::new()));
    let shards = vec![
        Arc::new(ConcurrentShardTransport {
            node_id: node.clone(),
            security: security.clone(),
            barrier: Arc::clone(&barrier),
            requests: Arc::clone(&first_log),
        }) as Arc<dyn PeerTransport>,
        Arc::new(ConcurrentShardTransport {
            node_id: node.clone(),
            security,
            barrier: Arc::clone(&barrier),
            requests: Arc::clone(&second_log),
        }) as Arc<dyn PeerTransport>,
    ];
    let transport =
        ShardedPeerTransport::new(shards, ["device-a".to_owned(), "device-b".to_owned()])
            .expect("bounded sharded transport");

    let first = tokio::spawn({
        let transport = Arc::clone(&transport);
        let node = node.clone();
        async move {
            transport
                .request(
                    PeerRequest::new(
                        node,
                        Some(7),
                        PeerOperation::Capabilities {
                            device_key: "device-a".to_owned(),
                        },
                    ),
                    &ExecutionControl::unbounded(),
                )
                .await
        }
    });
    let second = tokio::spawn({
        let transport = Arc::clone(&transport);
        async move {
            transport
                .request(
                    PeerRequest::new(
                        node,
                        Some(7),
                        PeerOperation::Capabilities {
                            device_key: "device-b".to_owned(),
                        },
                    ),
                    &ExecutionControl::unbounded(),
                )
                .await
        }
    });

    tokio::time::timeout(std::time::Duration::from_secs(1), barrier.wait())
        .await
        .expect("both device shards reached their transports concurrently");
    first.await.expect("first task").expect("first response");
    second.await.expect("second task").expect("second response");
    assert_eq!(*first_log.lock().expect("first shard log"), ["device-a"]);
    assert_eq!(*second_log.lock().expect("second shard log"), ["device-b"]);
}

#[tokio::test]
async fn cancellation_emits_cancel_then_poisons_connection() {
    let node = NodeId::parse("lab-a").expect("node");
    let security = PeerSecurity::external_tunnel("ssh-lab").expect("security");
    let (client, server) = tokio::io::duplex(64 * 1024);
    let transport = NdjsonPeerTransport::new(client, node.clone(), security);
    let (first_seen_tx, first_seen_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        let mut stream = BufStream::new(server);
        let mut first = Vec::new();
        stream.read_until(b'\n', &mut first).await.expect("first");
        first_seen_tx.send(()).expect("signal");
        let mut cancel = Vec::new();
        stream.read_until(b'\n', &mut cancel).await.expect("cancel");
        let cancel: PeerRequest = serde_json::from_slice(&cancel).expect("cancel frame");
        assert!(matches!(cancel.operation, PeerOperation::Cancel { .. }));
    });
    let (controller, control) = ExecutionController::new();
    let request = PeerRequest::new(node, Some(7), PeerOperation::Health);
    let pending = tokio::spawn({
        let transport = Arc::clone(&transport);
        async move { transport.request(request, &control).await }
    });
    first_seen_rx.await.expect("first request");
    assert!(controller.cancel(CancellationReason::Requested));
    assert_eq!(
        pending.await.expect("join"),
        Err(TransportError::CancelledAfterSend)
    );
    server_task.await.expect("server");
    assert!(!transport.is_open().await);
}

#[tokio::test]
async fn sharded_cancellation_poisoning_is_limited_to_the_target_device_shard() {
    let node = NodeId::parse("lab-shard-cancel").expect("node");
    let security = PeerSecurity::external_tunnel("ssh-shard-cancel").expect("security");
    let (first_client, _first_server) = tokio::io::duplex(64 * 1024);
    let (second_client, second_server) = tokio::io::duplex(64 * 1024);
    let first_transport = NdjsonPeerTransport::new(first_client, node.clone(), security.clone());
    let second_transport = NdjsonPeerTransport::new(second_client, node.clone(), security.clone());
    let transport = ShardedPeerTransport::new(
        vec![
            Arc::clone(&first_transport) as Arc<dyn PeerTransport>,
            Arc::clone(&second_transport) as Arc<dyn PeerTransport>,
        ],
        ["device-a".to_owned(), "device-b".to_owned()],
    )
    .expect("sharded transport");
    let (request_seen_tx, request_seen_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        let mut stream = BufStream::new(second_server);
        let mut request = Vec::new();
        stream
            .read_until(b'\n', &mut request)
            .await
            .expect("device request");
        let request: PeerRequest = serde_json::from_slice(&request).expect("request frame");
        assert!(matches!(
            request.operation,
            PeerOperation::Capabilities { ref device_key } if device_key == "device-b"
        ));
        request_seen_tx.send(()).expect("request signal");
        let mut cancel = Vec::new();
        stream
            .read_until(b'\n', &mut cancel)
            .await
            .expect("cancel frame");
        let cancel: PeerRequest = serde_json::from_slice(&cancel).expect("cancel request");
        assert!(matches!(cancel.operation, PeerOperation::Cancel { .. }));
    });
    let (controller, control) = ExecutionController::new();
    let pending = tokio::spawn({
        let transport = Arc::clone(&transport);
        let node = node.clone();
        async move {
            transport
                .request(
                    PeerRequest::new(
                        node,
                        Some(7),
                        PeerOperation::Capabilities {
                            device_key: "device-b".to_owned(),
                        },
                    ),
                    &control,
                )
                .await
        }
    });
    request_seen_rx.await.expect("device request reached shard");
    assert!(controller.cancel(CancellationReason::Requested));
    assert_eq!(
        pending.await.expect("request task"),
        Err(TransportError::CancelledAfterSend)
    );
    server_task.await.expect("server task");
    assert!(first_transport.is_open().await);
    assert!(!second_transport.is_open().await);
}
