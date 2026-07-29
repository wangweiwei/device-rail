use devicerail_distributed_router::{
    DISTRIBUTED_PROTOCOL_VERSION, PEER_PROTOCOL_SCHEMA, PeerRequest, PeerResponse, PeerResult,
};
use uuid::Uuid;

#[test]
fn peer_schema_and_golden_fixtures_validate_and_round_trip() {
    let schema: serde_json::Value =
        serde_json::from_str(PEER_PROTOCOL_SCHEMA).expect("parse peer schema");
    let validator = jsonschema::validator_for(&schema).expect("compile peer schema");
    for fixture in [
        include_str!("../protocol/fixtures/peer-v2.hello.request.json"),
        include_str!("../protocol/fixtures/peer-v2.hello.response.json"),
        include_str!("../protocol/fixtures/peer-v2.execute.request.json"),
    ] {
        let value: serde_json::Value = serde_json::from_str(fixture).expect("parse fixture");
        assert!(
            validator.is_valid(&value),
            "fixture failed schema: {:?}",
            validator.iter_errors(&value).collect::<Vec<_>>()
        );
    }

    for fixture in [
        include_str!("../protocol/fixtures/peer-v2.hello.request.json"),
        include_str!("../protocol/fixtures/peer-v2.execute.request.json"),
    ] {
        let value: serde_json::Value = serde_json::from_str(fixture).expect("parse request");
        let request: PeerRequest = serde_json::from_value(value.clone()).expect("typed request");
        request.validate().expect("valid request");
        assert_eq!(serde_json::to_value(&request).expect("serialize"), value);
    }
    let response_value: serde_json::Value = serde_json::from_str(include_str!(
        "../protocol/fixtures/peer-v2.hello.response.json"
    ))
    .expect("parse response");
    let response: PeerResponse =
        serde_json::from_value(response_value.clone()).expect("typed response");
    assert_eq!(
        serde_json::to_value(&response).expect("serialize"),
        response_value
    );
}

#[test]
fn protected_execute_fixture_is_redacted_from_debug() {
    let request: PeerRequest = serde_json::from_str(include_str!(
        "../protocol/fixtures/peer-v2.execute.request.json"
    ))
    .expect("request");
    let debug = format!("{request:?}");
    assert!(debug.contains("execute"));
    assert!(!debug.contains("fixture-secret"));
    assert!(!debug.contains("arguments"));
}

#[test]
fn peer_v2_schema_carries_ui_snapshot_refs_and_semantic_execution_metadata() {
    let schema: serde_json::Value = serde_json::from_str(PEER_PROTOCOL_SCHEMA).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("validator");
    let mut request_value: serde_json::Value = serde_json::from_str(include_str!(
        "../protocol/fixtures/peer-v2.execute.request.json"
    ))
    .expect("execute request");
    request_value["operation"]["call"]["name"] = serde_json::json!("findElement");
    request_value["operation"]["call"]["arguments"] = serde_json::json!({
        "selector": {"role": "button"}
    });
    request_value["operation"]["screenshotOmission"] = serde_json::Value::Null;
    request_value["operation"]["uiSnapshotsEnabled"] = serde_json::json!(true);
    request_value["operation"]["semanticActionsEnabled"] = serde_json::json!(true);
    let request: PeerRequest = serde_json::from_value(request_value).expect("semantic request");
    let response = serde_json::json!({
        "protocolVersion": 2,
        "requestId": request.request_id,
        "nodeId": "lab-a",
        "nodeEpoch": 7,
        "ok": true,
        "result": {
            "kind": "action",
            "result": {
                "callId": request.call_id,
                "startedAtMs": 1,
                "finishedAtMs": 2,
                "output": {"accepted": true},
                "before": null,
                "after": {
                    "id": "00000000-0000-4000-8000-000000000099",
                    "deviceId": "phone-1",
                    "capturedAtMs": 2,
                    "viewport": {"width": 1, "height": 1, "scaleFactor": 1.0},
                    "screenshot": null,
                    "uiSnapshot": {
                        "formatVersion": 1,
                        "context": {
                            "contextKind": "native",
                            "contextId": "NATIVE_APP",
                            "documentEpoch": "epoch-1"
                        },
                        "nodeCount": 1,
                        "byteLength": 128,
                        "evidence": {
                            "id": "ui-tree-1",
                            "mediaType": "application/vnd.devicerail.ui-tree+json;version=1",
                            "uri": "peer:evidence/ui-tree-1",
                            "sha256": null
                        }
                    },
                    "metadata": {}
                },
                "evidence": [],
                "execution": {
                    "mode": "nativeSemantic",
                    "context": {
                        "contextKind": "native",
                        "contextId": "NATIVE_APP",
                        "documentEpoch": "epoch-1"
                    }
                }
            }
        },
        "error": null
    });
    assert!(
        validator.is_valid(&response),
        "UI response failed peer-v2 schema: {:?}",
        validator.iter_errors(&response).collect::<Vec<_>>()
    );
    let typed: PeerResponse = serde_json::from_value(response).expect("typed UI response");
    typed
        .validate_for(&request)
        .expect("correlated UI response");
}

