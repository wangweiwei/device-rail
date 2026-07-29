use std::{
    env,
    ffi::{OsStr, OsString},
    future::Future,
    time::Duration,
};

use devicerail_client::{
    CallOptions, ClientState, DeviceRailClient, EventStreamError, EventStreamOptions, SpawnConfig,
    default_hello,
    methods::{self, NoParams},
    protocol::{
        ActionOutcome, DeviceExecuteParams, DeviceId, DeviceSelectParams, EventSequence,
        EventStreamCursor, EventStreamEpoch, EventsListParams, EventsStreamTermination,
        EventsSubscribeParams, RequestTimeoutMs, SessionOutcome, SessionState, TestEventPayload,
        Verdict, VerdictRecordParams, VerdictStatus, feature,
    },
};
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use uuid::Uuid;

const OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_OPERATION_TIMEOUT_MS: u64 = 3_000;

fn stock_daemon_config(evidence: &TempDir) -> SpawnConfig {
    let mut config = SpawnConfig::new(env!("CARGO_BIN_EXE_devicerail-daemon"), default_hello());

    // Start from the test process environment so platform requirements such as
    // PATH still work, while making the daemon hermetic with respect to every
    // externally supplied DeviceRail setting.
    config.env_clear = true;
    for (name, value) in env::vars_os() {
        if !is_devicerail_env(&name) {
            config.env.insert(name, value);
        }
    }
    config.env.insert(
        OsString::from("DEVICERAIL_EVIDENCE_DIR"),
        evidence.path().as_os_str().to_owned(),
    );
    for platform in [
        "DEVICERAIL_ANDROID",
        "DEVICERAIL_HARMONY",
        "DEVICERAIL_IOS",
        "DEVICERAIL_DESKTOP",
    ] {
        config
            .env
            .insert(OsString::from(platform), OsString::from("off"));
    }
    config.client.close_grace = Duration::from_secs(2);
    config
}

fn is_devicerail_env(name: &OsStr) -> bool {
    name.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with("DEVICERAIL_")
}

fn remote_operation_options() -> CallOptions {
    CallOptions {
        timeout_ms: RequestTimeoutMs::new(REMOTE_OPERATION_TIMEOUT_MS),
    }
}

async fn expect_client_result<T, E>(
    client: &DeviceRailClient,
    operation: &str,
    future: impl Future<Output = Result<T, E>>,
) -> T
where
    E: std::fmt::Display,
{
    match timeout(OPERATION_TIMEOUT, future).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!(
            "{operation} failed: {error}; daemon stderr: {}",
            client.stderr_tail()
        ),
        Err(_) => panic!(
            "{operation} exceeded {OPERATION_TIMEOUT:?}; daemon stderr: {}",
            client.stderr_tail()
        ),
    }
}

