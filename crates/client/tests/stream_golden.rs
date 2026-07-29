use devicerail_protocol::{
    EventSequence, EventsStreamTermination, RpcServerMessage, RpcServerNotification, fixtures,
};

const EVENT_FIXTURE: &str = fixtures::EVENTS_STREAM_EVENT_NOTIFICATION_JSON;
const TERMINAL_FIXTURE: &str = fixtures::EVENTS_STREAM_TERMINAL_NOTIFICATION_JSON;

#[test]
fn canonical_stream_notifications_match_the_shared_wire_model() {
    let event_value: serde_json::Value =
        serde_json::from_str(EVENT_FIXTURE).expect("event fixture JSON");
    let event: RpcServerMessage =
        serde_json::from_value(event_value.clone()).expect("typed event notification");
    match &event {
        RpcServerMessage::Notification(RpcServerNotification::Event(notification)) => {
            assert_eq!(
                notification.params.cursor.sequence,
                EventSequence::new(5).expect("fixture sequence")
            );
            assert_eq!(
                notification.params.event.sequence,
                notification.params.cursor.sequence
            );
            assert_eq!(
                notification.params.event.session_id,
                notification.params.cursor.session_id
            );
        }
        other => panic!("expected event notification, got {other:?}"),
    }
    assert_eq!(
        serde_json::to_value(event).expect("serialize typed event"),
        event_value
    );

    let terminal_value: serde_json::Value =
        serde_json::from_str(TERMINAL_FIXTURE).expect("terminal fixture JSON");
    let terminal: RpcServerMessage =
        serde_json::from_value(terminal_value.clone()).expect("typed terminal notification");
    match &terminal {
        RpcServerMessage::Notification(RpcServerNotification::Terminal(notification)) => {
            assert_eq!(
                notification
                    .params
                    .last_emitted_cursor
                    .as_ref()
                    .expect("fixture terminal cursor")
                    .sequence,
                EventSequence::new(8).expect("fixture sequence")
            );
            assert!(matches!(
                notification.params.termination,
                EventsStreamTermination::SlowConsumer { .. }
            ));
        }
        other => panic!("expected terminal notification, got {other:?}"),
    }
    assert_eq!(
        serde_json::to_value(terminal).expect("serialize typed terminal"),
        terminal_value
    );
}