#[test]
fn observation_nulls_match_rust_option_semantics_without_weakening_exclusivity() {
    let schema: serde_json::Value = serde_json::from_str(PEER_PROTOCOL_SCHEMA).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("validator");
    let response = serde_json::json!({
        "protocolVersion": DISTRIBUTED_PROTOCOL_VERSION,
        "requestId": "00000000-0000-4000-8000-000000000001",
        "nodeId": "lab-a",
        "nodeEpoch": 7,
        "ok": true,
        "result": {
            "kind": "observation",
            "observation": {
                "id": "00000000-0000-4000-8000-000000000099",
                "deviceId": "phone-1",
                "capturedAtMs": 2,
                "viewport": {"width": 1, "height": 1, "scaleFactor": 1.0},
                "screenshot": null,
                "screenshotOmission": null,
                "uiSnapshot": null,
                "uiSnapshotOmission": null,
                "metadata": {}
            }
        },
        "error": null
    });
    assert!(
        validator.is_valid(&response),
        "explicit Option nulls failed peer schema: {:?}",
        validator.iter_errors(&response).collect::<Vec<_>>()
    );
    let typed: PeerResponse = serde_json::from_value(response).expect("typed response");
    let Some(PeerResult::Observation { observation }) = typed.result else {
        panic!("observation result expected");
    };
    assert!(observation.screenshot_omission.is_none());
    assert!(observation.ui_snapshot.is_none());
    assert!(observation.ui_snapshot_omission.is_none());

    let mut conflict = serde_json::to_value(PeerResponse {
        protocol_version: DISTRIBUTED_PROTOCOL_VERSION,
        request_id: Uuid::from_u128(1),
        node_id: devicerail_distributed_router::NodeId::parse("lab-a").expect("node"),
        node_epoch: 7,
        ok: true,
        result: Some(PeerResult::Observation { observation }),
        error: None,
    })
    .expect("serialize response");
    let observation = &mut conflict["result"]["observation"];
    observation["uiSnapshot"] = serde_json::json!({
        "formatVersion": 1,
        "context": {
            "contextKind": "native",
            "contextId": "NATIVE_APP",
            "documentEpoch": "epoch-1"
        },
        "nodeCount": 1,
        "byteLength": 128,
        "evidence": {
            "id": "ui-tree-1",
            "mediaType": "application/vnd.devicerail.ui-tree+json;version=1",
            "uri": "peer:evidence/ui-tree-1",
            "sha256": null
        }
    });
    observation["uiSnapshotOmission"] = serde_json::json!("policy");
    assert!(!validator.is_valid(&conflict));
}

#[test]
fn schema_and_typed_decoder_reject_extension_fields_fail_closed() {
    let schema: serde_json::Value = serde_json::from_str(PEER_PROTOCOL_SCHEMA).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("validator");
    let mut value: serde_json::Value = serde_json::from_str(include_str!(
        "../protocol/fixtures/peer-v2.execute.request.json"
    ))
    .expect("fixture");
    value["operation"]["secretExtension"] = serde_json::json!(true);
    assert!(!validator.is_valid(&value));
    assert!(serde_json::from_value::<PeerRequest>(value).is_err());

    let invalid_node = serde_json::json!({
        "protocolVersion": DISTRIBUTED_PROTOCOL_VERSION,
        "requestId": "00000000-0000-4000-8000-000000000001",
        "traceId": "10000000-0000-4000-8000-000000000001",
        "nodeId": "invalid:node",
        "nodeEpoch": null,
        "timeoutMs": null,
        "callId": null,
        "lease": null,
        "operation": {"method": "hello"}
    });
    assert!(!validator.is_valid(&invalid_node));
    assert!(serde_json::from_value::<PeerRequest>(invalid_node).is_err());
}

#[test]
fn peer_v1_is_rejected_instead_of_silently_dropping_feature_gates() {
    let schema: serde_json::Value = serde_json::from_str(PEER_PROTOCOL_SCHEMA).expect("schema");
    let validator = jsonschema::validator_for(&schema).expect("validator");
    let old_hello = serde_json::json!({
        "protocolVersion": 1,
        "requestId": "00000000-0000-4000-8000-000000000001",
        "traceId": "10000000-0000-4000-8000-000000000001",
        "nodeId": "lab-a",
        "nodeEpoch": null,
        "timeoutMs": null,
        "callId": null,
        "lease": null,
        "operation": {"method": "hello"}
    });
    assert!(!validator.is_valid(&old_hello));
    let request: PeerRequest = serde_json::from_value(old_hello).expect("typed old request");
    assert_eq!(
        request.validate(),
        Err(devicerail_distributed_router::ModelError::UnsupportedVersion)
    );

    let current_request: PeerRequest = serde_json::from_str(include_str!(
        "../protocol/fixtures/peer-v2.hello.request.json"
    ))
    .expect("current request");
    let mut old_response: PeerResponse = serde_json::from_str(include_str!(
        "../protocol/fixtures/peer-v2.hello.response.json"
    ))
    .expect("typed response");
    old_response.protocol_version = 1;
    assert_eq!(
        old_response.validate_for(&current_request),
        Err(devicerail_distributed_router::ModelError::UnsupportedVersion)
    );
}
