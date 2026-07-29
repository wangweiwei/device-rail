use std::sync::Arc;

use devicerail_protocol::{
    AssetRef, DeviceId, EventSequence, MediaFrame, MediaStreamId, MediaStreamInfo, RpcId,
    SessionId, TestEventPayload,
};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    EventStoreError, EvidenceError, EvidenceInput, EvidenceStore, ExecutionControl, PendingEvent,
    PutEvidence, SessionEventStore,
};

struct WriterState {
    started: bool,
    next_frame_index: u64,
    pending: Option<PendingFrame>,
    terminal_pending: Option<PendingTerminal>,
    terminal_frame_count: Option<u64>,
}

#[derive(Clone)]
struct PendingFrame {
    request_id: Option<RpcId>,
    at_ms: u64,
    frame: MediaFrame,
}

#[derive(Clone)]
struct PendingTerminal {
    request_id: Option<RpcId>,
    at_ms: u64,
}

/// Serial, Session-scoped publisher for screenshot or video frames.
///
/// Every accepted frame is first persisted by the Evidence Store and then
/// enters the append-only event sequence only as its canonical `AssetRef`.
#[must_use = "a media stream must be finished or aborted so its Session can close"]
pub struct MediaStreamWriter<S: ?Sized> {
    events: Arc<S>,
    evidence: Arc<dyn EvidenceStore>,
    session_id: SessionId,
    default_request_id: Option<RpcId>,
    device_id: Option<DeviceId>,
    stream: MediaStreamInfo,
    started_at_ms: u64,
    state: Mutex<WriterState>,
}

