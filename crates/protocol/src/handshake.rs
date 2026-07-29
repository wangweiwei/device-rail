use std::{collections::BTreeSet, fmt};

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

pub mod feature {
    /// Enables per-connection device discovery and default-device selection.
    pub const DEVICE_ROUTING_V1: &str = "device.routing.v1";

    /// Enables the v1 event snapshot management methods (`events.list` and `events.clear`).
    pub const EVENTS_SNAPSHOT_V1: &str = "events.snapshot.v1";

    /// Enables bounded, cursor-resumable results from `session.export`.
    pub const SESSION_EXPORT_PAGE_V1: &str = "session.export.page.v1";

    /// Enables request deadlines and cooperative cancellation with `request.cancel`.
    pub const REQUEST_CONTROL_V1: &str = "request.control.v1";

    /// Enables discovery and execution of actions whose durable arguments are redacted.
    pub const ACTION_PROTECTED_V1: &str = "action.protected.v1";

    /// Enables the resumable real-time Session event stream transport.
    pub const EVENTS_STREAM_V1: &str = "events.stream.v1";

    /// Enables Evidence-referenced screenshot/video stream lifecycle events.
    pub const MEDIA_STREAM_V1: &str = "media.stream.v1";

    /// Enables Evidence-referenced canonical UI Trees and `ui.snapshot.get`.
    pub const OBSERVATION_UI_SNAPSHOT_V1: &str = "observation.uiSnapshot.v1";

    /// Enables selectors, stable node references, and semantic action execution metadata.
    pub const DEVICE_SEMANTIC_ACTIONS_V1: &str = "device.semanticActions.v1";

    /// Enables persisting an existing Verdict through `verdict.record`.
    pub const VERDICT_RECORD_V1: &str = "verdict.record.v1";
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

impl ProtocolVersion {
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

/// A contiguous minor-version range within one protocol major version.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolRange {
    pub major: u16,
    pub min_minor: u16,
    pub max_minor: u16,
}

impl ProtocolRange {
    pub const fn new(major: u16, min_minor: u16, max_minor: u16) -> Self {
        Self {
            major,
            min_minor,
            max_minor,
        }
    }

    pub const fn exact(version: ProtocolVersion) -> Self {
        Self::new(version.major, version.minor, version.minor)
    }

    pub const fn is_valid(self) -> bool {
        self.min_minor <= self.max_minor
    }

    pub const fn minimum(self) -> ProtocolVersion {
        ProtocolVersion::new(self.major, self.min_minor)
    }

    pub const fn maximum(self) -> ProtocolVersion {
        ProtocolVersion::new(self.major, self.max_minor)
    }
}

/// An explicit set of supported ranges. Multiple entries avoid implying support
/// for major versions that a peer intentionally skipped.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolOffer {
    #[cfg_attr(feature = "schema", schemars(length(min = 1)))]
    pub ranges: Vec<ProtocolRange>,
}

impl ProtocolOffer {
    pub fn new(ranges: Vec<ProtocolRange>) -> Self {
        Self { ranges }
    }

    pub fn exact(version: ProtocolVersion) -> Self {
        Self::new(vec![ProtocolRange::exact(version)])
    }

    pub fn minimum(&self) -> Option<ProtocolVersion> {
        self.ranges.iter().map(|range| range.minimum()).min()
    }

