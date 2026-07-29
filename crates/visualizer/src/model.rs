use devicerail_session_bundle::BundleLimits;

use devicerail_protocol::{TestEvent, TestEventPayload};

use crate::VisualizerError;

pub const MAX_EVENTS_PER_PAGE: usize = 50;
const MAX_TEXT_CHAR_LIMIT: usize = 16 * 1024;
const MAX_JSON_BYTE_LIMIT: usize = 256 * 1024;
const MAX_HTML_BYTE_LIMIT: usize = 2 * 1024 * 1024;

/// Independent ceilings for Bundle loading and presentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualizerLimits {
    pub bundle: BundleLimits,
    /// Fixed server-side page size; must be within `1..=50`.
    pub events_per_page: usize,
    /// Unicode scalar values retained from any one attacker-controlled label.
    pub max_text_chars: usize,
    /// Bytes retained while streaming any one arbitrary JSON value.
    pub max_json_bytes: usize,
    /// Absolute bytes retained for one complete HTML document.
    pub max_html_bytes: usize,
}

impl Default for VisualizerLimits {
    fn default() -> Self {
        Self {
            bundle: BundleLimits::default(),
            events_per_page: MAX_EVENTS_PER_PAGE,
            max_text_chars: 512,
            max_json_bytes: 16 * 1024,
            max_html_bytes: MAX_HTML_BYTE_LIMIT,
        }
    }
}

impl VisualizerLimits {
    pub(crate) fn validate(&self) -> Result<(), VisualizerError> {
        if !(1..=MAX_EVENTS_PER_PAGE).contains(&self.events_per_page) {
            return Err(VisualizerError::InvalidLimits(
                "events_per_page must be within 1..=50",
            ));
        }
        if !(1..=MAX_TEXT_CHAR_LIMIT).contains(&self.max_text_chars) {
            return Err(VisualizerError::InvalidLimits(
                "max_text_chars must be within 1..=16384",
            ));
        }
        if !(1..=MAX_JSON_BYTE_LIMIT).contains(&self.max_json_bytes) {
            return Err(VisualizerError::InvalidLimits(
                "max_json_bytes must be within 1..=262144",
            ));
        }
        if !(1..=MAX_HTML_BYTE_LIMIT).contains(&self.max_html_bytes) {
            return Err(VisualizerError::InvalidLimits(
                "max_html_bytes must be within 1..=2097152",
            ));
        }
        Ok(())
    }
}

/// Server-side Timeline filters. They never change replay ordering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PageKind {
    #[default]
    All,
    Observations,
    Actions,
    Errors,
    Verdicts,
}

impl PageKind {
    pub(crate) const COUNT: usize = 5;

    const fn index(self) -> usize {
        match self {
            Self::All => 0,
            Self::Observations => 1,
            Self::Actions => 2,
            Self::Errors => 3,
            Self::Verdicts => 4,
        }
    }

    pub const fn slug(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Observations => "observations",
            Self::Actions => "actions",
            Self::Errors => "errors",
            Self::Verdicts => "verdicts",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All events",
            Self::Observations => "Observations",
            Self::Actions => "Actions",
            Self::Errors => "Errors",
            Self::Verdicts => "Verdicts",
        }
    }
}

/// Immutable event offsets, built once after Bundle validation.
///
/// Every event is present in `All` and in at most one specialized filter, so
/// the index retains at most two offsets per event while preserving the
/// validator-confirmed replay order.
#[derive(Debug)]
pub(crate) struct TimelineIndex {
    by_kind: [Vec<usize>; PageKind::COUNT],
}

impl TimelineIndex {
    pub(crate) fn build(events: &[TestEvent]) -> Self {
        let mut by_kind: [Vec<usize>; PageKind::COUNT] = std::array::from_fn(|_| Vec::new());
        by_kind[PageKind::All.index()].reserve(events.len());

        for (event_index, event) in events.iter().enumerate() {
            by_kind[PageKind::All.index()].push(event_index);
            let specialized_kind = match event.payload {
                TestEventPayload::ObservationCaptured { .. }
                | TestEventPayload::MediaFrameCaptured { .. } => Some(PageKind::Observations),
                TestEventPayload::ActionStarted { .. }
                | TestEventPayload::ActionCompleted { .. } => Some(PageKind::Actions),
                TestEventPayload::Error { .. } => Some(PageKind::Errors),
                TestEventPayload::VerdictRecorded { .. } => Some(PageKind::Verdicts),
                _ => None,
            };
            if let Some(kind) = specialized_kind {
                by_kind[kind.index()].push(event_index);
            }
        }

        Self { by_kind }
    }

    pub(crate) fn event_indices(&self, kind: PageKind) -> &[usize] {
        &self.by_kind[kind.index()]
    }

    pub(crate) fn page_count(&self, kind: PageKind, events_per_page: usize) -> usize {
        self.event_indices(kind)
            .len()
            .div_ceil(events_per_page)
            .max(1)
    }
}

/// A one-based page request with a typed filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageQuery {
    pub kind: PageKind,
    pub page: usize,
}

impl PageQuery {
    pub const fn new(kind: PageKind, page: usize) -> Self {
        Self { kind, page }
    }
}

impl Default for PageQuery {
    fn default() -> Self {
        Self {
            kind: PageKind::All,
            page: 1,
        }
    }
}