#[derive(Debug, Error)]
pub enum MediaStreamError {
    #[error(transparent)]
    Event(#[from] EventStoreError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
    #[error("media stream operation was cancelled")]
    Cancelled,
    #[error("media stream operation timed out")]
    TimedOut,
    #[error("media stream is already ended")]
    Ended,
    #[error("media frame metadata is invalid")]
    InvalidFrame,
}

impl<S> MediaStreamWriter<S>
where
    S: SessionEventStore + ?Sized,
{
    /// Prepares a recoverable writer before publishing its start event.
    ///
    /// Callers that must survive an ambiguous start append can retain this
    /// value and retry [`Self::ensure_started`]. Every later frame or close
    /// operation also ensures the exact original start event first.
    pub fn prepare(
        events: Arc<S>,
        evidence: Arc<dyn EvidenceStore>,
        session_id: SessionId,
        request_id: Option<RpcId>,
        device_id: Option<DeviceId>,
        stream: MediaStreamInfo,
        at_ms: u64,
    ) -> Self {
        Self {
            events,
            evidence,
            session_id,
            default_request_id: request_id,
            device_id,
            stream,
            started_at_ms: at_ms,
            state: Mutex::new(WriterState {
                started: false,
                next_frame_index: 1,
                pending: None,
                terminal_pending: None,
                terminal_frame_count: None,
            }),
        }
    }

    /// Ensures that the exact prepared start event is committed once.
    pub async fn ensure_started(&self) -> Result<(), MediaStreamError> {
        let mut state = self.state.lock().await;
        self.ensure_started_locked(&mut state).await
    }

    pub fn id(&self) -> &MediaStreamId {
        &self.stream.id
    }

    pub async fn push_frame(
        &self,
        control: &ExecutionControl,
        at_ms: u64,
        key_frame: bool,
        duration_ms: Option<u64>,
        declared_size_bytes: u64,
        input: EvidenceInput,
    ) -> Result<MediaFrame, MediaStreamError> {
        self.push_frame_with_request_id(
            control,
            self.default_request_id.clone(),
            at_ms,
            key_frame,
            duration_ms,
            declared_size_bytes,
            input,
        )
        .await
    }

    /// Publishes a frame correlated to the request that produced it.
    #[allow(clippy::too_many_arguments)]
    pub async fn push_frame_with_request_id(
        &self,
        control: &ExecutionControl,
        request_id: Option<RpcId>,
        at_ms: u64,
        key_frame: bool,
        duration_ms: Option<u64>,
        declared_size_bytes: u64,
        input: EvidenceInput,
    ) -> Result<MediaFrame, MediaStreamError> {
        ensure_active(control)?;
        if declared_size_bytes == 0
            || duration_ms.is_some_and(|value| value > devicerail_protocol::MAX_SAFE_INTEGER)
        {
            return Err(MediaStreamError::InvalidFrame);
        }
        let mut state = tokio::select! {
            state = self.state.lock() => state,
            _ = control.cancelled() => return Err(MediaStreamError::Cancelled),
            _ = deadline(control) => return Err(MediaStreamError::TimedOut),
        };
        self.ensure_started_locked(&mut state).await?;
        if state.terminal_frame_count.is_some() {
            return Err(MediaStreamError::Ended);
        }
        self.flush_pending(&mut state).await?;
        ensure_active(control)?;
        let frame_index =
            EventSequence::new(state.next_frame_index).ok_or(MediaStreamError::InvalidFrame)?;
        let request = PutEvidence::new(self.session_id.clone(), self.stream.media_type.clone())?
            .with_declared_size_bytes(declared_size_bytes);
        // Evidence publication starts irreversible frame finalization. Once it
        // begins, finish the matching event append even if the caller is
        // cancelled; the append is the frame commit, and abandoning it could
        // leave a successfully pinned object unreachable.
        let stored = self.evidence.put(request, input).await?;
        let frame = MediaFrame {
            stream_id: self.stream.id.clone(),
            frame_index,
            key_frame,
            duration_ms,
            evidence: stored.asset_ref(),
        };
        state.pending = Some(PendingFrame {
            request_id,
            at_ms,
            frame: frame.clone(),
        });
        self.flush_pending(&mut state).await?;
        Ok(frame)
    }

    /// Retains an existing canonical Evidence object as the next frame.
    ///
    /// The caller cannot choose the owning Session: `attach` always binds the
    /// reference to this writer's Session. As with [`Self::push_frame`], once
    /// the Evidence operation begins, the matching event append is finalized
    /// independently of later request cancellation. An ambiguous append keeps
    /// the exact frame pending for `finish` or `abort` recovery.
    pub async fn push_asset_frame(
        &self,
        control: &ExecutionControl,
        at_ms: u64,
        key_frame: bool,
        duration_ms: Option<u64>,
        asset: &AssetRef,
    ) -> Result<MediaFrame, MediaStreamError> {
        self.push_asset_frame_with_request_id(
            control,
            self.default_request_id.clone(),
            at_ms,
            key_frame,
            duration_ms,
            asset,
        )
        .await
    }

    /// Retains a canonical Evidence object for the producing request.
    pub async fn push_asset_frame_with_request_id(
        &self,
        control: &ExecutionControl,
        request_id: Option<RpcId>,
        at_ms: u64,
        key_frame: bool,
        duration_ms: Option<u64>,
        asset: &AssetRef,
    ) -> Result<MediaFrame, MediaStreamError> {
        ensure_active(control)?;
        if asset.media_type != self.stream.media_type
            || duration_ms.is_some_and(|value| value > devicerail_protocol::MAX_SAFE_INTEGER)
        {
            return Err(MediaStreamError::InvalidFrame);
        }
        let mut state = tokio::select! {
            state = self.state.lock() => state,
            _ = control.cancelled() => return Err(MediaStreamError::Cancelled),
            _ = deadline(control) => return Err(MediaStreamError::TimedOut),
        };
        self.ensure_started_locked(&mut state).await?;
        if state.terminal_frame_count.is_some() {
            return Err(MediaStreamError::Ended);
        }
        self.flush_pending(&mut state).await?;
        ensure_active(control)?;
        let frame_index =
            EventSequence::new(state.next_frame_index).ok_or(MediaStreamError::InvalidFrame)?;
        let stored = self.evidence.attach(&self.session_id, asset).await?;
        let frame = MediaFrame {
            stream_id: self.stream.id.clone(),
            frame_index,
            key_frame,
            duration_ms,
            evidence: stored.asset_ref(),
        };
        state.pending = Some(PendingFrame {
            request_id,
            at_ms,
            frame: frame.clone(),
        });
        self.flush_pending(&mut state).await?;
        Ok(frame)
    }

    /// Close the stream after committing any Evidence-backed pending frame.
    ///
    /// Closure is idempotent and deliberately shielded from request
    /// cancellation. Retrying after an Event Store failure resumes the same
    /// pending frame instead of storing the Evidence again.
    pub async fn finish(&self, at_ms: u64) -> Result<u64, MediaStreamError> {
        self.finish_with_request_id(self.default_request_id.clone(), at_ms)
            .await
    }

    /// Closes the stream under the request that explicitly ended it.
    pub async fn finish_with_request_id(
        &self,
        request_id: Option<RpcId>,
        at_ms: u64,
    ) -> Result<u64, MediaStreamError> {
        self.close(request_id, at_ms).await
    }

    /// Recover and close a stream whose producer was cancelled or failed.
    ///
    /// Protocol 1.4 has one terminal stream event, so abort closes the accepted
    /// prefix rather than inventing a second wire-level outcome. It retries any
    /// frame whose Evidence was stored before its event append failed.
    pub async fn abort(&self, at_ms: u64) -> Result<u64, MediaStreamError> {
        self.abort_with_request_id(self.default_request_id.clone(), at_ms)
            .await
    }

    /// Recovers and closes the stream under the request that triggered abort.
    pub async fn abort_with_request_id(
        &self,
        request_id: Option<RpcId>,
        at_ms: u64,
    ) -> Result<u64, MediaStreamError> {
        self.close(request_id, at_ms).await
    }

    async fn close(&self, request_id: Option<RpcId>, at_ms: u64) -> Result<u64, MediaStreamError> {
        let mut state = self.state.lock().await;
        if let Some(frame_count) = state.terminal_frame_count {
            return Ok(frame_count);
        }
        self.ensure_started_locked(&mut state).await?;
        self.flush_pending(&mut state).await?;
        let frame_count = state.next_frame_index - 1;
        let terminal = state
            .terminal_pending
            .get_or_insert(PendingTerminal { request_id, at_ms })
            .clone();
        self.events
            .append(PendingEvent {
                session_id: self.session_id.clone(),
                request_id: terminal.request_id,
                device_id: self.device_id.clone(),
                at_ms: terminal.at_ms,
                payload: TestEventPayload::MediaStreamEnded {
                    stream_id: self.stream.id.clone(),
                    frame_count,
                },
            })
            .await?;
        state.terminal_pending = None;
        state.terminal_frame_count = Some(frame_count);
        Ok(frame_count)
    }

    async fn ensure_started_locked(&self, state: &mut WriterState) -> Result<(), MediaStreamError> {
        if state.started {
            return Ok(());
        }
        self.events
            .append(PendingEvent {
                session_id: self.session_id.clone(),
                request_id: self.default_request_id.clone(),
                device_id: self.device_id.clone(),
                at_ms: self.started_at_ms,
                payload: TestEventPayload::MediaStreamStarted {
                    stream: self.stream.clone(),
                },
            })
            .await?;
        state.started = true;
        Ok(())
    }

    async fn flush_pending(&self, state: &mut WriterState) -> Result<(), MediaStreamError> {
        let Some(pending) = state.pending.clone() else {
            return Ok(());
        };
        self.events
            .append(PendingEvent {
                session_id: self.session_id.clone(),
                request_id: pending.request_id,
                device_id: self.device_id.clone(),
                at_ms: pending.at_ms,
                payload: TestEventPayload::MediaFrameCaptured {
                    frame: pending.frame,
                },
            })
            .await?;
        state.pending = None;
        state.next_frame_index += 1;
        Ok(())
    }
}

fn ensure_active(control: &ExecutionControl) -> Result<(), MediaStreamError> {
    if control.is_cancelled() {
        Err(MediaStreamError::Cancelled)
    } else if control.is_expired() {
        Err(MediaStreamError::TimedOut)
    } else {
        Ok(())
    }
}

async fn deadline(control: &ExecutionControl) {
    match control.remaining() {
        Some(remaining) => tokio::time::sleep(remaining).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Cursor,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use devicerail_protocol::{
        AssetRef, EventSequence, RpcId, SessionExport, SessionId, SessionInfo, TestEvent,
    };
    use devicerail_protocol::{
        MediaStreamId, MediaStreamInfo, MediaStreamKind, SessionOutcome, TestEventPayload, Viewport,
    };
    use tokio::io::AsyncReadExt as _;

    use super::{MediaStreamError, MediaStreamWriter};
    use crate::{
        EndSession, EventStoreError, EvidenceError, EvidenceInput, EvidenceMetadata,
        EvidenceOutput, EvidenceResult, EvidenceStore, ExecutionControl, GcPolicy, GcReport,
        MemoryEventStore, ObservationLease, PendingEvent, PutEvidence, ReleaseReport,
        SessionEventStore, SessionExportPageSnapshot, Sha256Digest, StartSession, StoredEvidence,
        now_ms,
    };

    #[derive(Default)]
    struct TestEvidenceStore {
        cancel_after_put: Option<crate::ExecutionController>,
        cancel_after_attach: Option<crate::ExecutionController>,
        puts: AtomicUsize,
        attaches: AtomicUsize,
    }

    impl TestEvidenceStore {
        fn unused<T>() -> EvidenceResult<T> {
            Err(EvidenceError::Internal(
                "unused media test operation".to_owned(),
            ))
        }
    }

    #[async_trait]
    impl EvidenceStore for TestEvidenceStore {
        async fn put(
            &self,
            request: PutEvidence,
            mut input: EvidenceInput,
        ) -> EvidenceResult<StoredEvidence> {
            self.puts.fetch_add(1, Ordering::SeqCst);
            let (_session_id, media_type, _expected, declared) = request.into_parts();
            let mut bytes = Vec::new();
            input.read_to_end(&mut bytes).await.map_err(|error| {
                EvidenceError::Internal(format!("test evidence read failed: {error}"))
            })?;
            if declared != Some(bytes.len() as u64) {
                return Err(EvidenceError::DeclaredSizeMismatch {
                    declared: declared.unwrap_or_default(),
                    actual: bytes.len() as u64,
                });
            }
            let stored = StoredEvidence::new(
                EvidenceMetadata::new(
                    Sha256Digest::parse("a".repeat(64))?,
                    media_type,
                    bytes.len() as u64,
                    now_ms(),
                    1,
                )?,
                true,
            );
            if let Some(controller) = &self.cancel_after_put {
                controller.cancel(crate::CancellationReason::Requested);
            }
            Ok(stored)
        }

        async fn attach(
            &self,
            _session_id: &SessionId,
            asset: &AssetRef,
        ) -> EvidenceResult<StoredEvidence> {
            self.attaches.fetch_add(1, Ordering::SeqCst);
            let stored = StoredEvidence::new(
                EvidenceMetadata::new(
                    Sha256Digest::from_asset_ref(asset)?,
                    asset.media_type.clone(),
                    1,
                    now_ms(),
                    1,
                )?,
                true,
            );
            if let Some(controller) = &self.cancel_after_attach {
                controller.cancel(crate::CancellationReason::Requested);
            }
            Ok(stored)
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
            Ok(Vec::new())
        }

        async fn release_session(
            &self,
            session_id: &SessionId,
            _released_at_ms: u64,
        ) -> EvidenceResult<ReleaseReport> {
            Ok(ReleaseReport {
                session_id: session_id.clone(),
                released_references: 0,
                newly_unreferenced_assets: 0,
                newly_unreferenced_bytes: 0,
            })
        }

        async fn gc(&self, policy: GcPolicy) -> EvidenceResult<GcReport> {
            Ok(GcReport {
                dry_run: policy.dry_run,
                ..GcReport::default()
            })
        }
    }

    fn asset(media_type: &str) -> AssetRef {
        let digest = "b".repeat(64);
        AssetRef {
            id: format!("sha256:{digest}"),
            media_type: media_type.to_owned(),
            uri: format!("devicerail://assets/sha256/{digest}"),
            sha256: Some(digest),
        }
    }

    struct FailFrameOnceEventStore {
        inner: MemoryEventStore,
        fail_start_once: AtomicBool,
        fail_frame_once: AtomicBool,
        fail_terminal_once: AtomicBool,
    }

    impl Default for FailFrameOnceEventStore {
        fn default() -> Self {
            Self {
                inner: MemoryEventStore::default(),
                fail_start_once: AtomicBool::new(false),
                fail_frame_once: AtomicBool::new(true),
                fail_terminal_once: AtomicBool::new(true),
            }
        }
    }

    impl FailFrameOnceEventStore {
        fn with_lost_start_ack() -> Self {
            Self {
                fail_start_once: AtomicBool::new(true),
                fail_frame_once: AtomicBool::new(false),
                fail_terminal_once: AtomicBool::new(false),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl SessionEventStore for FailFrameOnceEventStore {
        async fn start_session(
            &self,
            command: StartSession,
        ) -> crate::EventStoreResult<SessionInfo> {
            self.inner.start_session(command).await
        }

        async fn reserve_observation(
            &self,
            session_id: &SessionId,
        ) -> crate::EventStoreResult<ObservationLease> {
            self.inner.reserve_observation(session_id).await
        }

        async fn release_observation(
            &self,
            session_id: &SessionId,
            lease: ObservationLease,
        ) -> crate::EventStoreResult<()> {
            self.inner.release_observation(session_id, lease).await
        }

        async fn append(&self, event: PendingEvent) -> crate::EventStoreResult<TestEvent> {
            let lose_start_ack =
                matches!(&event.payload, TestEventPayload::MediaStreamStarted { .. })
                    && self.fail_start_once.swap(false, Ordering::SeqCst);
            let lose_frame_ack =
                matches!(&event.payload, TestEventPayload::MediaFrameCaptured { .. })
                    && self.fail_frame_once.swap(false, Ordering::SeqCst);
            let lose_terminal_ack =
                matches!(&event.payload, TestEventPayload::MediaStreamEnded { .. })
                    && self.fail_terminal_once.swap(false, Ordering::SeqCst);
            let appended = self.inner.append(event).await?;
            if lose_start_ack || lose_frame_ack || lose_terminal_ack {
                return Err(EventStoreError::Internal(
                    "injected lost media append acknowledgement".to_owned(),
                ));
            }
            Ok(appended)
        }

        async fn end_session(&self, command: EndSession) -> crate::EventStoreResult<SessionInfo> {
            self.inner.end_session(command).await
        }

        async fn list_after(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
        ) -> crate::EventStoreResult<Vec<TestEvent>> {
            self.inner.list_after(session_id, after).await
        }

        async fn list_sessions(&self) -> crate::EventStoreResult<Vec<SessionInfo>> {
            self.inner.list_sessions().await
        }

        async fn export_session(
            &self,
            session_id: &SessionId,
        ) -> crate::EventStoreResult<SessionExport> {
            self.inner.export_session(session_id).await
        }

        async fn export_session_page(
            &self,
            session_id: &SessionId,
            after: Option<EventSequence>,
            limit: usize,
        ) -> crate::EventStoreResult<SessionExportPageSnapshot> {
            self.inner
                .export_session_page(session_id, after, limit)
                .await
        }

        async fn delete_ended(&self, session_id: &SessionId) -> crate::EventStoreResult<()> {
            self.inner.delete_ended(session_id).await
        }
    }

    #[tokio::test]
    async fn frames_are_deduplicated_evidence_references_with_closed_lifecycle() {
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::default());
        let evidence_trait: Arc<dyn EvidenceStore> = evidence.clone();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence_trait,
            session_id.clone(),
            None,
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: Some(Viewport {
                    width: 1,
                    height: 1,
                    scale_factor: 1.0,
                }),
            },
            now_ms(),
        );
        let bytes = b"bounded-frame".to_vec();
        let first = stream
            .push_frame(
                &ExecutionControl::unbounded(),
                now_ms(),
                true,
                Some(16),
                bytes.len() as u64,
                Box::pin(Cursor::new(bytes.clone())),
            )
            .await
            .expect("first frame");
        assert!(matches!(
            events
                .end_session(EndSession {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    outcome: SessionOutcome::Completed,
                    reason: None,
                })
                .await,
            Err(EventStoreError::MediaStreamsInFlight { count: 1, .. })
        ));
        let mut out_of_order = first.clone();
        out_of_order.frame_index = devicerail_protocol::EventSequence::new(3).expect("sequence");
        assert_eq!(
            events
                .append(PendingEvent {
                    session_id: session_id.clone(),
                    request_id: None,
                    device_id: None,
                    at_ms: now_ms(),
                    payload: TestEventPayload::MediaFrameCaptured {
                        frame: out_of_order,
                    },
                })
                .await,
            Err(EventStoreError::MediaFrameOutOfOrder)
        );
        let second = stream
            .push_frame(
                &ExecutionControl::unbounded(),
                now_ms(),
                false,
                Some(16),
                bytes.len() as u64,
                Box::pin(Cursor::new(bytes)),
            )
            .await
            .expect("second frame");
        assert_eq!(first.evidence, second.evidence);
        assert_eq!(stream.finish(now_ms()).await.expect("finish"), 2);
        assert_eq!(stream.finish(now_ms()).await.expect("idempotent finish"), 2);
        assert_eq!(stream.abort(now_ms()).await.expect("idempotent abort"), 2);
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
        let exported = events.export_session(&session_id).await.expect("export");
        assert_eq!(
            exported
                .events
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    TestEventPayload::MediaFrameCaptured { .. }
                ))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn append_failure_keeps_one_pending_reference_for_abort_recovery() {
        let events = Arc::new(FailFrameOnceEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::default());
        let evidence_trait: Arc<dyn EvidenceStore> = evidence.clone();
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence_trait,
            session_id.clone(),
            None,
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: None,
            },
            now_ms(),
        );
        let bytes = b"pending-frame".to_vec();
        let error = stream
            .push_frame(
                &ExecutionControl::unbounded(),
                now_ms(),
                true,
                None,
                bytes.len() as u64,
                Box::pin(Cursor::new(bytes)),
            )
            .await
            .expect_err("first frame append is injected to fail");
        assert!(matches!(
            error,
            MediaStreamError::Event(EventStoreError::Internal(_))
        ));
        assert_eq!(evidence.puts.load(Ordering::SeqCst), 1);

        assert!(matches!(
            stream.abort(now_ms()).await,
            Err(MediaStreamError::Event(EventStoreError::Internal(_)))
        ));
        assert_eq!(evidence.puts.load(Ordering::SeqCst), 1);
        assert_eq!(
            stream
                .abort(now_ms())
                .await
                .expect("abort retries terminal"),
            1
        );
        assert_eq!(
            stream
                .finish(now_ms())
                .await
                .expect("close remains idempotent"),
            1
        );
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Failed,
                reason: Some("producer failed".to_owned()),
            })
            .await
            .expect("end Session after recovery");
        let exported = events
            .export_session(&session_id)
            .await
            .expect("export Session");
        assert_eq!(
            exported
                .events
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    TestEventPayload::MediaFrameCaptured { .. }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cancellation_after_evidence_commit_still_appends_and_abort_closes_idempotently() {
        let events = Arc::new(MemoryEventStore::default());
        let (controller, control) = crate::ExecutionController::new();
        let evidence: Arc<dyn EvidenceStore> = Arc::new(TestEvidenceStore {
            cancel_after_put: Some(controller),
            ..TestEvidenceStore::default()
        });
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence,
            session_id.clone(),
            None,
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Video,
                media_type: "video/webm".to_owned(),
                viewport: None,
            },
            now_ms(),
        );
        let bytes = b"committed-before-cancel".to_vec();
        let frame = stream
            .push_frame(
                &control,
                now_ms(),
                true,
                Some(16),
                bytes.len() as u64,
                Box::pin(Cursor::new(bytes)),
            )
            .await
            .expect("committed frame is finalized despite cancellation");
        assert!(control.is_cancelled());
        assert_eq!(frame.frame_index, devicerail_protocol::EventSequence::FIRST);
        assert_eq!(
            stream.abort(now_ms()).await.expect("abort closes stream"),
            1
        );
        assert_eq!(
            stream.abort(now_ms()).await.expect("abort is idempotent"),
            1
        );
        events
            .end_session(EndSession {
                session_id,
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Cancelled,
                reason: Some("producer cancelled".to_owned()),
            })
            .await
            .expect("end Session after abort");
    }

    #[tokio::test]
    async fn attached_asset_becomes_the_canonical_frame_reference() {
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence.clone(),
            session_id.clone(),
            None,
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: None,
            },
            now_ms(),
        );
        let attached = asset("image/png");
        let frame = stream
            .push_asset_frame(
                &ExecutionControl::unbounded(),
                now_ms(),
                true,
                None,
                &attached,
            )
            .await
            .expect("attach media frame");
        assert_eq!(frame.evidence, attached);
        assert_eq!(evidence.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(stream.finish(now_ms()).await.expect("finish stream"), 1);

        let events = events
            .list_after(&session_id, None)
            .await
            .expect("list events");
        assert!(events.iter().any(|event| {
            matches!(
                &event.payload,
                TestEventPayload::MediaFrameCaptured { frame }
                    if frame.evidence == attached
            )
        }));
    }

    #[tokio::test]
    async fn asset_media_type_mismatch_fails_before_attach() {
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence.clone(),
            session_id,
            None,
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: None,
            },
            now_ms(),
        );
        assert!(matches!(
            stream
                .push_asset_frame(
                    &ExecutionControl::unbounded(),
                    now_ms(),
                    false,
                    None,
                    &asset("video/webm"),
                )
                .await,
            Err(MediaStreamError::InvalidFrame)
        ));
        assert_eq!(evidence.attaches.load(Ordering::SeqCst), 0);
        assert_eq!(stream.abort(now_ms()).await.expect("abort stream"), 0);
    }

    #[tokio::test]
    async fn pre_cancelled_asset_frame_never_attaches() {
        let events = Arc::new(MemoryEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence.clone(),
            session_id,
            None,
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: None,
            },
            now_ms(),
        );
        let (controller, control) = crate::ExecutionController::new();
        controller.cancel(crate::CancellationReason::Requested);
        assert!(matches!(
            stream
                .push_asset_frame(&control, now_ms(), false, None, &asset("image/png"))
                .await,
            Err(MediaStreamError::Cancelled)
        ));
        assert_eq!(evidence.attaches.load(Ordering::SeqCst), 0);
        assert_eq!(stream.abort(now_ms()).await.expect("abort stream"), 0);
    }

    #[tokio::test]
    async fn cancellation_after_attach_still_appends_the_matching_frame() {
        let events = Arc::new(MemoryEventStore::default());
        let (controller, control) = crate::ExecutionController::new();
        let evidence = Arc::new(TestEvidenceStore {
            cancel_after_attach: Some(controller),
            ..TestEvidenceStore::default()
        });
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence.clone(),
            session_id.clone(),
            None,
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Video,
                media_type: "video/webm".to_owned(),
                viewport: None,
            },
            now_ms(),
        );
        let frame = stream
            .push_asset_frame(&control, now_ms(), true, Some(16), &asset("video/webm"))
            .await
            .expect("finalize attached frame after cancellation");
        assert!(control.is_cancelled());
        assert_eq!(frame.frame_index, EventSequence::FIRST);
        assert_eq!(evidence.attaches.load(Ordering::SeqCst), 1);
        assert_eq!(stream.abort(now_ms()).await.expect("abort stream"), 1);
        assert_eq!(
            events
                .list_after(&session_id, None)
                .await
                .expect("list events")
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    TestEventPayload::MediaFrameCaptured { .. }
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn lost_start_ack_is_recovered_by_retained_writer_before_close() {
        let events = Arc::new(FailFrameOnceEventStore::with_lost_start_ack());
        let evidence: Arc<dyn EvidenceStore> = Arc::new(TestEvidenceStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence,
            session_id.clone(),
            Some(RpcId::Number(41)),
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: None,
            },
            now_ms(),
        );

        assert!(matches!(
            stream.ensure_started().await,
            Err(MediaStreamError::Event(EventStoreError::Internal(_)))
        ));
        assert_eq!(
            stream
                .abort_with_request_id(Some(RpcId::Number(42)), now_ms())
                .await
                .expect("close recovers exact start append"),
            0
        );
        events
            .end_session(EndSession {
                session_id: session_id.clone(),
                request_id: None,
                device_id: None,
                at_ms: now_ms(),
                outcome: SessionOutcome::Failed,
                reason: Some("start acknowledgement lost".to_owned()),
            })
            .await
            .expect("end Session after recovered start");

        let media = events
            .list_after(&session_id, None)
            .await
            .expect("list recovered lifecycle")
            .into_iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    TestEventPayload::MediaStreamStarted { .. }
                        | TestEventPayload::MediaStreamEnded { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(media.len(), 2);
        assert_eq!(media[0].request_id, Some(RpcId::Number(41)));
        assert_eq!(media[1].request_id, Some(RpcId::Number(42)));
    }

    #[tokio::test]
    async fn ambiguous_asset_frame_append_recovers_without_reattach_or_sequence_gap() {
        let events = Arc::new(FailFrameOnceEventStore::default());
        let evidence = Arc::new(TestEvidenceStore::default());
        let start = StartSession::new(None, None, now_ms());
        let session_id = start.session_id.clone();
        events.start_session(start).await.expect("start Session");
        let stream = MediaStreamWriter::prepare(
            Arc::clone(&events),
            evidence.clone(),
            session_id.clone(),
            Some(RpcId::Number(10)),
            None,
            MediaStreamInfo {
                id: MediaStreamId::new(),
                kind: MediaStreamKind::Screenshot,
                media_type: "image/png".to_owned(),
                viewport: None,
            },
            now_ms(),
        );
        assert!(matches!(
            stream
                .push_asset_frame_with_request_id(
                    &ExecutionControl::unbounded(),
                    Some(RpcId::Number(20)),
                    now_ms(),
                    true,
                    None,
                    &asset("image/png"),
                )
                .await,
            Err(MediaStreamError::Event(EventStoreError::Internal(_)))
        ));
        assert_eq!(evidence.attaches.load(Ordering::SeqCst), 1);
        assert!(matches!(
            stream
                .abort_with_request_id(Some(RpcId::Number(30)), now_ms())
                .await,
            Err(MediaStreamError::Event(EventStoreError::Internal(_)))
        ));
        assert_eq!(
            stream
                .abort_with_request_id(Some(RpcId::Number(31)), now_ms())
                .await
                .expect("retry abort"),
            1
        );
        assert_eq!(evidence.attaches.load(Ordering::SeqCst), 1);

        let recorded = events
            .list_after(&session_id, None)
            .await
            .expect("list media lifecycle");
        assert_eq!(recorded.len(), 4);
        assert_eq!(
            recorded
                .iter()
                .map(|event| event.sequence.get())
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    TestEventPayload::MediaFrameCaptured { .. }
                ))
                .count(),
            1
        );
        let media_request_ids = recorded
            .iter()
            .filter(|event| {
                matches!(
                    event.payload,
                    TestEventPayload::MediaStreamStarted { .. }
                        | TestEventPayload::MediaFrameCaptured { .. }
                        | TestEventPayload::MediaStreamEnded { .. }
                )
            })
            .map(|event| event.request_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            media_request_ids,
            vec![
                Some(RpcId::Number(10)),
                Some(RpcId::Number(20)),
                Some(RpcId::Number(30)),
            ]
        );
    }
}
