use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use devicerail_core::{CancellationReason, ExecutionControl, SessionEventStore};
use thiserror::Error;
use tokio::{
    io::{
        AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _,
        BufReader,
    },
    sync::{Semaphore, mpsc, watch},
    task::JoinSet,
};
use uuid::Uuid;

use crate::{
    MAX_PEER_FRAME_BYTES, PeerError, PeerRequest, PeerResponse, PeerSecurity, RegistryPeerService,
    model::{wire_protocol_version, wire_schema_accepts},
};

const MAX_IN_FLIGHT_PEER_REQUESTS: usize = 64;
const PEER_FRAME_COMPLETION_DEADLINE: Duration = Duration::from_secs(30);
const PEER_DRAIN_GRACE: Duration = Duration::from_secs(5);
const PEER_WRITE_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum PeerServerError {
    #[error("peer stream security attestation is invalid")]
    Security,
    #[error("peer stream frame is invalid or oversized")]
    Protocol,
    #[error("peer stream protocol version is unsupported")]
    UnsupportedVersion,
    #[error("peer stream partial frame timed out")]
    TimedOut,
    #[error("peer stream I/O failed")]
    Io,
    #[error("peer stream request task failed")]
    Task,
    #[error("peer stream authenticated resources could not be cleaned up")]
    Cleanup,
}

/// Serves peer-v2 over one already authenticated stream.
///
/// Requests execute concurrently so a following `cancel` frame can interrupt
/// an in-flight operation. Admission and response queues are bounded. Cancel
/// frames bypass normal admission, preventing saturation from starving them.
/// Idle streams remain open; the read deadline starts only after the first
/// byte of a frame. EOF cancels admitted requests and admits lease teardown to
/// a bounded service-owned cleanup task. This function waits a short grace for
/// completion; if that grace expires, cleanup continues independently with
/// bounded retries and staged progress rather than being cancelled with the
/// stream future.
pub async fn serve_peer_stream<T, S>(
    stream: T,
    security: PeerSecurity,
    service: Arc<RegistryPeerService<S>>,
) -> Result<(), PeerServerError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: SessionEventStore + ?Sized + 'static,
{
    serve_peer_stream_until_cancelled(stream, security, service, ExecutionControl::unbounded())
        .await
}