    pub fn maximum(&self) -> Option<ProtocolVersion> {
        self.ranges.iter().map(|range| range.maximum()).max()
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProtocolIncompatibilityReason {
    ClientTooOld,
    ServerTooOld,
    NoCommonVersion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolNegotiationError {
    EmptyClientOffer,
    EmptyServerOffer,
    InvalidClientRange,
    InvalidServerRange,
    Incompatible(ProtocolIncompatibilityReason),
}

/// Selects the lexicographically newest version present in both offers.
pub fn negotiate_protocol(
    client: &ProtocolOffer,
    server: &ProtocolOffer,
) -> Result<ProtocolVersion, ProtocolNegotiationError> {
    validate_offer(client, true)?;
    validate_offer(server, false)?;

    let mut selected: Option<ProtocolVersion> = None;
    for client_range in &client.ranges {
        for server_range in &server.ranges {
            if client_range.major != server_range.major {
                continue;
            }

            let min_minor = client_range.min_minor.max(server_range.min_minor);
            let max_minor = client_range.max_minor.min(server_range.max_minor);
            if min_minor <= max_minor {
                let candidate = ProtocolVersion::new(client_range.major, max_minor);
                selected = Some(selected.map_or(candidate, |current| current.max(candidate)));
            }
        }
    }

    selected.ok_or_else(|| {
        ProtocolNegotiationError::Incompatible(incompatibility_reason(client, server))
    })
}

fn validate_offer(offer: &ProtocolOffer, is_client: bool) -> Result<(), ProtocolNegotiationError> {
    if offer.ranges.is_empty() {
        return Err(if is_client {
            ProtocolNegotiationError::EmptyClientOffer
        } else {
            ProtocolNegotiationError::EmptyServerOffer
        });
    }

    if offer.ranges.iter().any(|range| !range.is_valid()) {
        return Err(if is_client {
            ProtocolNegotiationError::InvalidClientRange
        } else {
            ProtocolNegotiationError::InvalidServerRange
        });
    }

    Ok(())
}

fn incompatibility_reason(
    client: &ProtocolOffer,
    server: &ProtocolOffer,
) -> ProtocolIncompatibilityReason {
    match (
        client.maximum(),
        server.minimum(),
        client.minimum(),
        server.maximum(),
    ) {
        (Some(client_max), Some(server_min), _, _) if client_max < server_min => {
            ProtocolIncompatibilityReason::ClientTooOld
        }
        (_, _, Some(client_min), Some(server_max)) if client_min > server_max => {
            ProtocolIncompatibilityReason::ServerTooOld
        }
        _ => ProtocolIncompatibilityReason::NoCommonVersion,
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureOffer {
    #[serde(default, deserialize_with = "deserialize_unique_string_set")]
    pub required: BTreeSet<String>,
    #[serde(default, deserialize_with = "deserialize_unique_string_set")]
    pub optional: BTreeSet<String>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureSelection {
    #[serde(deserialize_with = "deserialize_unique_string_set")]
    pub enabled: BTreeSet<String>,
}

fn deserialize_unique_string_set<'de, D>(deserializer: D) -> Result<BTreeSet<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    let mut unique = BTreeSet::new();
    for value in values {
        if !unique.insert(value.clone()) {
            return Err(serde::de::Error::custom(format!(
                "duplicate feature name: {value}"
            )));
        }
    }
    Ok(unique)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureNegotiationError {
    pub unsupported_required: BTreeSet<String>,
}

pub fn negotiate_features(
    client: &FeatureOffer,
    available: &BTreeSet<String>,
) -> Result<FeatureSelection, FeatureNegotiationError> {
    let unsupported_required = client
        .required
        .difference(available)
        .cloned()
        .collect::<BTreeSet<_>>();
    if !unsupported_required.is_empty() {
        return Err(FeatureNegotiationError {
            unsupported_required,
        });
    }

    let enabled = client
        .required
        .union(&client.optional)
        .filter(|feature| available.contains(*feature))
        .cloned()
        .collect();
    Ok(FeatureSelection { enabled })
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerInfo {
    pub name: String,
    pub version: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelloParams {
    pub client: PeerInfo,
    pub protocol: ProtocolOffer,
    #[serde(default)]
    pub features: FeatureOffer,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolSelection {
    pub selected: ProtocolVersion,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransportInfo {
    pub kind: String,
    pub framing: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelloResult {
    pub connection_id: Uuid,
    pub protocol: ProtocolSelection,
    pub server: PeerInfo,
    pub transport: TransportInfo,
    pub features: FeatureSelection,
}

/// Highest protocol version implemented by this crate.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::new(1, 5);

pub fn supported_protocol_offer() -> ProtocolOffer {
    ProtocolOffer::new(vec![ProtocolRange::new(1, 0, PROTOCOL_VERSION.minor)])
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::{
        FeatureOffer, HelloParams, ProtocolIncompatibilityReason, ProtocolNegotiationError,
        ProtocolOffer, ProtocolRange, ProtocolVersion, feature, negotiate_features,
        negotiate_protocol, supported_protocol_offer,
    };

    #[test]
    fn chooses_newest_version_across_unsorted_multi_major_offers() {
        let client = ProtocolOffer::new(vec![
            ProtocolRange::new(1, 0, 8),
            ProtocolRange::new(3, 0, 2),
        ]);
        let server = ProtocolOffer::new(vec![
            ProtocolRange::new(3, 1, 4),
            ProtocolRange::new(1, 2, 9),
        ]);

        assert_eq!(
            negotiate_protocol(&client, &server),
            Ok(ProtocolVersion::new(3, 2))
        );
    }

    #[test]
    fn offer_does_not_imply_support_for_a_missing_major() {
        let client = ProtocolOffer::new(vec![
            ProtocolRange::new(1, 0, 4),
            ProtocolRange::new(3, 0, 4),
        ]);
        let server = ProtocolOffer::new(vec![ProtocolRange::new(2, 0, 4)]);

        assert_eq!(
            negotiate_protocol(&client, &server),
            Err(ProtocolNegotiationError::Incompatible(
                ProtocolIncompatibilityReason::NoCommonVersion
            ))
        );
    }

    #[test]
    fn distinguishes_clients_that_are_too_old_or_too_new() {
        let server = ProtocolOffer::exact(ProtocolVersion::new(2, 0));
        let old_client = ProtocolOffer::new(vec![ProtocolRange::new(1, 0, 9)]);
        let new_client = ProtocolOffer::new(vec![ProtocolRange::new(3, 0, 9)]);

        assert_eq!(
            negotiate_protocol(&old_client, &server),
            Err(ProtocolNegotiationError::Incompatible(
                ProtocolIncompatibilityReason::ClientTooOld
            ))
        );
        assert_eq!(
            negotiate_protocol(&new_client, &server),
            Err(ProtocolNegotiationError::Incompatible(
                ProtocolIncompatibilityReason::ServerTooOld
            ))
        );
    }

    #[test]
    fn validates_empty_and_inverted_offers() {
        let server = ProtocolOffer::exact(ProtocolVersion::new(1, 0));
        assert_eq!(
            negotiate_protocol(&ProtocolOffer::new(vec![]), &server),
            Err(ProtocolNegotiationError::EmptyClientOffer)
        );
        assert_eq!(
            negotiate_protocol(
                &ProtocolOffer::new(vec![ProtocolRange::new(1, 2, 1)]),
                &server
            ),
            Err(ProtocolNegotiationError::InvalidClientRange)
        );
    }

    #[test]
    fn v1_offer_preserves_minor_zero_and_advertises_current_features() {
        assert_eq!(
            supported_protocol_offer(),
            ProtocolOffer::new(vec![ProtocolRange::new(1, 0, 5)])
        );
        assert_eq!(feature::REQUEST_CONTROL_V1, "request.control.v1");
        assert_eq!(feature::DEVICE_ROUTING_V1, "device.routing.v1");
        assert_eq!(feature::ACTION_PROTECTED_V1, "action.protected.v1");
        assert_eq!(feature::SESSION_EXPORT_PAGE_V1, "session.export.page.v1");
        assert_eq!(
            feature::OBSERVATION_UI_SNAPSHOT_V1,
            "observation.uiSnapshot.v1"
        );
        assert_eq!(
            feature::DEVICE_SEMANTIC_ACTIONS_V1,
            "device.semanticActions.v1"
        );
        assert_eq!(feature::VERDICT_RECORD_V1, "verdict.record.v1");
    }

    #[test]
    fn feature_negotiation_requires_required_and_ignores_unknown_optional() {
        let available = BTreeSet::from([feature::EVENTS_SNAPSHOT_V1.to_owned()]);
        let offer = FeatureOffer {
            required: BTreeSet::from([feature::EVENTS_SNAPSHOT_V1.to_owned()]),
            optional: BTreeSet::from(["events.push.v1".to_owned()]),
        };
        let selected = negotiate_features(&offer, &available).expect("features are compatible");
        assert_eq!(selected.enabled, available);

        let unsupported = FeatureOffer {
            required: BTreeSet::from(["z.v1".to_owned(), "a.v1".to_owned()]),
            optional: BTreeSet::new(),
        };
        let error = negotiate_features(&unsupported, &available).expect_err("required are absent");
        assert_eq!(
            error.unsupported_required.into_iter().collect::<Vec<_>>(),
            ["a.v1", "z.v1"]
        );
    }

    #[test]
    fn offer_uses_camel_case_wire_fields() {
        let value = serde_json::to_value(ProtocolOffer::new(vec![ProtocolRange::new(1, 0, 2)]))
            .expect("serialize offer");
        assert_eq!(
            value,
            json!({ "ranges": [{ "major": 1, "minMinor": 0, "maxMinor": 2 }] })
        );
    }

    #[test]
    fn hello_rejects_unknown_or_misspelled_fields() {
        let misspelled = json!({
            "client": { "name": "client", "version": "0.1.0" },
            "protocol": {
                "ranges": [{ "major": 1, "minMinor": 0, "maxMinor": 0 }]
            },
            "feature": { "required": [], "optional": [] }
        });

        assert!(serde_json::from_value::<HelloParams>(misspelled).is_err());
    }

    #[test]
    fn feature_sets_reject_duplicate_wire_values() {
        let duplicated = json!({
            "client": { "name": "client", "version": "0.1.0" },
            "protocol": {
                "ranges": [{ "major": 1, "minMinor": 0, "maxMinor": 0 }]
            },
            "features": {
                "required": ["events.snapshot.v1", "events.snapshot.v1"],
                "optional": []
            }
        });

        assert!(serde_json::from_value::<HelloParams>(duplicated).is_err());
    }
}
