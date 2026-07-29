use std::collections::{BTreeMap, BTreeSet};

use devicerail_core::Sha256Digest;
use devicerail_protocol::{
    ActionExecution, ActionOutcome, AssetRef, Observation, ProtocolVersion, RpcId,
    ScreenshotOmissionReason, SessionExport, SessionState, TestEventPayload, UiContextKind,
    UiSnapshotOmissionReason,
};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{
    BUNDLE_MAGIC, BUNDLE_VERSION, BundleAsset, BundleLimits, BundleManifest, BundleSource,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("unsupported Session Bundle magic")]
    UnsupportedMagic,
    #[error("unsupported Session Bundle version {0}")]
    UnsupportedBundleVersion(u16),
    #[error("unsupported event protocol version {major}.{minor}")]
    UnsupportedEventProtocol { major: u16, minor: u16 },
    #[error("{field} requires event protocol 1.2 or newer")]
    FieldRequiresProtocol12 { field: &'static str },
    #[error("{field} requires event protocol 1.4 or newer")]
    FieldRequiresProtocol14 { field: &'static str },
    #[error("{field} requires event protocol 1.5 or newer")]
    FieldRequiresProtocol15 { field: &'static str },
    #[error("{resource} exceeds its configured limit of {limit}")]
    LimitExceeded { resource: &'static str, limit: u64 },
    #[error("event JSON exceeds its configured nesting-depth limit")]
    JsonDepthLimit,
    #[error("Session must be ended before it can be bundled")]
    SessionNotEnded,
    #[error("ended Session metadata is inconsistent")]
    InvalidEndedSession,
    #[error("Session event sequence is empty or inconsistent")]
    InvalidEventSequence,
    #[error("event belongs to a different Session")]
    EventSessionMismatch,
    #[error("duplicate eventId in Session event sequence")]
    DuplicateEventId,
    #[error("Session lifecycle events are inconsistent")]
    InvalidLifecycle,
    #[error("duplicate ActionStarted call id {0}")]
    DuplicateActionCall(Uuid),
    #[error("ActionCompleted has no matching ActionStarted for call {0}")]
    ActionNotStarted(Uuid),
    #[error("Action event correlation changed for call {0}")]
    ActionCorrelationMismatch(Uuid),
    #[error("successful ActionResult does not match call {0}")]
    ActionResultMismatch(Uuid),
    #[error("ActionResult timestamps are inconsistent for call {0}")]
    ActionTimeMismatch(Uuid),
    #[error("Session ended with an Action still in flight")]
    ActionStillInFlight,
    #[error("media stream lifecycle is inconsistent")]
    InvalidMediaStream,
    #[error("Session ended with a media stream still in flight")]
    MediaStreamStillInFlight,
    #[error("redacted Action arguments must be null")]
    RedactedArgumentsNotNull,
    #[error("Observation screenshot and screenshotOmission are mutually exclusive")]
    ObservationScreenshotConflict,
    #[error("Observation uiSnapshot and uiSnapshotOmission are mutually exclusive")]
    ObservationUiSnapshotConflict,
    #[error("Observation UI Snapshot reference is invalid")]
    InvalidUiSnapshotReference,
    #[error("ActionResult execution channel is invalid")]
    InvalidActionExecution,
    #[error("Observation viewport contains a non-finite scale factor")]
    InvalidObservationViewport,
    #[error("protected successful Action does not preserve the required omission contract")]
    InvalidProtectedActionOmission,
    #[error("standard successful Action contains a protectedAction omission")]
    UnexpectedProtectedActionOmission,
    #[error("typed Evidence reference is not canonical")]
    InvalidEvidenceReference,
    #[error("digest {0} is referenced with conflicting media types")]
    ConflictingEvidenceMediaType(String),
    #[error("digest {0} is referenced with conflicting declared byte lengths")]
    ConflictingEvidenceByteLength(String),
    #[error("Bundle asset digest or media type is invalid")]
    InvalidAssetIndexEntry,
    #[error("Bundle asset path is not derived from its digest")]
    InvalidAssetPath,
    #[error("Bundle asset index is not strictly digest-sorted")]
    AssetIndexNotSorted,
    #[error("Bundle asset byte lengths exceed configured limits")]
    AssetSizeLimit,
    #[error("Bundle asset index does not exactly match typed event references")]
    AssetSetMismatch,
    #[error("Bundle asset media type does not match its typed event reference")]
    AssetMediaTypeMismatch,
    #[error("Bundle asset byte length does not match its UI Snapshot reference")]
    AssetByteLengthMismatch,
}

/// Validate a captured source before performing any filesystem I/O.
pub fn validate_source(
    source: &BundleSource,
    limits: &BundleLimits,
) -> Result<BTreeMap<Sha256Digest, AssetRef>, ModelError> {
    validate_event_protocol(source.event_protocol_version)?;
    validate_export(
        source.event_protocol_version,
        &source.session_export,
        limits,
    )
    .map(|references| references.by_digest)
}

/// Validate the manifest header, event state machine, and exact asset index.
///
/// File size and hash checks remain the filesystem validator's responsibility.
pub fn validate_manifest_events(
    manifest: &BundleManifest,
    limits: &BundleLimits,
) -> Result<BTreeMap<Sha256Digest, AssetRef>, ModelError> {
    if manifest.magic != BUNDLE_MAGIC {
        return Err(ModelError::UnsupportedMagic);
    }
    if manifest.bundle_version != BUNDLE_VERSION {
        return Err(ModelError::UnsupportedBundleVersion(
            manifest.bundle_version,
        ));
    }
    validate_event_protocol(manifest.event_protocol_version)?;

    let export = SessionExport {
        session: manifest.session.clone(),
        events: manifest.events.clone(),
    };
    let references = validate_export(manifest.event_protocol_version, &export, limits)?;
    validate_asset_index(&manifest.assets, &references, limits)?;
    Ok(references.by_digest)
}

fn validate_event_protocol(version: ProtocolVersion) -> Result<(), ModelError> {
    if version.major == 1 && version.minor <= 5 {
        Ok(())
    } else {
        Err(ModelError::UnsupportedEventProtocol {
            major: version.major,
            minor: version.minor,
        })
    }
}

fn validate_export<'a>(
    protocol: ProtocolVersion,
    export: &SessionExport,
    limits: &'a BundleLimits,
) -> Result<ReferenceCollector<'a>, ModelError> {
    if export.events.len() > limits.max_events {
        return Err(ModelError::LimitExceeded {
            resource: "events",
            limit: limits.max_events as u64,
        });
    }
    if export.session.state != SessionState::Ended {
        return Err(ModelError::SessionNotEnded);
    }
    let Some(ended_at_ms) = export.session.ended_at_ms else {
        return Err(ModelError::InvalidEndedSession);
    };
    let event_count = u64::try_from(export.events.len())
        .ok()
        .and_then(devicerail_protocol::EventSequence::new)
        .ok_or(ModelError::InvalidEventSequence)?;
    if export.session.event_count != event_count || export.session.last_sequence != event_count {
        return Err(ModelError::InvalidEventSequence);
    }

    let mut event_ids = BTreeSet::new();
    let mut seen_calls = BTreeSet::new();
    let mut in_flight = BTreeMap::new();
    let mut seen_media_streams = BTreeSet::new();
    let mut media_streams = BTreeMap::new();
    let mut references = ReferenceCollector::new(limits);
    for (index, event) in export.events.iter().enumerate() {
        let expected_sequence = devicerail_protocol::EventSequence::new(index as u64 + 1)
            .ok_or(ModelError::InvalidEventSequence)?;
        if event.sequence != expected_sequence {
            return Err(ModelError::InvalidEventSequence);
        }
        if event.session_id != export.session.id {
            return Err(ModelError::EventSessionMismatch);
        }
        if !event_ids.insert(event.event_id.clone()) {
            return Err(ModelError::DuplicateEventId);
        }
        let first = index == 0;
        let last = index + 1 == export.events.len();
        match &event.payload {
            TestEventPayload::SessionStarted => {
                if !first || event.at_ms != export.session.started_at_ms {
                    return Err(ModelError::InvalidLifecycle);
                }
            }
            TestEventPayload::SessionEnded { .. } => {
                if !last || event.at_ms != ended_at_ms {
                    return Err(ModelError::InvalidLifecycle);
                }
            }
            TestEventPayload::ObservationCaptured { observation } => {
                if first || last {
                    return Err(ModelError::InvalidLifecycle);
                }
                validate_observation(observation, protocol, limits)?;
                if observation.screenshot_omission
                    == Some(ScreenshotOmissionReason::ProtectedAction)
                    || observation.ui_snapshot_omission
                        == Some(UiSnapshotOmissionReason::ProtectedAction)
                {
                    return Err(ModelError::UnexpectedProtectedActionOmission);
                }
                if let Some(reference) = &observation.screenshot {
                    references.add(reference)?;
                }
                if let Some(snapshot) = &observation.ui_snapshot {
                    references.add_with_length(&snapshot.evidence, snapshot.byte_length)?;
                }
            }
            TestEventPayload::ActionStarted { call } => {
                if first || last {
                    return Err(ModelError::InvalidLifecycle);
                }
                if call.arguments_redacted {
                    require_protocol_12(protocol, "argumentsRedacted")?;
                    if !call.arguments.is_null() {
                        return Err(ModelError::RedactedArgumentsNotNull);
                    }
                }
                validate_json_value(&call.arguments, 1, limits)?;
                // A standard call whose arguments happen to be JSON null is valid.
                if !seen_calls.insert(call.id) {
                    return Err(ModelError::DuplicateActionCall(call.id));
                }
                in_flight.insert(
                    call.id,
                    ActionCorrelation {
                        request_id: event.request_id.clone(),
                        device_id: event.device_id.clone(),
                        arguments_redacted: call.arguments_redacted,
                    },
                );
            }
            TestEventPayload::ActionCompleted { call_id, outcome } => {
                if first || last {
                    return Err(ModelError::InvalidLifecycle);
                }
                let started = in_flight
                    .remove(call_id)
                    .ok_or(ModelError::ActionNotStarted(*call_id))?;
                if started.request_id != event.request_id || started.device_id != event.device_id {
                    return Err(ModelError::ActionCorrelationMismatch(*call_id));
                }
                match outcome {
                    ActionOutcome::Succeeded { result } => {
                        if result.call_id != *call_id {
                            return Err(ModelError::ActionResultMismatch(*call_id));
                        }
                        if result.finished_at_ms < result.started_at_ms {
                            return Err(ModelError::ActionTimeMismatch(*call_id));
                        }
                        validate_json_value(&result.output, 1, limits)?;
                        for observation in result.before.iter().chain(result.after.iter()) {
                            validate_observation(observation, protocol, limits)?;
                        }
                        if let Some(execution) = &result.execution {
                            require_protocol_15(protocol, "execution")?;
                            let (context, expected_kind) = match execution {
                                ActionExecution::NativeSemantic { context } => {
                                    (context, Some(UiContextKind::Native))
                                }
                                ActionExecution::WebSemantic { context } => {
                                    (context, Some(UiContextKind::Web))
                                }
                                ActionExecution::CoordinateFallback { context, .. } => {
                                    (context, None)
                                }
                            };
                            if context.validate().is_err()
                                || expected_kind.is_some_and(|kind| context.context_kind != kind)
                            {
                                return Err(ModelError::InvalidActionExecution);
                            }
                        }
                        validate_success_omission(protocol, started.arguments_redacted, result)?;
                        for reference in result
                            .before
                            .iter()
                            .filter_map(|observation| observation.screenshot.as_ref())
                            .chain(
                                result
                                    .after
                                    .iter()
                                    .filter_map(|observation| observation.screenshot.as_ref()),
                            )
                            .chain(result.evidence.iter())
                        {
                            references.add(reference)?;
                        }
                        for snapshot in result
                            .before
                            .iter()
                            .chain(result.after.iter())
                            .filter_map(|observation| observation.ui_snapshot.as_ref())
                        {
                            references.add_with_length(&snapshot.evidence, snapshot.byte_length)?;
                        }
                    }
                    ActionOutcome::Failed { error }
                    | ActionOutcome::Cancelled { error }
                    | ActionOutcome::TimedOut { error, .. } => {
                        validate_error_details(error, limits)?;
                    }
                }
            }
            TestEventPayload::MediaStreamStarted { stream } => {
                require_protocol_14(protocol, "mediaStreamStarted")?;
                if first
                    || last
                    || stream.media_type.is_empty()
                    || stream.media_type.len() > 255
                    || stream
                        .media_type
                        .bytes()
                        .any(|byte| byte.is_ascii_control())
                    || !seen_media_streams.insert(stream.id.clone())
                    || stream.viewport.as_ref().is_some_and(|viewport| {
                        viewport.width == 0
                            || viewport.height == 0
                            || !viewport.scale_factor.is_finite()
                            || viewport.scale_factor <= 0.0
                    })
                {
                    return Err(ModelError::InvalidMediaStream);
                }
                media_streams.insert(stream.id.clone(), (stream.media_type.clone(), 1_u64));
            }
            TestEventPayload::MediaFrameCaptured { frame } => {
                require_protocol_14(protocol, "mediaFrameCaptured")?;
                if first || last {
                    return Err(ModelError::InvalidLifecycle);
                }
                let (media_type, next_index) = media_streams
                    .get_mut(&frame.stream_id)
                    .ok_or(ModelError::InvalidMediaStream)?;
                if frame.frame_index.get() != *next_index
                    || frame.evidence.media_type != *media_type
                    || frame
                        .duration_ms
                        .is_some_and(|value| value > devicerail_protocol::MAX_SAFE_INTEGER)
                {
                    return Err(ModelError::InvalidMediaStream);
                }
                references.add(&frame.evidence)?;
                *next_index += 1;
            }
            TestEventPayload::MediaStreamEnded {
                stream_id,
                frame_count,
            } => {
                require_protocol_14(protocol, "mediaStreamEnded")?;
                if first || last {
                    return Err(ModelError::InvalidLifecycle);
                }
                let (_, next_index) = media_streams
                    .remove(stream_id)
                    .ok_or(ModelError::InvalidMediaStream)?;
                if *frame_count != next_index - 1 {
                    return Err(ModelError::InvalidMediaStream);
                }
            }
            TestEventPayload::VerdictRecorded { verdict } => {
                if first || last {
                    return Err(ModelError::InvalidLifecycle);
                }
                for reference in &verdict.evidence {
                    references.add(reference)?;
                }
            }
            TestEventPayload::Error { error } => {
                if first || last {
                    return Err(ModelError::InvalidLifecycle);
                }
                validate_error_details(error, limits)?;
            }
        }

        if first && !matches!(&event.payload, TestEventPayload::SessionStarted) {
            return Err(ModelError::InvalidLifecycle);
        }
        if last && !matches!(&event.payload, TestEventPayload::SessionEnded { .. }) {
            return Err(ModelError::InvalidLifecycle);
        }
    }

    if !in_flight.is_empty() {
        return Err(ModelError::ActionStillInFlight);
    }
    if !media_streams.is_empty() {
        return Err(ModelError::MediaStreamStillInFlight);
    }
    if references.by_digest.len() > limits.max_assets {
        return Err(ModelError::LimitExceeded {
            resource: "unique typed Evidence assets",
            limit: limits.max_assets as u64,
        });
    }
    Ok(references)
}

#[derive(Clone)]
struct ActionCorrelation {
    request_id: Option<RpcId>,
    device_id: Option<devicerail_protocol::DeviceId>,
    arguments_redacted: bool,
}

fn validate_observation(
    observation: &Observation,
    protocol: ProtocolVersion,
    limits: &BundleLimits,
) -> Result<(), ModelError> {
    if observation.screenshot.is_some() && observation.screenshot_omission.is_some() {
        return Err(ModelError::ObservationScreenshotConflict);
    }
    if observation.screenshot_omission.is_some() {
        require_protocol_12(protocol, "screenshotOmission")?;
    }
    if observation.ui_snapshot.is_some() && observation.ui_snapshot_omission.is_some() {
        return Err(ModelError::ObservationUiSnapshotConflict);
    }
    if observation.ui_snapshot.is_some() || observation.ui_snapshot_omission.is_some() {
        require_protocol_15(protocol, "uiSnapshot")?;
    }
    if observation
        .ui_snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.validate().is_err())
    {
        return Err(ModelError::InvalidUiSnapshotReference);
    }
    if !observation.viewport.scale_factor.is_finite() {
        return Err(ModelError::InvalidObservationViewport);
    }
    for value in observation.metadata.values() {
        validate_json_value(value, 2, limits)?;
    }
    Ok(())
}