/// Serves peer-v2 until the stream ends or `shutdown` is cancelled.
///
/// Cancellation is graceful: it stops admitting frames, cancels admitted
/// requests, drains them for a bounded grace, admits connection-owned lease
/// cleanup, and shuts down the response writer. It is therefore safe for a
/// listener supervisor to cancel an otherwise idle stream without aborting its
/// task and bypassing authenticated resource cleanup.
pub async fn serve_peer_stream_until_cancelled<T, S>(
    stream: T,
    security: PeerSecurity,
    service: Arc<RegistryPeerService<S>>,
    shutdown: ExecutionControl,
) -> Result<(), PeerServerError>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: SessionEventStore + ?Sized + 'static,
{
    if security.subject().is_empty() {
        return Err(PeerServerError::Security);
    }
    let connection_id = Uuid::new_v4();
    let (read, mut write) = tokio::io::split(stream);
    let mut reader = BufReader::new(read);
    let (responses, mut response_rx) = mpsc::channel::<Vec<u8>>(MAX_IN_FLIGHT_PEER_REQUESTS);
    let (connection_failed_tx, mut connection_failed_rx) = watch::channel(false);
    let writer_failed_tx = connection_failed_tx.clone();
    let writer = tokio::spawn(async move {
        while let Some(frame) = response_rx.recv().await {
            let result = tokio::time::timeout(PEER_WRITE_GRACE, async {
                write.write_all(&frame).await?;
                write.flush().await
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                let _ = writer_failed_tx.send(true);
                return Err(PeerServerError::Io);
            }
        }
        let result = tokio::time::timeout(PEER_WRITE_GRACE, write.shutdown())
            .await
            .map_err(|_| PeerServerError::Io)?
            .map_err(|_| PeerServerError::Io);
        if result.is_err() {
            let _ = writer_failed_tx.send(true);
        }
        result
    });
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_PEER_REQUESTS));
    let mut tasks = JoinSet::new();
    let admitted = Arc::new(StdMutex::new(BTreeSet::new()));
    let mut read_error = None;

    loop {
        let line = match tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            changed = connection_failed_rx.changed() => {
                if changed.is_err() || *connection_failed_rx.borrow() {
                    None
                } else {
                    continue;
                }
            },
            joined = tasks.join_next(), if !tasks.is_empty() => {
                match joined {
                    Some(Ok(())) | None => continue,
                    Some(Err(_)) => {
                        read_error = Some(PeerServerError::Task);
                        break;
                    }
                }
            },
            result = read_frame(&mut reader) => Some(result),
        } {
            Some(Ok(Some(line))) => line,
            Some(Ok(None)) => break,
            Some(Err(error)) => {
                read_error = Some(error);
                break;
            }
            None => {
                read_error = Some(PeerServerError::Io);
                break;
            }
        };
        if !wire_schema_accepts(&line) {
            read_error = Some(
                if wire_protocol_version(&line)
                    .is_some_and(|version| version != crate::DISTRIBUTED_PROTOCOL_VERSION)
                {
                    PeerServerError::UnsupportedVersion
                } else {
                    PeerServerError::Protocol
                },
            );
            break;
        }
        let request: PeerRequest = match serde_json::from_slice(&line) {
            Ok(request) => request,
            Err(_) => {
                read_error = Some(PeerServerError::Protocol);
                break;
            }
        };
        if request.validate().is_err() || request.node_id != *service.node_id() {
            read_error = Some(PeerServerError::Protocol);
            break;
        }

        if matches!(request.operation, crate::PeerOperation::Cancel { .. }) {
            let response = match service
                .handle(request.clone(), &security, connection_id)
                .await
            {
                Ok(response) => response,
                Err(_) => service_failure(&request, service.epoch()),
            };
            let frame = encode_response(&request, response, service.epoch())?;
            if responses.try_send(frame).is_err() {
                read_error = Some(PeerServerError::Io);
                break;
            }
            continue;
        }

        let permit = match Arc::clone(&permits).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                let response = PeerResponse::failure(
                    &request,
                    service.epoch(),
                    PeerError {
                        code: "too_many_peer_requests".to_owned(),
                        retryable: true,
                        outcome_unknown: false,
                    },
                );
                let frame = encode_response(&request, response, service.epoch())?;
                if responses.try_send(frame).is_err() {
                    read_error = Some(PeerServerError::Io);
                    break;
                }
                continue;
            }
        };
        let inserted = admitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(request.request_id);
        if !inserted {
            let response = PeerResponse::failure(
                &request,
                service.epoch(),
                PeerError {
                    code: "duplicate_request_id".to_owned(),
                    retryable: false,
                    outcome_unknown: false,
                },
            );
            let frame = encode_response(&request, response, service.epoch())?;
            if responses.try_send(frame).is_err() {
                read_error = Some(PeerServerError::Io);
                break;
            }
            continue;
        }

        let service = Arc::clone(&service);
        let responses = responses.clone();
        let security = security.clone();
        let admitted = Arc::clone(&admitted);
        let connection_failed = connection_failed_tx.clone();
        tasks.spawn(async move {
            let _permit = permit;
            let request_copy = request.clone();
            let response = match service.handle(request, &security, connection_id).await {
                Ok(response) => response,
                Err(_) => service_failure(&request_copy, service.epoch()),
            };
            let delivered = encode_response(&request_copy, response, service.epoch())
                .is_ok_and(|frame| responses.try_send(frame).is_ok());
            if !delivered {
                let _ = connection_failed.send(true);
            }
            admitted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&request_copy.request_id);
        });
    }

    let admitted = admitted
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .copied()
        .collect::<Vec<_>>();
    for request_id in admitted {
        service.cancel_request(connection_id, request_id, CancellationReason::Shutdown);
    }
    let mut request_task_failed = false;
    if tokio::time::timeout(PEER_DRAIN_GRACE, async {
        while let Some(joined) = tasks.join_next().await {
            request_task_failed |= joined.is_err();
        }
    })
    .await
    .is_err()
    {
        read_error.get_or_insert(PeerServerError::Io);
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    if request_task_failed {
        read_error.get_or_insert(PeerServerError::Task);
    }

    let cleanup = service.admit_connection_cleanup(connection_id).await;
    let cleanup_error = match tokio::time::timeout(PEER_DRAIN_GRACE, cleanup).await {
        Ok(Ok(errors)) if errors.is_empty() => None,
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => Some(PeerServerError::Cleanup),
    };
    drop(responses);
    let mut writer = writer;
    let writer_result = match tokio::time::timeout(PEER_DRAIN_GRACE, &mut writer).await {
        Ok(result) => result.map_err(|_| PeerServerError::Io)?,
        Err(_) => {
            writer.abort();
            let _ = writer.await;
            Err(PeerServerError::Io)
        }
    };
    if let Some(error) = read_error {
        return Err(error);
    }
    if let Some(error) = cleanup_error {
        return Err(error);
    }
    writer_result
}

