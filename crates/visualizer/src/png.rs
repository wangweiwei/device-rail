//! Resource-bounded validation for PNG evidence previews.
//!
//! The visualizer must treat Bundle assets as attacker-controlled even after
//! their digest has been validated. This module accepts bytes only, never a
//! path, and deliberately maps decoder failures to stable errors which cannot
//! echo input data.

use std::{fmt, io::Cursor};

use png::{DecodeOptions, Decoder, Limits};

pub const MAX_PREVIEW_ENCODED_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PREVIEW_DIMENSION: u32 = 8_192;
pub const MAX_PREVIEW_PIXELS: u64 = 16_000_000;
pub const MAX_PREVIEW_DECODED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_PREVIEW_METADATA_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PREVIEW_CHUNKS: usize = 4_096;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Limits callers may lower for a particular viewer session.
///
/// Values above the built-in security ceilings are clamped, so constructing a
/// permissive value cannot weaken the visualizer's absolute limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PngPreviewLimits {
    pub max_encoded_bytes: usize,
    pub max_dimension: u32,
    pub max_pixels: u64,
    pub max_decoded_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_chunks: usize,
}

impl Default for PngPreviewLimits {
    fn default() -> Self {
        Self {
            max_encoded_bytes: MAX_PREVIEW_ENCODED_BYTES,
            max_dimension: MAX_PREVIEW_DIMENSION,
            max_pixels: MAX_PREVIEW_PIXELS,
            max_decoded_bytes: MAX_PREVIEW_DECODED_BYTES,
            max_metadata_bytes: MAX_PREVIEW_METADATA_BYTES,
            max_chunks: MAX_PREVIEW_CHUNKS,
        }
    }
}

impl PngPreviewLimits {
    fn constrained(self) -> Self {
        Self {
            max_encoded_bytes: self.max_encoded_bytes.min(MAX_PREVIEW_ENCODED_BYTES),
            max_dimension: self.max_dimension.min(MAX_PREVIEW_DIMENSION),
            max_pixels: self.max_pixels.min(MAX_PREVIEW_PIXELS),
            max_decoded_bytes: self.max_decoded_bytes.min(MAX_PREVIEW_DECODED_BYTES),
            max_metadata_bytes: self.max_metadata_bytes.min(MAX_PREVIEW_METADATA_BYTES),
            max_chunks: self.max_chunks.min(MAX_PREVIEW_CHUNKS),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PngInfo {
    pub width: u32,
    pub height: u32,
    pub decoded_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PngError {
    EncodedLimitExceeded,
    InvalidSignature,
    InvalidStructure,
    Animated,
    InvalidDimensions,
    PixelLimitExceeded,
    DecodedLimitExceeded,
    MetadataLimitExceeded,
    ChunkLimitExceeded,
    DecodeFailed,
}

impl fmt::Display for PngError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EncodedLimitExceeded => "PNG preview exceeds the encoded byte limit",
            Self::InvalidSignature => "PNG preview has an invalid signature",
            Self::InvalidStructure => "PNG preview has an invalid container structure",
            Self::Animated => "animated PNG previews are not supported",
            Self::InvalidDimensions => "PNG preview dimensions are outside the allowed range",
            Self::PixelLimitExceeded => "PNG preview exceeds the pixel limit",
            Self::DecodedLimitExceeded => "PNG preview exceeds the decoded byte limit",
            Self::MetadataLimitExceeded => "PNG preview exceeds the metadata limit",
            Self::ChunkLimitExceeded => "PNG preview exceeds the chunk-count limit",
            Self::DecodeFailed => "PNG preview decoding failed",
        })
    }
}

impl std::error::Error for PngError {}

/// Validates that `bytes` is one complete, static PNG suitable for inline
/// display and returns only its safe display geometry.
///
/// Validation performs a bounded container preflight, a header-only decoder
/// pass, and then a complete decode of the sole frame through `IEND`. Text and
/// ICC payloads are ignored rather than allocated, while all CRC and Adler
/// checks remain enabled.
pub fn validate_preview_png(bytes: &[u8], limits: PngPreviewLimits) -> Result<PngInfo, PngError> {
    let limits = limits.constrained();
    if bytes.len() > limits.max_encoded_bytes {
        return Err(PngError::EncodedLimitExceeded);
    }

    let (preflight_width, preflight_height) = preflight_container(bytes, limits)?;
    validate_dimensions(preflight_width, preflight_height, limits)?;

    let mut options = DecodeOptions::default();
    options.set_ignore_checksums(false);
    options.set_skip_ancillary_crc_failures(false);
    options.set_ignore_text_chunk(true);
    options.set_ignore_iccp_chunk(true);
    let mut decoder = Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(Limits {
        // The decoder allocation budget is an absolute safety ceiling.
        // Caller-specific decoded limits are applied to the calculated
        // frame size below, after bounded metadata parsing.
        bytes: MAX_PREVIEW_DECODED_BYTES,
    });

    // This call stops after IHDR, so hostile dimensions are rejected before
    // metadata parsing or image inflation can allocate meaningful resources.
    let (width, height) = {
        let header = decoder
            .read_header_info()
            .map_err(|_| PngError::DecodeFailed)?;
        (header.width, header.height)
    };
    validate_dimensions(width, height, limits)?;
    if (width, height) != (preflight_width, preflight_height) {
        return Err(PngError::InvalidStructure);
    }

    let mut reader = decoder.read_info().map_err(|_| PngError::DecodeFailed)?;
    if reader.info().animation_control.is_some() || reader.info().frame_control.is_some() {
        return Err(PngError::Animated);
    }

    let decoded_bytes = reader
        .output_buffer_size()
        .ok_or(PngError::DecodedLimitExceeded)?;
    if decoded_bytes > limits.max_decoded_bytes {
        return Err(PngError::DecodedLimitExceeded);
    }

    // Iterating rows forces DEFLATE, scanline, palette, CRC, and Adler
    // validation without allocating a full decoded frame. `finish` validates
    // every remaining chunk through the exact IEND boundary checked above.
    while reader
        .next_row()
        .map_err(|_| PngError::DecodeFailed)?
        .is_some()
    {}
    reader.finish().map_err(|_| PngError::DecodeFailed)?;

    Ok(PngInfo {
        width,
        height,
        decoded_bytes,
    })
}

