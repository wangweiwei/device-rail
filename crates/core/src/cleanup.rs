use std::collections::HashSet;

use devicerail_protocol::SessionId;
use thiserror::Error;

use crate::{EventStoreError, EvidenceError, EvidenceStore, ReleaseReport, SessionEventStore};

#[derive(Debug, Error)]
pub enum SessionCleanupError {
    #[error(transparent)]
    Events(#[from] EventStoreError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionCleanupReport {
    pub session_id: SessionId,
    pub event_log_deleted_now: bool,
    pub evidence: ReleaseReport,
}

/// Deletes one ended Session log and then releases its durable Evidence pins.
///
/// A retry treats an already-missing event log as the expected intermediate
/// state and continues with the idempotent Evidence release. This ordering can
/// leak bytes during a crash, but cannot leave a retained event pointing at a
/// deleted object. The Event Store's lifetime-unique Session ID contract also
/// prevents a new Session from claiming this ID in the delete/release gap.
pub async fn cleanup_ended_session<S, E>(
    events: &S,
    evidence: &E,
    session_id: &SessionId,
    released_at_ms: u64,
) -> Result<SessionCleanupReport, SessionCleanupError>
where
    S: SessionEventStore + ?Sized,
    E: EvidenceStore + ?Sized,
{
    let event_log_deleted_now = match events.delete_ended(session_id).await {
        Ok(()) => true,
        Err(EventStoreError::SessionNotFound(_)) => false,
        Err(error) => return Err(error.into()),
    };
    let evidence = evidence.release_session(session_id, released_at_ms).await?;
    Ok(SessionCleanupReport {
        session_id: session_id.clone(),
        event_log_deleted_now,
        evidence,
    })
}

/// Releases durable pins whose Session no longer exists in the event store.
///
/// Callers must serialize this reconciliation with Session creation/deletion.
/// It is intended for daemon startup and cleanup retry paths.
pub async fn reconcile_missing_session_evidence<S, E>(
    events: &S,
    evidence: &E,
    released_at_ms: u64,
) -> Result<Vec<ReleaseReport>, SessionCleanupError>
where
    S: SessionEventStore + ?Sized,
    E: EvidenceStore + ?Sized,
{
    let live = events
        .list_sessions()
        .await?
        .into_iter()
        .map(|session| session.id)
        .collect::<HashSet<_>>();
    let mut reports = Vec::new();
    for session_id in evidence.referenced_sessions().await? {
        if !live.contains(&session_id) {
            reports.push(
                evidence
                    .release_session(&session_id, released_at_ms)
                    .await?,
            );
        }
    }
    Ok(reports)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use devicerail_protocol::{AssetRef, SessionId, SessionOutcome};
    use tokio::sync::Notify;

    use super::cleanup_ended_session;
    use crate::{
        EndSession, EventStoreError, EvidenceError, EvidenceInput, EvidenceMetadata,
        EvidenceOutput, EvidenceResult, EvidenceStore, GcPolicy, GcReport, MemoryEventStore,
        PutEvidence, ReleaseReport, SessionEventStore, Sha256Digest, StartSession, StoredEvidence,
        now_ms,
    };

    struct BlockingReleaseStore {
        release_started: Notify,
        allow_release: Notify,
    }

    impl BlockingReleaseStore {
        fn new() -> Self {
            Self {
                release_started: Notify::new(),
                allow_release: Notify::new(),
            }
        }

        fn unused<T>() -> EvidenceResult<T> {
            Err(EvidenceError::Internal(
                "unused cleanup test operation".to_owned(),
            ))
        }
    }

    #[async_trait]
    impl EvidenceStore for BlockingReleaseStore {
        async fn put(
            &self,
            _request: PutEvidence,
            _input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            Self::unused()
        }

        async fn attach(
            &self,
            _session_id: &SessionId,
            _asset: &AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            Self::unused()
        }

        async fn verify_session_reference(
            &self,
            _session_id: &SessionId,
            _asset: &AssetRef,
        ) -> EvidenceResult<EvidenceMetadata> {
            Self::unused()
        }

        async fn open(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceOutput> {
            Self::unused()
        }

        async fn metadata(&self, _digest: &Sha256Digest) -> EvidenceResult<EvidenceMetadata> {
            Self::unused()
        }

        async fn referenced_sessions(&self) -> EvidenceResult<Vec<SessionId>> {
            Self::unused()
        }

        async fn release_session(
            &self,
            session_id: &SessionId,
            _released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            self.release_started.notify_one();
            self.allow_release.notified().await;
            Ok(ReleaseReport {
                session_id: session_id.clone(),
                released_references: 0,
                newly_unreferenced_assets: 0,
                newly_unreferenced_bytes: 0,
            })
        }

        async fn gc(&self, _policy: GcPolicy) -> EvidenceResult<GcReport> {
            Self::unused()
        }
    }

    #[tokio::test]
    async fn cleanup_gap_cannot_reuse_the_deleted_session_id() {
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(BlockingReleaseStore::new());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Completed,
                reason: None,
            })
            .await
            .expect("end Session");

        let cleanup = tokio::spawn({
            let events = Arc::clone(&events);
            let evidence = Arc::clone(&evidence);
            let session_id = session_id.clone();
            async move { cleanup_ended_session(&*events, &*evidence, &session_id, now_ms()).await }
        });
        evidence.release_started.notified().await;

        assert!(matches!(
            events.export_session(&session_id).await,
            Err(EventStoreError::SessionNotFound(id)) if id == session_id
        ));
        assert_eq!(
            events
                .start_session(StartSession {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                })
                .await
                .expect_err("cleanup gap must retain the used-ID tombstone"),
            EventStoreError::SessionAlreadyExists(session_id.clone())
        );

        evidence.allow_release.notify_one();
        let report = cleanup
            .await
            .expect("cleanup task")
            .expect("complete cleanup");
        assert_eq!(report.session_id, session_id);
        assert!(report.event_log_deleted_now);
    }
}
