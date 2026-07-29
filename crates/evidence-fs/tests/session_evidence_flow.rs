use std::{io::Cursor, sync::Arc};

use devicerail_core::{
    EndSession, EventStoreError, EvidenceStore, GcPolicy, MemoryEventStore, PendingEvent,
    PutEvidence, SessionEventStore, StartSession, cleanup_ended_session, now_ms,
    reconcile_missing_session_evidence,
};
use devicerail_evidence_fs::{FileEvidenceStore, FileEvidenceStoreConfig};
use devicerail_protocol::{DeviceId, Observation, SessionOutcome, TestEventPayload, Viewport};
use serde_json::Map;
use tempfile::TempDir;
use tokio::io::AsyncReadExt as _;
use uuid::Uuid;

#[tokio::test]
async fn durable_evidence_is_pinned_before_event_append_and_released_after_session_delete() {
    let root = TempDir::new().expect("temporary evidence root");
    let evidence = FileEvidenceStore::new(root.path(), FileEvidenceStoreConfig::default())
        .expect("open evidence store");
    let events = Arc::new(MemoryEventStore::default());
    let device_id = DeviceId::new("evidence-device");
    let start = StartSession::new(None, Some(device_id.clone()), now_ms());
    let session_id = start.session_id.clone();
    events.start_session(start).await.expect("start session");

    let stored = evidence
        .put(
            PutEvidence::new(session_id.clone(), "image/png").expect("put request"),
            Box::pin(Cursor::new(b"png fixture bytes".to_vec())),
        )
        .await
        .expect("persist and pin evidence before event append");
    let screenshot = stored.asset_ref();
    events
        .append(PendingEvent {
            session_id: session_id.clone(),
            request_id: None,
            device_id: Some(device_id.clone()),
            at_ms: now_ms(),
            payload: TestEventPayload::ObservationCaptured {
                observation: Box::new(Observation {
                    id: Uuid::new_v4(),
                    device_id: device_id.clone(),
                    captured_at_ms: now_ms(),
                    viewport: Viewport {
                        width: 100,
                        height: 200,
                        scale_factor: 1.0,
                    },
                    screenshot: Some(screenshot.clone()),
                    screenshot_omission: None,
                    ui_snapshot: None,
                    ui_snapshot_omission: None,
                    metadata: Map::new(),
                }),
            },
        })
        .await
        .expect("append event only after evidence is durable");

    assert!(matches!(
        events.delete_ended(&session_id).await,
        Err(EventStoreError::SessionActive(_))
    ));
    let mut verified = evidence
        .open_asset(&screenshot)
        .await
        .expect("open evidence");
    let mut bytes = Vec::new();
    verified
        .read_to_end(&mut bytes)
        .await
        .expect("read evidence");
    assert_eq!(bytes, b"png fixture bytes");
    // GC renames the containing object directory before removing it. Windows
    // rejects that rename while a child file handle is still open, so make the
    // intended lifecycle explicit instead of relying on platform-specific
    // directory-unlink semantics or end-of-scope drop timing.
    drop(verified);

    events
        .end_session(EndSession {
            session_id: session_id.clone(),
            request_id: None,
            device_id: Some(device_id),
            at_ms: now_ms(),
            outcome: SessionOutcome::Completed,
            reason: None,
        })
        .await
        .expect("seal session");
    let exported = events
        .export_session(&session_id)
        .await
        .expect("export replayable session");
    assert_eq!(exported.events.len(), 3);

    // Cleanup is deliberately ordered in reverse: stop/delete the event log,
    // then release durable pins. A crash between these steps leaks safely and
    // requires an idempotent release retry/reconciliation; it never leaves a
    // live event pointing at a missing blob.
    events
        .delete_ended(&session_id)
        .await
        .expect("delete sealed event log");
    let referenced = evidence
        .referenced_sessions()
        .await
        .expect("enumerate durable pins");
    assert_eq!(referenced.len(), 1);
    assert_eq!(referenced[0], session_id);
    let reconciled = reconcile_missing_session_evidence(&*events, &evidence, now_ms())
        .await
        .expect("reconcile crash window");
    assert_eq!(reconciled.len(), 1);
    assert_eq!(reconciled[0].session_id, session_id);

    let retry = cleanup_ended_session(&*events, &evidence, &session_id, now_ms())
        .await
        .expect("cleanup retry is idempotent");
    assert!(!retry.event_log_deleted_now);
    assert_eq!(retry.evidence.released_references, 0);
    let collected = evidence
        .gc(GcPolicy::delete(u64::MAX))
        .await
        .expect("collect unreferenced evidence");
    assert_eq!(collected.deleted_assets, 1);
    assert!(evidence.open_asset(&screenshot).await.is_err());
}
