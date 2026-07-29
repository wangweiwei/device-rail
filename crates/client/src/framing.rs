use crate::FramingError;

pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Incremental, byte-bounded, fatal-UTF-8 NDJSON decoder.
#[derive(Debug)]
pub struct NdjsonDecoder {
    buffer: Vec<u8>,
    max_frame_bytes: usize,
    scan_from: usize,
    ended: bool,
    failure: Option<FramingError>,
}

impl NdjsonDecoder {
    pub fn new(max_frame_bytes: usize) -> Result<Self, FramingError> {
        if max_frame_bytes == 0 {
            return Err(FramingError::FrameTooLarge { limit: 0 });
        }
        Ok(Self {
            buffer: Vec::new(),
            max_frame_bytes,
            scan_from: 0,
            ended: false,
            failure: None,
        })
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, FramingError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.ended {
            return Err(FramingError::AlreadyEnded);
        }
        self.buffer.extend_from_slice(chunk);

        let mut frames = Vec::new();
        let mut frame_start = 0;
        let mut index = self.scan_from;
        while index < self.buffer.len() {
            if self.buffer[index] != b'\n' {
                let pending_bytes = index.saturating_sub(frame_start).saturating_add(1);
                let split_crlf_candidate = pending_bytes == self.max_frame_bytes.saturating_add(1)
                    && self.buffer[index] == b'\r'
                    && self.buffer.get(index + 1).is_none_or(|next| *next == b'\n');
                if pending_bytes > self.max_frame_bytes && !split_crlf_candidate {
                    return self.fail(FramingError::FrameTooLarge {
                        limit: self.max_frame_bytes,
                    });
                }
                index += 1;
                continue;
            }

            let mut frame_end = index;
            if frame_end > frame_start && self.buffer[frame_end - 1] == b'\r' {
                frame_end -= 1;
            }
            let frame_bytes = &self.buffer[frame_start..frame_end];
            if frame_bytes.len() > self.max_frame_bytes {
                return self.fail(FramingError::FrameTooLarge {
                    limit: self.max_frame_bytes,
                });
            }
            let frame = match std::str::from_utf8(frame_bytes) {
                Ok(frame) => frame,
                Err(_) => return self.fail(FramingError::InvalidUtf8),
            };
            frames.push(frame.to_owned());
            frame_start = index + 1;
            index += 1;
        }

        if frame_start > 0 {
            self.buffer.drain(..frame_start);
            index -= frame_start;
        }
        let split_crlf_candidate = self.buffer.len() == self.max_frame_bytes.saturating_add(1)
            && self.buffer.last() == Some(&b'\r');
        if self.buffer.len() > self.max_frame_bytes && !split_crlf_candidate {
            return self.fail(FramingError::FrameTooLarge {
                limit: self.max_frame_bytes,
            });
        }
        self.scan_from = index;
        Ok(frames)
    }

    pub fn finish(&mut self) -> Result<(), FramingError> {
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        if self.ended {
            return Err(FramingError::AlreadyEnded);
        }
        self.ended = true;
        if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(FramingError::IncompleteFrame)
        }
    }

    fn fail<T>(&mut self, error: FramingError) -> Result<T, FramingError> {
        self.failure = Some(error.clone());
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::NdjsonDecoder;
    use crate::FramingError;

    #[test]
    fn decodes_fragmented_lf_and_crlf_frames() {
        let mut decoder = NdjsonDecoder::new(64).expect("decoder");
        assert!(decoder.push(br#"{"a":"#).expect("prefix").is_empty());
        assert_eq!(
            decoder
                .push(b"1}\r\n{}\n")
                .expect("complete fragmented frames"),
            [r#"{"a":1}"#, "{}"]
        );
        decoder.finish().expect("clean end");
    }

    #[test]
    fn enforces_byte_limit_and_fatal_utf8() {
        let mut exact = NdjsonDecoder::new(3).expect("decoder");
        assert_eq!(exact.push(b"abc\n").expect("exact frame"), ["abc"]);

        let mut split_crlf = NdjsonDecoder::new(3).expect("decoder");
        assert!(split_crlf.push(b"abc\r").expect("split CR").is_empty());
        assert_eq!(split_crlf.push(b"\n").expect("split LF"), ["abc"]);

        let mut oversized = NdjsonDecoder::new(3).expect("decoder");
        assert!(matches!(
            oversized.push(b"abcd"),
            Err(FramingError::FrameTooLarge { limit: 3 })
        ));

        let mut invalid = NdjsonDecoder::new(3).expect("decoder");
        assert_eq!(invalid.push(&[0xff, b'\n']), Err(FramingError::InvalidUtf8));
    }

    #[test]
    fn rejects_partial_eof_and_use_after_end() {
        let mut decoder = NdjsonDecoder::new(16).expect("decoder");
        decoder.push(b"{}").expect("partial frame");
        assert_eq!(decoder.finish(), Err(FramingError::IncompleteFrame));
        assert_eq!(decoder.push(b"\n"), Err(FramingError::AlreadyEnded));
    }
}