async fn read_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>, PeerServerError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut first = [0_u8; 1];
    let read = reader
        .read(&mut first)
        .await
        .map_err(|_| PeerServerError::Io)?;
    if read == 0 {
        return Ok(None);
    }
    let mut bytes = vec![first[0]];
    if first[0] != b'\n' {
        let mut remainder = Vec::new();
        let mut bounded = reader.take(MAX_PEER_FRAME_BYTES as u64);
        tokio::time::timeout(
            PEER_FRAME_COMPLETION_DEADLINE,
            bounded.read_until(b'\n', &mut remainder),
        )
        .await
        .map_err(|_| PeerServerError::TimedOut)?
        .map_err(|_| PeerServerError::Io)?;
        bytes.extend_from_slice(&remainder);
    }
    if bytes.len() > MAX_PEER_FRAME_BYTES || bytes.last() != Some(&b'\n') {
        return Err(PeerServerError::Protocol);
    }
    bytes.pop();
    if bytes.is_empty() || bytes.last() == Some(&b'\r') {
        return Err(PeerServerError::Protocol);
    }
    Ok(Some(bytes))
}

fn service_failure(request: &PeerRequest, epoch: u64) -> PeerResponse {
    PeerResponse::failure(
        request,
        epoch,
        PeerError {
            code: "peer_service_error".to_owned(),
            retryable: false,
            outcome_unknown: false,
        },
    )
}

fn encode_response(
    request: &PeerRequest,
    response: PeerResponse,
    epoch: u64,
) -> Result<Vec<u8>, PeerServerError> {
    let mut bytes = serde_json::to_vec(&response).map_err(|_| PeerServerError::Protocol)?;
    if bytes.len() >= MAX_PEER_FRAME_BYTES {
        let outcome_unknown = matches!(request.operation, crate::PeerOperation::Execute { .. });
        let bounded = PeerResponse::failure(
            request,
            epoch,
            PeerError {
                code: "peer_response_too_large".to_owned(),
                retryable: false,
                outcome_unknown,
            },
        );
        bytes = serde_json::to_vec(&bounded).map_err(|_| PeerServerError::Protocol)?;
    }
    if bytes.is_empty() || bytes.len() >= MAX_PEER_FRAME_BYTES {
        return Err(PeerServerError::Protocol);
    }
    if !wire_schema_accepts(&bytes) {
        return Err(PeerServerError::Protocol);
    }
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use devicerail_protocol::{ActionCall, ActionResult};
    use serde_json::Value;
    use uuid::Uuid;

    use super::encode_response;
    use crate::{
        MAX_PEER_FRAME_BYTES, NodeId, PeerLease, PeerOperation, PeerRequest, PeerResponse,
        PeerResult,
    };

    #[test]
    fn oversized_execute_response_is_never_reported_as_a_known_failure() {
        let node_id = NodeId::parse("node-a").expect("node id");
        let lease = PeerLease {
            lease_id: Uuid::new_v4(),
            device_key: "device-a".to_owned(),
            owner_id: "owner-a".to_owned(),
            node_epoch: 1,
            expires_at_ms: 10_000,
        };
        let call = ActionCall {
            id: Uuid::new_v4(),
            name: "tap".to_owned(),
            arguments: serde_json::json!({"x": 1, "y": 1}),
        };
        let mut request = PeerRequest::new(
            node_id,
            Some(1),
            PeerOperation::Execute {
                device_key: lease.device_key.clone(),
                call: call.clone(),
                screenshot_omission: None,
                ui_snapshots_enabled: false,
                semantic_actions_enabled: false,
            },
        );
        request.lease = Some(lease);
        let response = PeerResponse::success(
            &request,
            1,
            PeerResult::Action {
                result: Box::new(ActionResult {
                    call_id: call.id,
                    started_at_ms: 1,
                    finished_at_ms: 2,
                    output: Value::String("x".repeat(MAX_PEER_FRAME_BYTES)),
                    before: None,
                    after: None,
                    evidence: Vec::new(),
                    execution: None,
                }),
            },
        );

        let mut frame = encode_response(&request, response, 1).expect("bounded fallback");
        assert_eq!(frame.pop(), Some(b'\n'));
        let response: PeerResponse = serde_json::from_slice(&frame).expect("fallback response");
        response
            .validate_for(&request)
            .expect("fallback remains correlated");
        let error = response.error.expect("oversize failure");
        assert_eq!(error.code, "peer_response_too_large");
        assert!(error.outcome_unknown);
    }
}
