use std::io::Cursor;

use devicerail_visualizer::png::{
    MAX_PREVIEW_DECODED_BYTES, MAX_PREVIEW_DIMENSION, MAX_PREVIEW_ENCODED_BYTES,
    MAX_PREVIEW_PIXELS, PngError, PngPreviewLimits, validate_preview_png,
};

fn encode_rgba(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write PNG header");
        writer.write_image_data(pixels).expect("write PNG pixels");
        writer.finish().expect("write PNG trailer");
    }
    bytes
}

fn tiny_png() -> Vec<u8> {
    encode_rgba(
        2,
        2,
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    )
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn chunks(bytes: &[u8]) -> impl Iterator<Item = (usize, [u8; 4], usize)> + '_ {
    let mut offset = 8_usize;
    std::iter::from_fn(move || {
        if offset + 12 > bytes.len() {
            return None;
        }
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type = bytes[offset + 4..offset + 8].try_into().ok()?;
        let current = offset;
        offset = offset.checked_add(12 + length)?;
        Some((current, chunk_type, length))
    })
}

fn replace_ihdr_dimensions(bytes: &mut [u8], width: u32, height: u32) {
    assert_eq!(&bytes[12..16], b"IHDR");
    bytes[16..20].copy_from_slice(&width.to_be_bytes());
    bytes[20..24].copy_from_slice(&height.to_be_bytes());
    let crc = crc32(&bytes[12..29]);
    bytes[29..33].copy_from_slice(&crc.to_be_bytes());
}

fn png_chunk(chunk_type: [u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(12 + data.len());
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(&chunk_type);
    chunk.extend_from_slice(data);
    let crc = crc32(
        &chunk_type
            .iter()
            .copied()
            .chain(data.iter().copied())
            .collect::<Vec<_>>(),
    );
    chunk.extend_from_slice(&crc.to_be_bytes());
    chunk
}

#[test]
fn accepts_a_complete_small_static_png() {
    let info =
        validate_preview_png(&tiny_png(), PngPreviewLimits::default()).expect("valid static PNG");

    assert_eq!((info.width, info.height), (2, 2));
    assert_eq!(info.decoded_bytes, 16);
}

#[test]
fn rejects_a_false_or_truncated_signature() {
    let mut false_signature = tiny_png();
    false_signature[0] = b'P';
    assert_eq!(
        validate_preview_png(&false_signature, PngPreviewLimits::default()),
        Err(PngError::InvalidSignature)
    );
    assert_eq!(
        validate_preview_png(b"\x89PNG", PngPreviewLimits::default()),
        Err(PngError::InvalidSignature)
    );
}

#[test]
fn rejects_truncated_chunks_and_bytes_after_iend() {
    let mut truncated = tiny_png();
    truncated.pop();
    assert_eq!(
        validate_preview_png(&truncated, PngPreviewLimits::default()),
        Err(PngError::InvalidStructure)
    );

    let mut trailing = tiny_png();
    trailing.extend_from_slice(b"attacker-controlled trailer");
    assert_eq!(
        validate_preview_png(&trailing, PngPreviewLimits::default()),
        Err(PngError::InvalidStructure)
    );
}

#[test]
fn rejects_bad_chunk_crc() {
    let mut bytes = tiny_png();
    let (idat, _, length) = chunks(&bytes)
        .find(|(_, kind, _)| kind == b"IDAT")
        .expect("IDAT chunk");
    assert!(length > 0);
    bytes[idat + 8] ^= 0x01;

    assert_eq!(
        validate_preview_png(&bytes, PngPreviewLimits::default()),
        Err(PngError::DecodeFailed)
    );
}

#[test]
fn rejects_bad_ancillary_chunk_crc() {
    let original = tiny_png();
    let insert_at = 8 + 12 + 13;
    let mut ancillary = png_chunk(*b"tEXt", b"label\0value");
    let last = ancillary.len() - 1;
    ancillary[last] ^= 0x01;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&original[..insert_at]);
    bytes.extend_from_slice(&ancillary);
    bytes.extend_from_slice(&original[insert_at..]);

    assert_eq!(
        validate_preview_png(&bytes, PngPreviewLimits::default()),
        Err(PngError::DecodeFailed)
    );
}

#[test]
fn rejects_zero_and_oversized_dimensions_before_inflation() {
    let mut zero = tiny_png();
    replace_ihdr_dimensions(&mut zero, 0, 2);
    assert_eq!(
        validate_preview_png(&zero, PngPreviewLimits::default()),
        Err(PngError::InvalidDimensions)
    );

    let mut too_wide = tiny_png();
    replace_ihdr_dimensions(&mut too_wide, MAX_PREVIEW_DIMENSION + 1, 1);
    assert_eq!(
        validate_preview_png(&too_wide, PngPreviewLimits::default()),
        Err(PngError::InvalidDimensions)
    );
}

