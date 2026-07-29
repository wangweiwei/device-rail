use devicerail_client::{FramingError, NdjsonDecoder};

fn decode_with_partition_mask(input: &[u8], mask: usize) -> Vec<String> {
    let mut decoder = NdjsonDecoder::new(3).expect("decoder");
    let mut frames = Vec::new();
    let mut chunk_start = 0;
    for boundary in 1..input.len() {
        if mask & (1 << (boundary - 1)) != 0 {
            frames.extend(
                decoder
                    .push(&input[chunk_start..boundary])
                    .expect("valid chunk partition"),
            );
            chunk_start = boundary;
        }
    }
    frames.extend(
        decoder
            .push(&input[chunk_start..])
            .expect("valid final chunk"),
    );
    decoder.finish().expect("clean end");
    frames
}

#[test]
fn exact_limit_frame_accepts_crlf_when_cr_and_lf_are_fragmented() {
    let mut decoder = NdjsonDecoder::new(3).expect("decoder");
    assert!(
        decoder
            .push(b"abc\r")
            .expect("terminal CR is not payload")
            .is_empty()
    );
    assert_eq!(
        decoder
            .push(b"\nxyz\n")
            .expect("complete CRLF and LF frames"),
        ["abc", "xyz"]
    );
    decoder.finish().expect("clean end");
}

#[test]
fn exact_limit_crlf_frames_have_identical_semantics_at_every_chunk_boundary() {
    const INPUT: &[u8] = b"abc\r\nxyz\r\n";
    let partition_count = 1_usize << (INPUT.len() - 1);
    for mask in 0..partition_count {
        assert_eq!(
            decode_with_partition_mask(INPUT, mask),
            ["abc", "xyz"],
            "partition mask {mask:#011b} changed decoder semantics"
        );
    }
}

#[test]
fn one_byte_fragmentation_preserves_an_exact_limit_frame() {
    const FRAME_BYTES: usize = 64 * 1024;
    let mut decoder = NdjsonDecoder::new(FRAME_BYTES).expect("decoder");
    for _ in 0..FRAME_BYTES {
        assert!(decoder.push(b"x").expect("fragment").is_empty());
    }
    let frames = decoder.push(b"\n").expect("delimiter");
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].len(), FRAME_BYTES);
    decoder.finish().expect("clean end");
}

#[test]
fn decoder_exposes_strict_utf8_size_and_eof_failures() {
    let mut invalid_utf8 = NdjsonDecoder::new(8).expect("decoder");
    assert_eq!(
        invalid_utf8.push(&[0xc3, 0x28, b'\n']),
        Err(FramingError::InvalidUtf8)
    );

    let mut oversized = NdjsonDecoder::new(3).expect("decoder");
    assert_eq!(
        oversized.push(b"abcd"),
        Err(FramingError::FrameTooLarge { limit: 3 })
    );

    let mut incomplete = NdjsonDecoder::new(8).expect("decoder");
    incomplete.push(b"partial").expect("buffer partial frame");
    assert_eq!(incomplete.finish(), Err(FramingError::IncompleteFrame));
    assert_eq!(incomplete.push(b"\n"), Err(FramingError::AlreadyEnded));
}
