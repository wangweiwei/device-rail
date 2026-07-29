use devicerail_protocol::{EventSequence, MAX_SAFE_INTEGER};

use crate::WatermarkError;

/// Converts an application watermark with a zero initial sentinel into an
/// exclusive event cursor.
///
/// DeviceRail event sequences are one-based, so a watermark of `0` means that
/// the caller has not consumed any events and must omit `afterSequence` to read
/// from the beginning. Values from `1` through [`MAX_SAFE_INTEGER`] become an
/// [`EventSequence`] suitable for `EventsListParams::after_sequence` or
/// `SessionExportParams::after_sequence`. Larger values fail explicitly rather
/// than being mistaken for the beginning of the stream.
///
/// Pagination remains caller-driven; this helper performs only the safe cursor
/// conversion and never fetches or aggregates pages.
pub fn after_sequence_from_watermark(
    watermark: u64,
) -> Result<Option<EventSequence>, WatermarkError> {
    if watermark == 0 {
        return Ok(None);
    }

    EventSequence::new(watermark)
        .map(Some)
        .ok_or(WatermarkError::ExceedsSafeInteger {
            watermark,
            max: MAX_SAFE_INTEGER,
        })
}

#[cfg(test)]
mod tests {
    use super::after_sequence_from_watermark;
    use crate::{WatermarkError, protocol::MAX_SAFE_INTEGER};

    #[test]
    fn zero_watermark_omits_the_exclusive_cursor() {
        assert_eq!(after_sequence_from_watermark(0), Ok(None));
    }

    #[test]
    fn positive_safe_watermarks_become_event_sequences() {
        let first = after_sequence_from_watermark(1)
            .expect("first watermark")
            .expect("event sequence");
        assert_eq!(first.get(), 1);

        let maximum = after_sequence_from_watermark(MAX_SAFE_INTEGER)
            .expect("maximum safe watermark")
            .expect("event sequence");
        assert_eq!(maximum.get(), MAX_SAFE_INTEGER);
    }

    #[test]
    fn unsafe_watermark_is_rejected_instead_of_omitted() {
        assert_eq!(
            after_sequence_from_watermark(MAX_SAFE_INTEGER + 1),
            Err(WatermarkError::ExceedsSafeInteger {
                watermark: MAX_SAFE_INTEGER + 1,
                max: MAX_SAFE_INTEGER,
            })
        );
        assert!(matches!(
            after_sequence_from_watermark(u64::MAX),
            Err(WatermarkError::ExceedsSafeInteger {
                watermark: u64::MAX,
                max: MAX_SAFE_INTEGER,
            })
        ));
    }
}