#[test]
fn rejects_dimensions_over_the_pixel_limit_before_inflation() {
    let mut bytes = tiny_png();
    replace_ihdr_dimensions(&mut bytes, 4_001, 4_000);
    assert_eq!(
        validate_preview_png(&bytes, PngPreviewLimits::default()),
        Err(PngError::PixelLimitExceeded)
    );
}

#[test]
fn rejects_apng_control_and_frame_chunks() {
    for (chunk_type, data) in [
        (*b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0][..]),
        (*b"fcTL", &[0; 26][..]),
        (*b"fdAT", &[0, 0, 0, 1][..]),
    ] {
        let original = tiny_png();
        let insert_at = 8 + 12 + 13;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&original[..insert_at]);
        bytes.extend_from_slice(&png_chunk(chunk_type, data));
        bytes.extend_from_slice(&original[insert_at..]);

        assert_eq!(
            validate_preview_png(&bytes, PngPreviewLimits::default()),
            Err(PngError::Animated),
            "APNG chunk {chunk_type:?} must be rejected"
        );
    }
}

#[test]
fn caller_limits_can_only_tighten_absolute_limits() {
    let png = tiny_png();
    let tight_encoded = PngPreviewLimits {
        max_encoded_bytes: png.len() - 1,
        ..PngPreviewLimits::default()
    };
    assert_eq!(
        validate_preview_png(&png, tight_encoded),
        Err(PngError::EncodedLimitExceeded)
    );

    let overly_permissive = PngPreviewLimits {
        max_encoded_bytes: usize::MAX,
        max_dimension: u32::MAX,
        max_pixels: u64::MAX,
        max_decoded_bytes: usize::MAX,
        max_metadata_bytes: usize::MAX,
        max_chunks: usize::MAX,
    };
    let constrained = PngPreviewLimits::default();
    assert_eq!(constrained.max_encoded_bytes, MAX_PREVIEW_ENCODED_BYTES);
    assert_eq!(constrained.max_pixels, MAX_PREVIEW_PIXELS);
    assert_eq!(constrained.max_decoded_bytes, MAX_PREVIEW_DECODED_BYTES);
    assert!(validate_preview_png(&png, overly_permissive).is_ok());
}

#[test]
fn reports_the_chunk_count_budget_explicitly() {
    let limits = PngPreviewLimits {
        max_chunks: 1,
        ..PngPreviewLimits::default()
    };
    assert_eq!(
        validate_preview_png(&tiny_png(), limits),
        Err(PngError::ChunkLimitExceeded)
    );
}

#[test]
fn bounds_metadata_before_the_decoder_can_allocate_it() {
    let original = tiny_png();
    let insert_at = 8 + 12 + 13;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&original[..insert_at]);
    bytes.extend_from_slice(&png_chunk(*b"tEXt", b"1234"));
    bytes.extend_from_slice(&original[insert_at..]);
    let limits = PngPreviewLimits {
        max_metadata_bytes: 3,
        ..PngPreviewLimits::default()
    };

    assert_eq!(
        validate_preview_png(&bytes, limits),
        Err(PngError::MetadataLimitExceeded)
    );
}

#[test]
fn bounds_the_calculated_decoded_frame_size() {
    let bytes = tiny_png();
    let limits = PngPreviewLimits {
        max_decoded_bytes: 15,
        ..PngPreviewLimits::default()
    };

    assert_eq!(
        validate_preview_png(&bytes, limits),
        Err(PngError::DecodedLimitExceeded)
    );
}

#[test]
fn errors_never_echo_input_bytes() {
    let marker = b"SECRET-PATH-file:///private/evidence.png";
    let error = validate_preview_png(marker, PngPreviewLimits::default())
        .expect_err("not a PNG")
        .to_string();
    assert!(!error.contains("SECRET"));
    assert!(!error.contains("file://"));
}

#[test]
fn complete_decode_rejects_invalid_deflate_even_with_valid_container_crcs() {
    let mut bytes = tiny_png();
    let (idat, _, length) = chunks(&bytes)
        .find(|(_, kind, _)| kind == b"IDAT")
        .expect("IDAT chunk");
    assert!(length >= 2);
    bytes[idat + 8] ^= 0x7f;
    let crc = crc32(&bytes[idat + 4..idat + 8 + length]);
    bytes[idat + 8 + length..idat + 12 + length].copy_from_slice(&crc.to_be_bytes());

    assert_eq!(
        validate_preview_png(&bytes, PngPreviewLimits::default()),
        Err(PngError::DecodeFailed)
    );
}

#[test]
fn png_encoder_fixture_itself_is_strictly_decodable() {
    let bytes = tiny_png();
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info().expect("fixture metadata");
    while reader.next_row().expect("fixture row").is_some() {}
    reader.finish().expect("fixture trailer");
}