fn validate_error_details(
    error: &devicerail_protocol::ErrorInfo,
    limits: &BundleLimits,
) -> Result<(), ModelError> {
    if let Some(details) = &error.details {
        validate_json_value(details, 1, limits)?;
    }
    Ok(())
}

fn validate_json_value(
    root: &serde_json::Value,
    initial_depth: usize,
    limits: &BundleLimits,
) -> Result<(), ModelError> {
    let mut pending = vec![(root, initial_depth)];
    let mut observed_nodes = 0_usize;
    while let Some((value, depth)) = pending.pop() {
        if depth > limits.max_json_depth {
            return Err(ModelError::JsonDepthLimit);
        }
        observed_nodes = observed_nodes
            .checked_add(1)
            .ok_or(ModelError::LimitExceeded {
                resource: "event JSON nodes",
                limit: limits.max_json_nodes as u64,
            })?;
        if observed_nodes > limits.max_json_nodes {
            return Err(ModelError::LimitExceeded {
                resource: "event JSON nodes",
                limit: limits.max_json_nodes as u64,
            });
        }
        let next_depth = depth.checked_add(1).ok_or(ModelError::JsonDepthLimit)?;
        let remaining = limits.max_json_nodes.saturating_sub(observed_nodes);
        if pending.len() > remaining {
            return Err(ModelError::LimitExceeded {
                resource: "event JSON nodes",
                limit: limits.max_json_nodes as u64,
            });
        }
        let available = remaining - pending.len();
        match value {
            serde_json::Value::Array(values) => {
                if values.len() > available {
                    return Err(ModelError::LimitExceeded {
                        resource: "event JSON nodes",
                        limit: limits.max_json_nodes as u64,
                    });
                }
                pending.extend(values.iter().map(|value| (value, next_depth)));
            }
            serde_json::Value::Object(values) => {
                if values.len() > available {
                    return Err(ModelError::LimitExceeded {
                        resource: "event JSON nodes",
                        limit: limits.max_json_nodes as u64,
                    });
                }
                pending.extend(values.values().map(|value| (value, next_depth)));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_success_omission(
    protocol: ProtocolVersion,
    arguments_redacted: bool,
    result: &devicerail_protocol::ActionResult,
) -> Result<(), ModelError> {
    if arguments_redacted {
        let protected_observation = |observation: Option<&Observation>| {
            observation.is_some_and(|observation| {
                observation.screenshot.is_none()
                    && observation.screenshot_omission
                        == Some(ScreenshotOmissionReason::ProtectedAction)
                    && observation.ui_snapshot.is_none()
                    && (protocol.minor < 5
                        || observation.ui_snapshot_omission.is_none()
                        || observation.ui_snapshot_omission
                            == Some(UiSnapshotOmissionReason::ProtectedAction))
            })
        };
        if !result.evidence.is_empty()
            || !protected_observation(result.before.as_ref())
            || !protected_observation(result.after.as_ref())
        {
            return Err(ModelError::InvalidProtectedActionOmission);
        }
    } else if result
        .before
        .iter()
        .chain(result.after.iter())
        .any(|observation| {
            observation.screenshot_omission == Some(ScreenshotOmissionReason::ProtectedAction)
                || observation.ui_snapshot_omission
                    == Some(UiSnapshotOmissionReason::ProtectedAction)
        })
    {
        return Err(ModelError::UnexpectedProtectedActionOmission);
    }
    Ok(())
}

fn require_protocol_12(protocol: ProtocolVersion, field: &'static str) -> Result<(), ModelError> {
    if protocol.major == 1 && protocol.minor >= 2 {
        Ok(())
    } else {
        Err(ModelError::FieldRequiresProtocol12 { field })
    }
}

fn require_protocol_14(protocol: ProtocolVersion, field: &'static str) -> Result<(), ModelError> {
    if protocol.major == 1 && protocol.minor >= 4 {
        Ok(())
    } else {
        Err(ModelError::FieldRequiresProtocol14 { field })
    }
}

fn require_protocol_15(protocol: ProtocolVersion, field: &'static str) -> Result<(), ModelError> {
    if protocol.major == 1 && protocol.minor >= 5 {
        Ok(())
    } else {
        Err(ModelError::FieldRequiresProtocol15 { field })
    }
}

struct ReferenceCollector<'a> {
    limits: &'a BundleLimits,
    typed_reference_count: usize,
    by_digest: BTreeMap<Sha256Digest, AssetRef>,
    declared_byte_lengths: BTreeMap<Sha256Digest, u64>,
}

impl<'a> ReferenceCollector<'a> {
    fn new(limits: &'a BundleLimits) -> Self {
        Self {
            limits,
            typed_reference_count: 0,
            by_digest: BTreeMap::new(),
            declared_byte_lengths: BTreeMap::new(),
        }
    }

    fn add(&mut self, reference: &AssetRef) -> Result<(), ModelError> {
        self.typed_reference_count =
            self.typed_reference_count
                .checked_add(1)
                .ok_or(ModelError::LimitExceeded {
                    resource: "typed Evidence references",
                    limit: self.limits.max_typed_references as u64,
                })?;
        if self.typed_reference_count > self.limits.max_typed_references {
            return Err(ModelError::LimitExceeded {
                resource: "typed Evidence references",
                limit: self.limits.max_typed_references as u64,
            });
        }

        let digest = Sha256Digest::from_asset_ref(reference)
            .map_err(|_| ModelError::InvalidEvidenceReference)?;
        if let Some(existing) = self.by_digest.get(&digest) {
            if existing.media_type != reference.media_type {
                return Err(ModelError::ConflictingEvidenceMediaType(digest.to_string()));
            }
        } else {
            if self.by_digest.len() >= self.limits.max_assets {
                return Err(ModelError::LimitExceeded {
                    resource: "unique typed Evidence assets",
                    limit: self.limits.max_assets as u64,
                });
            }
            self.by_digest.insert(digest, reference.clone());
        }
        Ok(())
    }

    fn add_with_length(
        &mut self,
        reference: &AssetRef,
        byte_length: u64,
    ) -> Result<(), ModelError> {
        self.add(reference)?;
        let digest = Sha256Digest::from_asset_ref(reference)
            .map_err(|_| ModelError::InvalidEvidenceReference)?;
        if let Some(existing) = self.declared_byte_lengths.get(&digest) {
            if *existing != byte_length {
                return Err(ModelError::ConflictingEvidenceByteLength(
                    digest.to_string(),
                ));
            }
        } else {
            self.declared_byte_lengths.insert(digest, byte_length);
        }
        Ok(())
    }
}

fn validate_asset_index(
    assets: &[BundleAsset],
    references: &ReferenceCollector<'_>,
    limits: &BundleLimits,
) -> Result<(), ModelError> {
    if assets.len() > limits.max_assets {
        return Err(ModelError::LimitExceeded {
            resource: "Bundle assets",
            limit: limits.max_assets as u64,
        });
    }

    let mut indexed = BTreeMap::new();
    let mut previous_digest: Option<Sha256Digest> = None;
    let mut total_bytes = 0_u64;
    for asset in assets {
        let digest = Sha256Digest::parse(asset.sha256.clone())
            .map_err(|_| ModelError::InvalidAssetIndexEntry)?;
        let reference = AssetRef {
            id: digest.asset_id(),
            media_type: asset.media_type.clone(),
            uri: digest.asset_uri(),
            sha256: Some(asset.sha256.clone()),
        };
        Sha256Digest::from_asset_ref(&reference).map_err(|_| ModelError::InvalidAssetIndexEntry)?;

        if asset.path != BundleAsset::canonical_path(digest.as_str()) {
            return Err(ModelError::InvalidAssetPath);
        }
        if previous_digest
            .as_ref()
            .is_some_and(|previous| previous >= &digest)
        {
            return Err(ModelError::AssetIndexNotSorted);
        }
        previous_digest = Some(digest.clone());

        if asset.byte_length > limits.max_asset_bytes {
            return Err(ModelError::AssetSizeLimit);
        }
        total_bytes = total_bytes
            .checked_add(asset.byte_length)
            .ok_or(ModelError::AssetSizeLimit)?;
        if total_bytes > limits.max_total_asset_bytes {
            return Err(ModelError::AssetSizeLimit);
        }
        indexed.insert(digest, asset);
    }

    if indexed.len() != references.by_digest.len()
        || indexed
            .keys()
            .zip(references.by_digest.keys())
            .any(|(indexed, referenced)| indexed != referenced)
    {
        return Err(ModelError::AssetSetMismatch);
    }
    for (digest, reference) in &references.by_digest {
        let asset = indexed.get(digest).ok_or(ModelError::AssetSetMismatch)?;
        if asset.media_type != reference.media_type {
            return Err(ModelError::AssetMediaTypeMismatch);
        }
        if references
            .declared_byte_lengths
            .get(digest)
            .is_some_and(|expected| asset.byte_length != *expected)
        {
            return Err(ModelError::AssetByteLengthMismatch);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use devicerail_protocol::{
        ActionExecution, ActionOutcome, ActionResult, AssetRef, DeviceId, ErrorInfo, EventId,
        EventSequence, MediaFrame, MediaStreamId, MediaStreamInfo, MediaStreamKind, Observation,
        ProtocolVersion, RecordedActionCall, ScreenshotOmissionReason, SessionExport, SessionId,
        SessionInfo, SessionOutcome, SessionState, TestEvent, TestEventPayload,
        UI_SNAPSHOT_FORMAT_VERSION, UI_SNAPSHOT_MEDIA_TYPE, UiContextKind, UiContextRef,
        UiSnapshotRef, Verdict, VerdictStatus, Viewport,
    };
    use serde_json::{Map, Value, json};
    use uuid::Uuid;

    use super::{ModelError, validate_manifest_events, validate_source};
    use crate::model::{BundleAsset, BundleLimits, BundleManifest, BundleSource};

    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn event(
        session_id: &SessionId,
        sequence: u64,
        at_ms: u64,
        payload: TestEventPayload,
    ) -> TestEvent {
        TestEvent {
            event_id: EventId::from(Uuid::from_u128(sequence as u128)),
            session_id: session_id.clone(),
            sequence: EventSequence::new(sequence).expect("test sequence"),
            request_id: None,
            device_id: None,
            at_ms,
            payload,
        }
    }

    fn source(protocol: ProtocolVersion, middle: Vec<(u64, TestEventPayload)>) -> BundleSource {
        let session_id = SessionId::from(Uuid::from_u128(99));
        let mut events = vec![event(&session_id, 1, 100, TestEventPayload::SessionStarted)];
        for (index, (at_ms, payload)) in middle.into_iter().enumerate() {
            events.push(event(&session_id, index as u64 + 2, at_ms, payload));
        }
        let last = events.len() as u64 + 1;
        // Wall time is intentionally earlier than Session start. Replay order is
        // the sequence number; a host clock can move backwards.
        events.push(event(
            &session_id,
            last,
            80,
            TestEventPayload::SessionEnded {
                outcome: SessionOutcome::Completed,
                reason: None,
            },
        ));
        let last_sequence = EventSequence::new(last).expect("event count");
        BundleSource {
            event_protocol_version: protocol,
            session_export: SessionExport {
                session: SessionInfo {
                    id: session_id,
                    state: SessionState::Ended,
                    started_at_ms: 100,
                    ended_at_ms: Some(80),
                    event_count: last_sequence,
                    last_sequence,
                },
                events,
            },
        }
    }

    fn asset(media_type: &str) -> AssetRef {
        AssetRef {
            id: format!("sha256:{DIGEST}"),
            media_type: media_type.to_owned(),
            uri: format!("devicerail://assets/sha256/{DIGEST}"),
            sha256: Some(DIGEST.to_owned()),
        }
    }

    fn observation(
        screenshot: Option<AssetRef>,
        omission: Option<ScreenshotOmissionReason>,
    ) -> Observation {
        Observation {
            id: Uuid::from_u128(7),
            device_id: DeviceId::new("mock-1"),
            captured_at_ms: 7,
            viewport: Viewport {
                width: 1,
                height: 1,
                scale_factor: 1.0,
            },
            screenshot,
            screenshot_omission: omission,
            ui_snapshot: None,
            ui_snapshot_omission: None,
            metadata: Map::new(),
        }
    }

    fn ui_snapshot_ref() -> UiSnapshotRef {
        UiSnapshotRef {
            format_version: UI_SNAPSHOT_FORMAT_VERSION,
            context: UiContextRef {
                context_kind: UiContextKind::Native,
                context_id: "NATIVE_APP".to_owned(),
                document_epoch: "wda-session-1".to_owned(),
            },
            node_count: 1,
            byte_length: 3,
            evidence: asset(UI_SNAPSHOT_MEDIA_TYPE),
        }
    }

    #[test]
    fn source_is_strict_and_ended_session_uses_sequence_not_wall_clock_order() {
        let source = source(ProtocolVersion::new(1, 0), Vec::new());
        assert!(validate_source(&source, &BundleLimits::default()).is_ok());

        let mut value = serde_json::to_value(source).expect("source JSON");
        value
            .as_object_mut()
            .expect("object")
            .insert("unknown".to_owned(), Value::Bool(true));
        assert!(serde_json::from_value::<BundleSource>(value).is_err());
    }

    #[test]
    fn standard_null_arguments_are_valid() {
        let call_id = Uuid::from_u128(4);
        let source = source(
            ProtocolVersion::new(1, 0),
            vec![
                (
                    300,
                    TestEventPayload::ActionStarted {
                        call: RecordedActionCall {
                            id: call_id,
                            name: "noop".to_owned(),
                            arguments: Value::Null,
                            arguments_redacted: false,
                        },
                    },
                ),
                (
                    20,
                    TestEventPayload::ActionCompleted {
                        call_id,
                        outcome: ActionOutcome::Succeeded {
                            result: Box::new(ActionResult {
                                call_id,
                                started_at_ms: 10,
                                finished_at_ms: 11,
                                output: Value::Null,
                                before: None,
                                after: None,
                                evidence: Vec::new(),
                                execution: None,
                            }),
                        },
                    },
                ),
            ],
        );
        assert!(validate_source(&source, &BundleLimits::default()).is_ok());
    }

    #[test]
    fn media_frames_require_protocol_14_and_form_a_closed_evidence_stream() {
        let stream_id = MediaStreamId::from(Uuid::from_u128(88));
        let events = vec![
            (
                110,
                TestEventPayload::MediaStreamStarted {
                    stream: MediaStreamInfo {
                        id: stream_id.clone(),
                        kind: MediaStreamKind::Video,
                        media_type: "video/webm".to_owned(),
                        viewport: Some(Viewport {
                            width: 1280,
                            height: 720,
                            scale_factor: 1.0,
                        }),
                    },
                },
            ),
            (
                120,
                TestEventPayload::MediaFrameCaptured {
                    frame: MediaFrame {
                        stream_id: stream_id.clone(),
                        frame_index: EventSequence::FIRST,
                        key_frame: true,
                        duration_ms: Some(100),
                        evidence: asset("video/webm"),
                    },
                },
            ),
            (
                130,
                TestEventPayload::MediaStreamEnded {
                    stream_id,
                    frame_count: 1,
                },
            ),
        ];
        let source_14 = source(ProtocolVersion::new(1, 4), events.clone());
        let references =
            validate_source(&source_14, &BundleLimits::default()).expect("valid media stream");
        assert_eq!(references.len(), 1);

        assert!(matches!(
            validate_source(
                &source(ProtocolVersion::new(1, 3), events),
                &BundleLimits::default()
            ),
            Err(ModelError::FieldRequiresProtocol14 { .. })
        ));

        let mut bad_count = source_14;
        let TestEventPayload::MediaStreamEnded { frame_count, .. } =
            &mut bad_count.session_export.events[3].payload
        else {
            panic!("media stream terminal")
        };
        *frame_count = 2;
        assert_eq!(
            validate_source(&bad_count, &BundleLimits::default()),
            Err(ModelError::InvalidMediaStream)
        );
    }

    #[test]
    fn ui_snapshot_references_require_15_and_bind_the_manifest_byte_length() {
        let mut captured = observation(None, None);
        captured.ui_snapshot = Some(ui_snapshot_ref());
        let current = source(
            ProtocolVersion::new(1, 5),
            vec![(
                110,
                TestEventPayload::ObservationCaptured {
                    observation: Box::new(captured.clone()),
                },
            )],
        );
        let references =
            validate_source(&current, &BundleLimits::default()).expect("valid UI Snapshot");
        assert_eq!(references.len(), 1);

        assert!(matches!(
            validate_source(
                &source(
                    ProtocolVersion::new(1, 4),
                    vec![(
                        110,
                        TestEventPayload::ObservationCaptured {
                            observation: Box::new(captured),
                        },
                    )],
                ),
                &BundleLimits::default(),
            ),
            Err(ModelError::FieldRequiresProtocol15 { .. })
        ));

        let indexed = BundleAsset {
            sha256: DIGEST.to_owned(),
            media_type: UI_SNAPSHOT_MEDIA_TYPE.to_owned(),
            byte_length: 4,
            path: BundleAsset::canonical_path(DIGEST),
        };
        let manifest = BundleManifest::from_source(&current, vec![indexed]);
        assert_eq!(
            validate_manifest_events(&manifest, &BundleLimits::default()),
            Err(ModelError::AssetByteLengthMismatch)
        );
    }

    #[test]
    fn action_execution_requires_15_and_matches_its_context_kind() {
        let call_id = Uuid::from_u128(55);
        let make_source = |protocol, execution| {
            source(
                protocol,
                vec![
                    (
                        10,
                        TestEventPayload::ActionStarted {
                            call: RecordedActionCall {
                                id: call_id,
                                name: "findElement".to_owned(),
                                arguments: json!({ "selector": { "role": "button" } }),
                                arguments_redacted: false,
                            },
                        },
                    ),
                    (
                        20,
                        TestEventPayload::ActionCompleted {
                            call_id,
                            outcome: ActionOutcome::Succeeded {
                                result: Box::new(ActionResult {
                                    call_id,
                                    started_at_ms: 1,
                                    finished_at_ms: 2,
                                    output: json!({ "matched": true }),
                                    before: None,
                                    after: None,
                                    evidence: Vec::new(),
                                    execution: Some(execution),
                                }),
                            },
                        },
                    ),
                ],
            )
        };
        let native_context = UiContextRef {
            context_kind: UiContextKind::Native,
            context_id: "NATIVE_APP".to_owned(),
            document_epoch: "wda-session-1".to_owned(),
        };
        let valid_execution = ActionExecution::NativeSemantic {
            context: native_context.clone(),
        };
        assert!(
            validate_source(
                &make_source(ProtocolVersion::new(1, 5), valid_execution.clone()),
                &BundleLimits::default(),
            )
            .is_ok()
        );
        assert!(matches!(
            validate_source(
                &make_source(ProtocolVersion::new(1, 4), valid_execution),
                &BundleLimits::default(),
            ),
            Err(ModelError::FieldRequiresProtocol15 { .. })
        ));

        let invalid_execution = ActionExecution::WebSemantic {
            context: native_context,
        };
        assert_eq!(
            validate_source(
                &make_source(ProtocolVersion::new(1, 5), invalid_execution),
                &BundleLimits::default(),
            ),
            Err(ModelError::InvalidActionExecution)
        );
    }

    #[test]
    fn arbitrary_event_json_depth_is_bounded_before_serialization() {
        let call_id = Uuid::from_u128(14);
        let nested = json!([[[null]]]);
        let source = source(
            ProtocolVersion::new(1, 2),
            vec![
                (
                    10,
                    TestEventPayload::ActionStarted {
                        call: RecordedActionCall {
                            id: call_id,
                            name: "noop".to_owned(),
                            arguments: nested,
                            arguments_redacted: false,
                        },
                    },
                ),
                (
                    20,
                    TestEventPayload::ActionCompleted {
                        call_id,
                        outcome: ActionOutcome::Failed {
                            error: ErrorInfo {
                                code: "expected".to_owned(),
                                message: "expected".to_owned(),
                                retryable: false,
                                details: None,
                            },
                        },
                    },
                ),
            ],
        );
        let limits = BundleLimits {
            max_json_depth: 3,
            ..BundleLimits::default()
        };
        assert!(matches!(
            validate_source(&source, &limits),
            Err(ModelError::JsonDepthLimit)
        ));
    }

    #[test]
    fn event_identity_lifecycle_and_action_correlation_fail_closed() {
        let call_id = Uuid::from_u128(44);
        let valid = source(
            ProtocolVersion::new(1, 2),
            vec![
                (
                    10,
                    TestEventPayload::ActionStarted {
                        call: RecordedActionCall {
                            id: call_id,
                            name: "noop".to_owned(),
                            arguments: Value::Null,
                            arguments_redacted: false,
                        },
                    },
                ),
                (
                    20,
                    TestEventPayload::ActionCompleted {
                        call_id,
                        outcome: ActionOutcome::Succeeded {
                            result: Box::new(ActionResult {
                                call_id,
                                started_at_ms: 1,
                                finished_at_ms: 2,
                                output: Value::Null,
                                before: None,
                                after: None,
                                evidence: Vec::new(),
                                execution: None,
                            }),
                        },
                    },
                ),
            ],
        );

        let mut duplicate_event = valid.clone();
        duplicate_event.session_export.events[2].event_id =
            duplicate_event.session_export.events[1].event_id.clone();
        assert!(matches!(
            validate_source(&duplicate_event, &BundleLimits::default()),
            Err(ModelError::DuplicateEventId)
        ));

        let mut changed_correlation = valid.clone();
        changed_correlation.session_export.events[1].device_id = Some(DeviceId::new("mock-1"));
        changed_correlation.session_export.events[2].device_id = Some(DeviceId::new("mock-2"));
        assert!(matches!(
            validate_source(&changed_correlation, &BundleLimits::default()),
            Err(ModelError::ActionCorrelationMismatch(id)) if id == call_id
        ));

        let mut open_action = valid.clone();
        open_action.session_export.events[2].payload = TestEventPayload::Error {
            error: ErrorInfo {
                code: "test".to_owned(),
                message: "test".to_owned(),
                retryable: false,
                details: None,
            },
        };
        assert!(matches!(
            validate_source(&open_action, &BundleLimits::default()),
            Err(ModelError::ActionStillInFlight)
        ));

        let mut bad_lifecycle = valid;
        bad_lifecycle.session_export.events[0].payload = TestEventPayload::Error {
            error: ErrorInfo {
                code: "test".to_owned(),
                message: "test".to_owned(),
                retryable: false,
                details: None,
            },
        };
        assert!(matches!(
            validate_source(&bad_lifecycle, &BundleLimits::default()),
            Err(ModelError::InvalidLifecycle)
        ));
    }

    #[test]
    fn protected_success_requires_v12_and_exact_omission_contract() {
        let call_id = Uuid::from_u128(5);
        let protected_observation =
            observation(None, Some(ScreenshotOmissionReason::ProtectedAction));
        let make_source = |protocol| {
            source(
                protocol,
                vec![
                    (
                        10,
                        TestEventPayload::ActionStarted {
                            call: RecordedActionCall {
                                id: call_id,
                                name: "inputSecret".to_owned(),
                                arguments: Value::Null,
                                arguments_redacted: true,
                            },
                        },
                    ),
                    (
                        20,
                        TestEventPayload::ActionCompleted {
                            call_id,
                            outcome: ActionOutcome::Succeeded {
                                result: Box::new(ActionResult {
                                    call_id,
                                    started_at_ms: 1,
                                    finished_at_ms: 2,
                                    output: json!({ "redacted": true }),
                                    before: Some(protected_observation.clone()),
                                    after: Some(protected_observation.clone()),
                                    evidence: Vec::new(),
                                    execution: None,
                                }),
                            },
                        },
                    ),
                ],
            )
        };

        assert!(
            validate_source(
                &make_source(ProtocolVersion::new(1, 2)),
                &BundleLimits::default()
            )
            .is_ok()
        );
        assert!(
            validate_source(
                &make_source(ProtocolVersion::new(1, 5)),
                &BundleLimits::default()
            )
            .is_ok(),
            "a legacy Driver may make no UI Snapshot claim in a 1.5 Session"
        );
        assert!(matches!(
            validate_source(
                &make_source(ProtocolVersion::new(1, 1)),
                &BundleLimits::default()
            ),
            Err(ModelError::FieldRequiresProtocol12 { .. })
        ));

        let mut invalid = make_source(ProtocolVersion::new(1, 2));
        let TestEventPayload::ActionCompleted { outcome, .. } =
            &mut invalid.session_export.events[2].payload
        else {
            panic!("completion")
        };
        let ActionOutcome::Succeeded { result } = outcome else {
            panic!("success")
        };
        result.evidence.push(asset("image/png"));
        assert!(matches!(
            validate_source(&invalid, &BundleLimits::default()),
            Err(ModelError::InvalidProtectedActionOmission)
        ));
    }

    #[test]
    fn only_typed_references_are_collected_and_occurrences_are_limited() {
        let reference = asset("image/png");
        let mut captured = observation(Some(reference.clone()), None);
        captured.metadata.insert(
            "untypedIgnored".to_owned(),
            serde_json::to_value(asset("application/json")).expect("metadata"),
        );
        let source = source(
            ProtocolVersion::new(1, 2),
            vec![
                (
                    10,
                    TestEventPayload::ObservationCaptured {
                        observation: Box::new(captured),
                    },
                ),
                (
                    20,
                    TestEventPayload::VerdictRecorded {
                        verdict: Verdict {
                            status: VerdictStatus::Pass,
                            summary: "ok".to_owned(),
                            evidence: vec![reference],
                        },
                    },
                ),
            ],
        );

        let references =
            validate_source(&source, &BundleLimits::default()).expect("valid references");
        assert_eq!(references.len(), 1);

        let limits = BundleLimits {
            max_typed_references: 1,
            ..BundleLimits::default()
        };
        assert!(matches!(
            validate_source(&source, &limits),
            Err(ModelError::LimitExceeded {
                resource: "typed Evidence references",
                ..
            })
        ));
    }

    #[test]
    fn conflicting_media_and_observation_conflicts_fail_closed() {
        let conflicting = source(
            ProtocolVersion::new(1, 2),
            vec![
                (
                    10,
                    TestEventPayload::ObservationCaptured {
                        observation: Box::new(observation(Some(asset("image/png")), None)),
                    },
                ),
                (
                    20,
                    TestEventPayload::VerdictRecorded {
                        verdict: Verdict {
                            status: VerdictStatus::Pass,
                            summary: "ok".to_owned(),
                            evidence: vec![asset("application/json")],
                        },
                    },
                ),
            ],
        );
        assert!(matches!(
            validate_source(&conflicting, &BundleLimits::default()),
            Err(ModelError::ConflictingEvidenceMediaType(_))
        ));

        let both = source(
            ProtocolVersion::new(1, 2),
            vec![(
                10,
                TestEventPayload::ObservationCaptured {
                    observation: Box::new(observation(
                        Some(asset("image/png")),
                        Some(ScreenshotOmissionReason::Policy),
                    )),
                },
            )],
        );
        assert!(matches!(
            validate_source(&both, &BundleLimits::default()),
            Err(ModelError::ObservationScreenshotConflict)
        ));

        let protected_capture = source(
            ProtocolVersion::new(1, 2),
            vec![(
                10,
                TestEventPayload::ObservationCaptured {
                    observation: Box::new(observation(
                        None,
                        Some(ScreenshotOmissionReason::ProtectedAction),
                    )),
                },
            )],
        );
        assert!(matches!(
            validate_source(&protected_capture, &BundleLimits::default()),
            Err(ModelError::UnexpectedProtectedActionOmission)
        ));
    }

    #[test]
    fn manifest_index_must_be_exact_sorted_and_canonical() {
        let source = source(
            ProtocolVersion::new(1, 2),
            vec![(
                10,
                TestEventPayload::ObservationCaptured {
                    observation: Box::new(observation(Some(asset("image/png")), None)),
                },
            )],
        );
        let indexed_asset = BundleAsset {
            sha256: DIGEST.to_owned(),
            media_type: "image/png".to_owned(),
            byte_length: 3,
            path: BundleAsset::canonical_path(DIGEST),
        };
        let manifest = BundleManifest::from_source(&source, vec![indexed_asset.clone()]);
        assert!(validate_manifest_events(&manifest, &BundleLimits::default()).is_ok());

        let mut missing = manifest.clone();
        missing.assets.clear();
        assert!(matches!(
            validate_manifest_events(&missing, &BundleLimits::default()),
            Err(ModelError::AssetSetMismatch)
        ));

        let mut wrong_path = manifest;
        wrong_path.assets[0].path = "../escape".to_owned();
        assert!(matches!(
            validate_manifest_events(&wrong_path, &BundleLimits::default()),
            Err(ModelError::InvalidAssetPath)
        ));
    }
}