fn validate_dimensions(width: u32, height: u32, limits: PngPreviewLimits) -> Result<(), PngError> {
    if width == 0 || height == 0 || width > limits.max_dimension || height > limits.max_dimension {
        return Err(PngError::InvalidDimensions);
    }

    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or(PngError::PixelLimitExceeded)?;
    if pixels > limits.max_pixels {
        return Err(PngError::PixelLimitExceeded);
    }
    Ok(())
}

/// Checks the container properties the decoder API does not expose directly:
/// an exact IEND boundary, a bounded number/volume of metadata chunks, and a
/// blanket APNG rejection. Format ordering, CRCs, DEFLATE, palette contents,
/// and scanlines are still validated by the maintained decoder.
fn preflight_container(bytes: &[u8], limits: PngPreviewLimits) -> Result<(u32, u32), PngError> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(PngError::InvalidSignature);
    }

    let mut offset = PNG_SIGNATURE.len();
    let mut chunk_count = 0_usize;
    let mut metadata_bytes = 0_usize;
    let mut saw_ihdr = false;
    let mut saw_idat = false;
    let mut idat_bytes = 0_usize;
    let mut dimensions = None;

    while offset < bytes.len() {
        chunk_count = chunk_count
            .checked_add(1)
            .ok_or(PngError::InvalidStructure)?;
        if chunk_count > limits.max_chunks {
            return Err(PngError::ChunkLimitExceeded);
        }

        let header_end = offset.checked_add(8).ok_or(PngError::InvalidStructure)?;
        if header_end > bytes.len() {
            return Err(PngError::InvalidStructure);
        }

        let chunk_len = usize::try_from(u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .map_err(|_| PngError::InvalidStructure)?,
        ))
        .map_err(|_| PngError::InvalidStructure)?;
        let chunk_type: [u8; 4] = bytes[offset + 4..header_end]
            .try_into()
            .map_err(|_| PngError::InvalidStructure)?;
        let data_end = header_end
            .checked_add(chunk_len)
            .ok_or(PngError::InvalidStructure)?;
        let chunk_end = data_end.checked_add(4).ok_or(PngError::InvalidStructure)?;
        if chunk_end > bytes.len() {
            return Err(PngError::InvalidStructure);
        }

        if matches!(&chunk_type, b"acTL" | b"fcTL" | b"fdAT") {
            return Err(PngError::Animated);
        }

        match &chunk_type {
            b"IHDR" => {
                if saw_ihdr || chunk_count != 1 || chunk_len != 13 {
                    return Err(PngError::InvalidStructure);
                }
                saw_ihdr = true;
                let width = u32::from_be_bytes(
                    bytes[header_end..header_end + 4]
                        .try_into()
                        .map_err(|_| PngError::InvalidStructure)?,
                );
                let height = u32::from_be_bytes(
                    bytes[header_end + 4..header_end + 8]
                        .try_into()
                        .map_err(|_| PngError::InvalidStructure)?,
                );
                dimensions = Some((width, height));
            }
            b"IDAT" => {
                if !saw_ihdr {
                    return Err(PngError::InvalidStructure);
                }
                saw_idat = true;
                idat_bytes = idat_bytes
                    .checked_add(chunk_len)
                    .ok_or(PngError::InvalidStructure)?;
            }
            b"IEND" => {
                if chunk_len != 0 || !saw_ihdr || !saw_idat || idat_bytes == 0 {
                    return Err(PngError::InvalidStructure);
                }
                if chunk_end != bytes.len() {
                    return Err(PngError::InvalidStructure);
                }
                return dimensions.ok_or(PngError::InvalidStructure);
            }
            _ => {
                metadata_bytes = metadata_bytes
                    .checked_add(chunk_len)
                    .ok_or(PngError::MetadataLimitExceeded)?;
                if metadata_bytes > limits.max_metadata_bytes {
                    return Err(PngError::MetadataLimitExceeded);
                }
            }
        }

        offset = chunk_end;
    }

    Err(PngError::InvalidStructure)
}