#[tokio::test]
async fn rust_client_drives_the_stock_daemon_and_confirms_its_session_stream() {
    let evidence = TempDir::new().expect("create isolated Evidence directory");
    let client = timeout(
        OPERATION_TIMEOUT,
        DeviceRailClient::spawn(stock_daemon_config(&evidence)),
    )
    .await
    .expect("stock daemon spawn and automatic hello timed out")
    .expect("spawn stock daemon and negotiate hello");

    assert_eq!(client.state(), ClientState::Ready);
    assert!(
        client
            .enabled_features()
            .contains(feature::DEVICE_ROUTING_V1),
        "stock hello must negotiate device routing"
    );
    assert!(
        client
            .enabled_features()
            .contains(feature::VERDICT_RECORD_V1),
        "the default Rust hello must negotiate verdict.record.v1 with the stock daemon"
    );
    // Hardened runners may prohibit loopback listeners; the daemon then
    // advertises the control plane without events.stream.v1.
    let event_stream_available = client
        .enabled_features()
        .contains(feature::EVENTS_STREAM_V1);
    if !event_stream_available {
        assert_eq!(
            env::var("DEVICERAIL_ALLOW_NO_LOOPBACK").as_deref(),
            Ok("1"),
            "stock daemon omitted events.stream.v1 without an explicit no-loopback test waiver; stderr: {}",
            client.stderr_tail()
        );
    }

    let connection = expect_client_result(
        &client,
        "system.describe",
        client.call::<methods::SystemDescribe>(NoParams, CallOptions::default()),
    )
    .await;
    assert_eq!(connection.connection.server.name, "devicerail-daemon");
    assert_eq!(connection.connection.transport.kind, "stdio");
    assert_eq!(connection.connection.transport.framing, "ndjson");

    let listed = expect_client_result(
        &client,
        "devices.list",
        client.call::<methods::DevicesList>(NoParams, CallOptions::default()),
    )
    .await;
    assert_eq!(
        listed.devices.len(),
        1,
        "disabled platform discovery must leave only the stock mock device"
    );
    assert_eq!(listed.devices[0].id, DeviceId::new("mock-1"));
    assert_eq!(listed.selected_device_id, Some(DeviceId::new("mock-1")));

    let selected = expect_client_result(
        &client,
        "device.select",
        client.call::<methods::DeviceSelect>(
            DeviceSelectParams {
                device_id: DeviceId::new("mock-1"),
            },
            CallOptions::default(),
        ),
    )
    .await;
    assert_eq!(selected.device.id, DeviceId::new("mock-1"));

    let connected = expect_client_result(
        &client,
        "device.connect",
        client.call::<methods::DeviceConnect>(NoParams, remote_operation_options()),
    )
    .await;
    assert_eq!(connected.id, DeviceId::new("mock-1"));
    assert!(connected.connected);

    let session = expect_client_result(
        &client,
        "session.start",
        client.call::<methods::SessionStart>(NoParams, CallOptions::default()),
    )
    .await;
    assert_eq!(session.state, SessionState::Active);

    if event_stream_available {
        let stale_cursor = EventStreamCursor {
            stream_epoch: EventStreamEpoch::new(),
            session_id: session.id.clone(),
            sequence: EventSequence::FIRST,
        };
        timeout(Duration::from_secs(60), async {
            // The stock server admits 256 pending capabilities. Repeating one
            // attempt beyond that limit proves a stale cursor consumes its
            // one-shot bearer during WebSocket admission instead of leaking
            // every capability until its TTL expires.
            for attempt in 0..257 {
                let error = client
                    .open_event_stream(
                        EventsSubscribeParams {
                            session_id: session.id.clone(),
                            after_cursor: Some(stale_cursor.clone()),
                        },
                        EventStreamOptions::default(),
                    )
                    .await
                    .expect_err("stale event cursor must be rejected");
                assert!(
                    matches!(error, EventStreamError::Cursor(_)),
                    "stale cursor attempt {attempt} returned {error}"
                );
            }
        })
        .await
        .expect("stale-cursor capability consumption regression timed out");
    }

    let mut events = if event_stream_available {
        Some(
            expect_client_result(
                &client,
                "open event stream",
                client.open_event_stream(
                    EventsSubscribeParams {
                        session_id: session.id.clone(),
                        after_cursor: None,
                    },
                    EventStreamOptions::default(),
                ),
            )
            .await,
        )
    } else {
        None
    };

    let capabilities = expect_client_result(
        &client,
        "device.capabilities",
        client.call::<methods::DeviceCapabilities>(NoParams, remote_operation_options()),
    )
    .await;
    assert!(
        capabilities
            .iter()
            .any(|definition| definition.name == "tap"),
        "stock mock capabilities must include tap"
    );

    let action_id = Uuid::new_v4();
    let action = expect_client_result(
        &client,
        "device.execute",
        client.call::<methods::DeviceExecute>(
            DeviceExecuteParams {
                id: action_id,
                name: "tap".to_owned(),
                arguments: json!({ "x": 10, "y": 20 }),
                action_timeout_ms: None,
            },
            remote_operation_options(),
        ),
    )
    .await;
    assert_eq!(action.call_id, action_id);

    let listed_events = expect_client_result(
        &client,
        "events.list",
        client.call::<methods::EventsList>(
            Some(EventsListParams {
                session_id: Some(session.id.clone()),
                after_sequence: None,
                limit: None,
            }),
            CallOptions::default(),
        ),
    )
    .await;
    let mut action_started_index = None;
    let mut action_completed_index = None;
    for (index, event) in listed_events.iter().enumerate() {
        assert_eq!(event.session_id, session.id);
        match &event.payload {
            TestEventPayload::ActionStarted { call } if call.id == action_id => {
                assert_eq!(call.name, "tap");
                action_started_index = Some(index);
            }
            TestEventPayload::ActionCompleted { call_id, outcome } if *call_id == action_id => {
                let ActionOutcome::Succeeded { result } = outcome else {
                    panic!("tap ActionCompleted outcome was not succeeded: {outcome:?}");
                };
                assert_eq!(result.call_id, action_id);
                action_completed_index = Some(index);
            }
            _ => {}
        }
    }
    let action_started_index =
        action_started_index.expect("events.list omitted the tap actionStarted event");
    let action_completed_index =
        action_completed_index.expect("events.list omitted the tap actionCompleted event");
    assert!(
        action_started_index < action_completed_index,
        "actionStarted must precede actionCompleted"
    );

    let expected_verdict = Verdict {
        status: VerdictStatus::Pass,
        summary: "Rust client completed the stock tap action".to_owned(),
        evidence: Vec::new(),
    };
    let recorded_verdict = expect_client_result(
        &client,
        "verdict.record",
        client.call::<methods::VerdictRecord>(
            VerdictRecordParams {
                verdict: expected_verdict.clone(),
            },
            CallOptions::default(),
        ),
    )
    .await;
    assert_eq!(recorded_verdict.event.session_id, session.id);
    match &recorded_verdict.event.payload {
        TestEventPayload::VerdictRecorded { verdict } => {
            assert_eq!(verdict, &expected_verdict);
        }
        payload => panic!("verdict.record returned an unexpected event payload: {payload:?}"),
    }

    let observation = expect_client_result(
        &client,
        "device.observe",
        client.call::<methods::DeviceObserve>(NoParams, remote_operation_options()),
    )
    .await;
    assert_eq!(observation.device_id, DeviceId::new("mock-1"));

    let ended = expect_client_result(
        &client,
        "session.end",
        client.call::<methods::SessionEnd>(None, CallOptions::default()),
    )
    .await;
    assert_eq!(ended.id, session.id);
    assert_eq!(ended.state, SessionState::Ended);
    assert!(ended.ended_at_ms.is_some());

    if let Some(events) = events.as_mut() {
        let mut saw_started = false;
        let mut saw_observation = false;
        let mut saw_ended = false;
        for _ in 0..16 {
            let Some(item) =
                expect_client_result(&client, "read event stream", events.next()).await
            else {
                break;
            };
            assert_eq!(item.event.session_id, session.id);
            match &item.event.payload {
                TestEventPayload::SessionStarted => saw_started = true,
                TestEventPayload::ObservationCaptured { observation } => {
                    assert_eq!(observation.device_id, DeviceId::new("mock-1"));
                    saw_observation = true;
                }
                TestEventPayload::SessionEnded { outcome, reason } => {
                    assert_eq!(*outcome, SessionOutcome::Completed);
                    assert!(reason.is_none());
                    saw_ended = true;
                }
                _ => {}
            }
            events
                .confirm(&item.cursor)
                .expect("confirm the delivered event cursor");
        }
        assert!(saw_started, "stream omitted sessionStarted");
        assert!(saw_observation, "stream omitted observationCaptured");
        assert!(saw_ended, "stream omitted sessionEnded");
        assert_eq!(
            events
                .terminal()
                .expect("clean stream terminal after session.end")
                .termination,
            EventsStreamTermination::SessionEnded
        );
        assert_eq!(events.confirmed_cursor(), events.delivered_cursor());

        timeout(OPERATION_TIMEOUT, events.cancel())
            .await
            .expect("event stream cleanup timed out");
    }

    expect_client_result(&client, "client.close", client.close()).await;
    assert_eq!(client.state(), ClientState::Closed);
}
