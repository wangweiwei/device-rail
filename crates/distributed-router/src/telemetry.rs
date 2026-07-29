use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Mutex,
};

use uuid::Uuid;

use crate::NodeId;

const MAX_TELEMETRY_NODES: usize = 64;
const MAX_TRACE_RECORDS: usize = 1_024;
const OVERFLOW_NODE: &str = "overflow";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationMethod {
    Hello,
    Inventory,
    Health,
    Capabilities,
    LeaseAcquire,
    LeaseRenew,
    LeaseRelease,
    Connect,
    Disconnect,
    Observe,
    Execute,
    EvidenceRead,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OperationOutcome {
    Success,
    RemoteError,
    ProtocolError,
    TransportError,
    Cancelled,
    TimedOut,
    OutcomeUnknown,
}

/// Bounded tracing data. Device ids, action names, arguments, evidence URIs,
/// credentials, and raw errors are deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelemetryRecord {
    pub trace_id: Uuid,
    pub node: String,
    pub method: OperationMethod,
    pub outcome: OperationOutcome,
    pub duration_ms: u64,
}

pub trait TelemetrySink: Send + Sync {
    fn record(&self, record: TelemetryRecord);
}

#[derive(Default)]
struct State {
    known_nodes: BTreeSet<String>,
    counts: BTreeMap<(String, OperationMethod, OperationOutcome), u64>,
    records: VecDeque<TelemetryRecord>,
}

#[derive(Default)]
pub struct MemoryTelemetry {
    state: Mutex<State>,
}

impl std::fmt::Debug for MemoryTelemetry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        formatter
            .debug_struct("MemoryTelemetry")
            .field("node_count", &state.known_nodes.len())
            .field("metric_series", &state.counts.len())
            .field("trace_records", &state.records.len())
            .finish()
    }
}

impl MemoryTelemetry {
    pub fn records(&self) -> Vec<TelemetryRecord> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .iter()
            .cloned()
            .collect()
    }

    pub fn count(&self, node: &str, method: OperationMethod, outcome: OperationOutcome) -> u64 {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counts
            .get(&(node.to_owned(), method, outcome))
            .unwrap_or(&0)
    }

    pub fn series_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counts
            .len()
    }
}

impl TelemetrySink for MemoryTelemetry {
    fn record(&self, mut record: TelemetryRecord) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.known_nodes.contains(&record.node) {
            if state.known_nodes.len() < MAX_TELEMETRY_NODES {
                state.known_nodes.insert(record.node.clone());
            } else {
                record.node = OVERFLOW_NODE.to_owned();
                state.known_nodes.insert(record.node.clone());
            }
        }
        *state
            .counts
            .entry((record.node.clone(), record.method, record.outcome))
            .or_default() += 1;
        if state.records.len() == MAX_TRACE_RECORDS {
            state.records.pop_front();
        }
        state.records.push_back(record);
    }
}

pub(crate) fn record(
    sink: &dyn TelemetrySink,
    trace_id: Uuid,
    node: &NodeId,
    method: OperationMethod,
    outcome: OperationOutcome,
    started: std::time::Instant,
) {
    sink.record(TelemetryRecord {
        trace_id,
        node: node.as_str().to_owned(),
        method,
        outcome,
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
    });
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_TELEMETRY_NODES, MemoryTelemetry, OperationMethod, OperationOutcome, TelemetryRecord,
        TelemetrySink,
    };

    #[test]
    fn metric_cardinality_is_bounded_and_records_have_no_secret_field() {
        let sink = MemoryTelemetry::default();
        for index in 0..(MAX_TELEMETRY_NODES + 20) {
            sink.record(TelemetryRecord {
                trace_id: uuid::Uuid::new_v4(),
                node: format!("node-{index}"),
                method: OperationMethod::Execute,
                outcome: OperationOutcome::Success,
                duration_ms: 1,
            });
        }
        assert!(sink.series_count() <= MAX_TELEMETRY_NODES + 1);
        let rendered = format!("{:?}", sink.records());
        assert!(!rendered.contains("argument"));
        assert!(!rendered.contains("deviceId"));
    }
}
