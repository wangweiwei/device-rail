use std::io::Cursor;

use crate::{AndroidAdbError, AndroidAdbResult, command::SCREENSHOT_STDOUT_LIMIT};
use devicerail_core::{DeviceOperationError, DriverError, EvidenceError};
use devicerail_protocol::DeviceId;
use png::{BitDepth, ColorType, Decoder, Limits};
use thiserror::Error;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_SCREENSHOT_DIMENSION: u32 = 16_384;
const MAX_SCREENSHOT_PIXELS: u64 = 33_554_432;
const MAX_DECODED_FRAME_BYTES: usize = 128 * 1024 * 1024;
const MAX_DISPLAY_DENSITY_DPI: u32 = 10_000;
const MAX_PNG_CHUNKS: usize = 4096;
const ANDROID_BASE_DENSITY_DPI: f64 = 160.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelSize {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl PixelSize {
    fn new(width: u32, height: u32, input: &'static str) -> AndroidAdbResult<Self> {
        if width == 0 || height == 0 {
            return Err(malformed_observation(
                input,
                "width and height must both be positive",
            ));
        }
        Ok(Self { width, height })
    }

    fn orientation(self) -> DisplayOrientation {
        match self.width.cmp(&self.height) {
            std::cmp::Ordering::Less => DisplayOrientation::Portrait,
            std::cmp::Ordering::Greater => DisplayOrientation::Landscape,
            std::cmp::Ordering::Equal => DisplayOrientation::Square,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayOrientation {
    Portrait,
    Landscape,
    Square,
}

impl DisplayOrientation {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Portrait => "portrait",
            Self::Landscape => "landscape",
            Self::Square => "square",
        }
    }
}

/// Observation failures preserve the Android Driver's platform/evidence
/// boundary. Evidence failures stay evidence failures when this internal
/// result is mapped into Core.
#[derive(Debug, Error)]
pub(crate) enum AndroidObservationError {
    #[error("Android device {0} is not logically connected")]
    NotConnected(DeviceId),
    #[error(transparent)]
    Adb(#[from] AndroidAdbError),
    #[error(transparent)]
    Evidence(#[from] EvidenceError),
}

pub(crate) type AndroidObservationResult<T> = Result<T, AndroidObservationError>;

impl AndroidObservationError {
    pub(crate) fn into_device_operation_error(self) -> DeviceOperationError {
        match self {
            Self::NotConnected(device_id) => DriverError::NotConnected(device_id).into(),
            Self::Adb(AndroidAdbError::Cancelled) => DriverError::Cancelled.into(),
            Self::Adb(AndroidAdbError::TimedOut { .. }) => DriverError::TimedOut.into(),
            Self::Adb(error) => DriverError::Platform {
                code: error.code().to_owned(),
                retryable: error.retryable(),
            }
            .into(),
            Self::Evidence(error) => error.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DisplaySizeReading {
    pub(crate) physical: PixelSize,
    pub(crate) override_size: Option<PixelSize>,
}

impl DisplaySizeReading {
    pub(crate) fn effective(self) -> PixelSize {
        self.override_size.unwrap_or(self.physical)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DisplayDensityReading {
    pub(crate) physical_dpi: u32,
    pub(crate) override_dpi: Option<u32>,
}

impl DisplayDensityReading {
    pub(crate) fn effective_dpi(self) -> u32 {
        self.override_dpi.unwrap_or(self.physical_dpi)
    }
}

/// Validated display values used to construct a protocol `Observation` later.
///
/// This intentionally contains no evidence or protocol DTO. The Android
/// driver remains responsible for persisting the validated screenshot through
/// the Session's evidence boundary before publishing an Observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AndroidObservationGeometry {
    pub(crate) screenshot_size: Option<PixelSize>,
    pub(crate) viewport_size: PixelSize,
    pub(crate) orientation: DisplayOrientation,
    pub(crate) display_size: DisplaySizeReading,
    pub(crate) display_density: DisplayDensityReading,
    pub(crate) density_dpi: u32,
    pub(crate) scale_factor: f64,
}

pub(crate) fn parse_observation_geometry(
    screenshot_png: &[u8],
    window_size_output: &str,
    window_density_output: &str,
) -> AndroidAdbResult<AndroidObservationGeometry> {
    let screenshot_size = parse_png_dimensions(screenshot_png)?;
    let display_size = parse_window_size(window_size_output)?;
    let display_density = parse_window_density(window_density_output)?;
    // The screenshot is the exact captured coordinate space. `wm size` is
    // retained for metadata and diagnostics because Android commonly reports
    // it in natural orientation even while the display is rotated.
    build_geometry(Some(screenshot_size), display_size, display_density)
}

pub(crate) fn parse_display_only_geometry(
    window_size_output: &str,
    window_density_output: &str,
) -> AndroidAdbResult<AndroidObservationGeometry> {
    let display_size = parse_window_size(window_size_output)?;
    let display_density = parse_window_density(window_density_output)?;
    build_geometry(None, display_size, display_density)
}

fn build_geometry(
    screenshot_size: Option<PixelSize>,
    display_size: DisplaySizeReading,
    display_density: DisplayDensityReading,
) -> AndroidAdbResult<AndroidObservationGeometry> {
    let viewport_size = screenshot_size.unwrap_or_else(|| display_size.effective());
    let orientation = viewport_size.orientation();
    let density_dpi = display_density.effective_dpi();
    let scale_factor = f64::from(density_dpi) / ANDROID_BASE_DENSITY_DPI;
    if !scale_factor.is_finite() || scale_factor <= 0.0 {
        return Err(malformed_observation(
            "wm density",
            "computed scale factor must be finite and positive",
        ));
    }

    Ok(AndroidObservationGeometry {
        screenshot_size,
        viewport_size,
        orientation,
        display_size,
        display_density,
        density_dpi,
        scale_factor,
    })
}

pub(crate) fn parse_png_dimensions(bytes: &[u8]) -> AndroidAdbResult<PixelSize> {
    // The input is already bounded by the adb process runner. Keep the same
    // bound here because this parser is also exercised through replaceable
    // runners in tests and future embeddings.
    if bytes.len() > SCREENSHOT_STDOUT_LIMIT {
        return Err(malformed_png(format!(
            "encoded screenshot exceeds {SCREENSHOT_STDOUT_LIMIT} bytes"
        )));
    }
    preflight_png_container(bytes)?;

    let mut decoder = Decoder::new_with_limits(
        Cursor::new(bytes),
        Limits {
            bytes: MAX_DECODED_FRAME_BYTES,
        },
    );
    // Screenshot metadata is not used by the protocol DTO. Skipping text and
    // ICC payloads keeps those attacker-controlled chunks from consuming
    // memory; CRC and Adler checks remain enabled.
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    decoder.ignore_checksums(false);

    let header = decoder
        .read_header_info()
        .map_err(|error| malformed_png(format!("invalid PNG header: {error}")))?;
    let dimensions = validate_png_resource_limits(
        header.width,
        header.height,
        header.color_type,
        header.bit_depth,
    )?;

    let mut reader = decoder
        .read_info()
        .map_err(|error| malformed_png(format!("invalid PNG metadata: {error}")))?;
    if reader.info().animation_control.is_some() || reader.info().frame_control.is_some() {
        return Err(malformed_png(
            "Android screenshots must be a single static PNG frame",
        ));
    }
    let decoded_size = reader
        .output_buffer_size()
        .ok_or_else(|| malformed_png("decoded frame size overflow"))?;
    if decoded_size > MAX_DECODED_FRAME_BYTES {
        return Err(malformed_png(format!(
            "decoded frame exceeds {MAX_DECODED_FRAME_BYTES} bytes"
        )));
    }

    // Iterating every row forces complete IDAT inflation, scanline unfiltering,
    // palette validation, and Adler verification without allocating a second
    // full-frame buffer. `finish` then validates all remaining chunks through
    // IEND. The container preflight below additionally rejects bytes after it.
    while reader
        .next_row()
        .map_err(|error| malformed_png(format!("invalid PNG image data: {error}")))?
        .is_some()
    {}
    reader
        .finish()
        .map_err(|error| malformed_png(format!("invalid PNG trailer: {error}")))?;

    Ok(dimensions)
}

fn validate_png_resource_limits(
    width: u32,
    height: u32,
    color_type: ColorType,
    bit_depth: BitDepth,
) -> AndroidAdbResult<PixelSize> {
    if width == 0
        || height == 0
        || width > MAX_SCREENSHOT_DIMENSION
        || height > MAX_SCREENSHOT_DIMENSION
    {
        return Err(malformed_png(format!(
            "screenshot dimensions must each be between 1 and {MAX_SCREENSHOT_DIMENSION}"
        )));
    }

    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| malformed_png("screenshot pixel count overflow"))?;
    if pixels > MAX_SCREENSHOT_PIXELS {
        return Err(malformed_png(format!(
            "screenshot exceeds {MAX_SCREENSHOT_PIXELS} pixels"
        )));
    }

    let row_bits = u64::from(width)
        .checked_mul(color_type.samples() as u64)
        .and_then(|value| value.checked_mul(bit_depth as u64))
        .ok_or_else(|| malformed_png("decoded row size overflow"))?;
    let row_bytes = row_bits
        .checked_add(7)
        .map(|value| value / 8)
        .ok_or_else(|| malformed_png("decoded row size overflow"))?;
    let decoded_bytes = row_bytes
        .checked_mul(u64::from(height))
        .ok_or_else(|| malformed_png("decoded frame size overflow"))?;
    if decoded_bytes > MAX_DECODED_FRAME_BYTES as u64 {
        return Err(malformed_png(format!(
            "decoded frame exceeds {MAX_DECODED_FRAME_BYTES} bytes"
        )));
    }

    Ok(PixelSize { width, height })
}

/// Performs only the checks the decoder API cannot express for a borrowed
/// in-memory input: an exact IEND boundary and a blanket APNG rejection. All
/// PNG format, ordering, CRC, DEFLATE, Adler, palette contents, and scanline
/// validation is delegated to the maintained `png` decoder above. The one
/// explicit palette supplement rejects indexed images before IDAT when PLTE is
/// absent because the decoder otherwise permits that omission in identity mode.
fn preflight_png_container(bytes: &[u8]) -> AndroidAdbResult<()> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err(malformed_png("invalid or truncated PNG signature"));
    }

    let mut offset = PNG_SIGNATURE.len();
    let mut indexed = false;
    let mut seen_plte = false;
    let mut idat_bytes = 0_usize;
    let mut chunk_count = 0_usize;
    while offset < bytes.len() {
        chunk_count += 1;
        if chunk_count > MAX_PNG_CHUNKS {
            return Err(malformed_png(format!(
                "PNG contains more than {MAX_PNG_CHUNKS} chunks"
            )));
        }
        let header_end = offset
            .checked_add(8)
            .ok_or_else(|| malformed_png("chunk header offset overflow"))?;
        if header_end > bytes.len() {
            return Err(malformed_png("truncated PNG chunk header"));
        }
        let length = u32::from_be_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("four-byte PNG chunk length"),
        ) as usize;
        let chunk_type: [u8; 4] = bytes[offset + 4..header_end]
            .try_into()
            .expect("four-byte PNG chunk type");
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|value| value.checked_add(4))
            .ok_or_else(|| malformed_png("chunk length overflow"))?;
        if chunk_end > bytes.len() {
            return Err(malformed_png("truncated PNG chunk"));
        }

        if matches!(&chunk_type, b"acTL" | b"fcTL" | b"fdAT") {
            return Err(malformed_png(
                "Android screenshots must not contain APNG chunks",
            ));
        }
        let data = &bytes[header_end..header_end + length];
        match &chunk_type {
            b"IHDR" if data.len() >= 10 => indexed = data[9] == ColorType::Indexed as u8,
            b"PLTE" => seen_plte = true,
            b"IDAT" => {
                if indexed && !seen_plte {
                    return Err(malformed_png("indexed PNG is missing a PLTE chunk"));
                }
                idat_bytes = idat_bytes
                    .checked_add(length)
                    .ok_or_else(|| malformed_png("IDAT length overflow"))?;
            }
            _ => {}
        }
        if &chunk_type == b"IEND" {
            if chunk_end != bytes.len() {
                return Err(malformed_png("PNG contains bytes after IEND"));
            }
            if idat_bytes == 0 {
                return Err(malformed_png("PNG contains no compressed image data"));
            }
            return Ok(());
        }
        offset = chunk_end;
    }

    Err(malformed_png("PNG is missing an IEND chunk"))
}

pub(crate) fn parse_window_size(output: &str) -> AndroidAdbResult<DisplaySizeReading> {
    let mut physical = None;
    let mut override_size = None;

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(value) = line.strip_prefix("Physical size:") {
            if physical.is_some() {
                return Err(malformed_observation("wm size", "duplicate physical size"));
            }
            physical = Some(parse_pixel_size(value.trim(), "wm size")?);
        } else if let Some(value) = line.strip_prefix("Override size:") {
            if override_size.is_some() {
                return Err(malformed_observation("wm size", "duplicate override size"));
            }
            override_size = Some(parse_pixel_size(value.trim(), "wm size")?);
        } else {
            return Err(malformed_observation(
                "wm size",
                format!("unrecognized non-empty line {line:?}"),
            ));
        }
    }

    let physical =
        physical.ok_or_else(|| malformed_observation("wm size", "missing physical size"))?;
    Ok(DisplaySizeReading {
        physical,
        override_size,
    })
}

fn parse_pixel_size(value: &str, input: &'static str) -> AndroidAdbResult<PixelSize> {
    let (width, height) = value
        .split_once('x')
        .ok_or_else(|| malformed_observation(input, format!("invalid size {value:?}")))?;
    if height.contains('x') {
        return Err(malformed_observation(
            input,
            format!("invalid size {value:?}"),
        ));
    }
    let width = parse_positive_u32(width.trim(), input, "width")?;
    let height = parse_positive_u32(height.trim(), input, "height")?;
    let size = PixelSize::new(width, height, input)?;
    if size.width > MAX_SCREENSHOT_DIMENSION || size.height > MAX_SCREENSHOT_DIMENSION {
        return Err(malformed_observation(
            input,
            format!("width and height must not exceed {MAX_SCREENSHOT_DIMENSION}"),
        ));
    }
    let pixels = u64::from(size.width)
        .checked_mul(u64::from(size.height))
        .ok_or_else(|| malformed_observation(input, "display pixel count overflow"))?;
    if pixels > MAX_SCREENSHOT_PIXELS {
        return Err(malformed_observation(
            input,
            format!("display size must not exceed {MAX_SCREENSHOT_PIXELS} pixels"),
        ));
    }
    Ok(size)
}

pub(crate) fn parse_window_density(output: &str) -> AndroidAdbResult<DisplayDensityReading> {
    let mut physical_dpi = None;
    let mut override_dpi = None;

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(value) = line.strip_prefix("Physical density:") {
            if physical_dpi.is_some() {
                return Err(malformed_observation(
                    "wm density",
                    "duplicate physical density",
                ));
            }
            physical_dpi = Some(parse_density_dpi(
                value.trim(),
                "wm density",
                "physical density",
            )?);
        } else if let Some(value) = line.strip_prefix("Override density:") {
            if override_dpi.is_some() {
                return Err(malformed_observation(
                    "wm density",
                    "duplicate override density",
                ));
            }
            override_dpi = Some(parse_density_dpi(
                value.trim(),
                "wm density",
                "override density",
            )?);
        } else {
            return Err(malformed_observation(
                "wm density",
                format!("unrecognized non-empty line {line:?}"),
            ));
        }
    }

    let physical_dpi = physical_dpi
        .ok_or_else(|| malformed_observation("wm density", "missing physical density"))?;
    Ok(DisplayDensityReading {
        physical_dpi,
        override_dpi,
    })
}

fn parse_density_dpi(
    value: &str,
    input: &'static str,
    field: &'static str,
) -> AndroidAdbResult<u32> {
    let dpi = parse_positive_u32(value, input, field)?;
    if dpi > MAX_DISPLAY_DENSITY_DPI {
        return Err(malformed_observation(
            input,
            format!("{field} must not exceed {MAX_DISPLAY_DENSITY_DPI} dpi"),
        ));
    }
    Ok(dpi)
}

fn parse_positive_u32(
    value: &str,
    input: &'static str,
    field: &'static str,
) -> AndroidAdbResult<u32> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| malformed_observation(input, format!("{field} is not a u32: {value:?}")))?;
    if parsed == 0 {
        return Err(malformed_observation(
            input,
            format!("{field} must be positive"),
        ));
    }
    Ok(parsed)
}

fn malformed_png(detail: impl Into<String>) -> AndroidAdbError {
    AndroidAdbError::MalformedPng(detail.into())
}

fn malformed_observation(input: &'static str, detail: impl Into<String>) -> AndroidAdbError {
    AndroidAdbError::MalformedObservation {
        input,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayOrientation, MAX_DECODED_FRAME_BYTES, MAX_DISPLAY_DENSITY_DPI, MAX_PNG_CHUNKS,
        MAX_SCREENSHOT_DIMENSION, PixelSize, parse_display_only_geometry,
        parse_observation_geometry, parse_png_dimensions, parse_window_density, parse_window_size,
        validate_png_resource_limits,
    };
    use crate::AndroidAdbError;
    use png::{BitDepth, ColorType, Encoder};

    fn push_chunk(png: &mut Vec<u8>, chunk_type: [u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(&chunk_type);
        png.extend_from_slice(data);
        png.extend_from_slice(&crc32(chunk_type, data).to_be_bytes());
    }

    fn fixture_png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, width, height);
            encoder.set_color(ColorType::Grayscale);
            encoder.set_depth(BitDepth::Eight);
            let mut writer = encoder.write_header().expect("PNG header");
            let pixels = usize::try_from(u64::from(width) * u64::from(height))
                .expect("fixture dimensions fit usize");
            writer
                .write_image_data(&vec![0; pixels])
                .expect("PNG image data");
            writer.finish().expect("PNG trailer");
        }
        bytes
    }

    fn crc32(chunk_type: [u8; 4], data: &[u8]) -> u32 {
        let mut crc = u32::MAX;
        for byte in chunk_type.iter().chain(data) {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0_u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn chunks(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        let mut result = Vec::new();
        let mut offset = super::PNG_SIGNATURE.len();
        while offset < bytes.len() {
            let length =
                u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("length")) as usize;
            let chunk_type = bytes[offset + 4..offset + 8].try_into().expect("type");
            let data_start = offset + 8;
            let data_end = data_start + length;
            result.push((chunk_type, bytes[data_start..data_end].to_vec()));
            offset = data_end + 4;
        }
        result
    }

    fn rebuild_png(chunks: impl IntoIterator<Item = ([u8; 4], Vec<u8>)>) -> Vec<u8> {
        let mut bytes = super::PNG_SIGNATURE.to_vec();
        for (chunk_type, data) in chunks {
            push_chunk(&mut bytes, chunk_type, &data);
        }
        bytes
    }

    fn rewrite_ihdr(
        bytes: &[u8],
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
    ) -> Vec<u8> {
        let mut chunks = chunks(bytes);
        let ihdr = &mut chunks.first_mut().expect("IHDR").1;
        ihdr[0..4].copy_from_slice(&width.to_be_bytes());
        ihdr[4..8].copy_from_slice(&height.to_be_bytes());
        ihdr[8] = bit_depth;
        ihdr[9] = color_type;
        rebuild_png(chunks)
    }

    fn adler32(data: &[u8]) -> u32 {
        let mut a = 1_u32;
        let mut b = 0_u32;
        for byte in data {
            a = (a + u32::from(*byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }

    fn stored_zlib(data: &[u8]) -> Vec<u8> {
        let length = u16::try_from(data.len()).expect("small stored block");
        let mut stream = vec![0x78, 0x01, 0x01];
        stream.extend_from_slice(&length.to_le_bytes());
        stream.extend_from_slice(&(!length).to_le_bytes());
        stream.extend_from_slice(data);
        stream.extend_from_slice(&adler32(data).to_be_bytes());
        stream
    }

    fn custom_png(ihdr: Vec<u8>, idat: Vec<u8>) -> Vec<u8> {
        rebuild_png([(*b"IHDR", ihdr), (*b"IDAT", idat), (*b"IEND", Vec::new())])
    }

    #[test]
    fn parses_crlf_and_uses_size_and_density_overrides() {
        let size = parse_window_size("Physical size: 1080x2400\r\nOverride size: 720x1280\r\n")
            .expect("valid size");
        assert_eq!(
            size.physical,
            PixelSize {
                width: 1080,
                height: 2400
            }
        );
        assert_eq!(
            size.override_size,
            Some(PixelSize {
                width: 720,
                height: 1280
            })
        );
        assert_eq!(
            size.effective(),
            PixelSize {
                width: 720,
                height: 1280
            }
        );

        let density = parse_window_density("Physical density: 420\r\nOverride density: 560\r\n")
            .expect("valid density");
        assert_eq!(density.physical_dpi, 420);
        assert_eq!(density.override_dpi, Some(560));
        assert_eq!(density.effective_dpi(), 560);
    }

    #[test]
    fn derives_landscape_viewport_from_rotated_screenshot() {
        let png = fixture_png(2400, 1080);
        let geometry = parse_observation_geometry(
            &png,
            "Physical size: 1080x2400\n",
            "Physical density: 420\n",
        )
        .expect("valid geometry");

        assert_eq!(
            geometry.screenshot_size,
            Some(PixelSize {
                width: 2400,
                height: 1080
            })
        );
        assert_eq!(
            geometry.viewport_size,
            PixelSize {
                width: 2400,
                height: 1080
            }
        );
        assert_eq!(geometry.orientation, DisplayOrientation::Landscape);
        assert_eq!(
            geometry.display_size.effective(),
            PixelSize {
                width: 1080,
                height: 2400
            }
        );
        assert_eq!(geometry.display_density.physical_dpi, 420);
        assert_eq!(geometry.density_dpi, 420);
        assert_eq!(geometry.scale_factor, 2.625);
    }

    #[test]
    fn display_only_geometry_uses_effective_display_without_screenshot_dimensions() {
        let geometry = parse_display_only_geometry(
            "Physical size: 1080x2400\nOverride size: 720x1280\n",
            "Physical density: 420\nOverride density: 560\n",
        )
        .expect("valid display-only geometry");

        assert_eq!(geometry.screenshot_size, None);
        assert_eq!(
            geometry.viewport_size,
            PixelSize {
                width: 720,
                height: 1280,
            }
        );
        assert_eq!(geometry.orientation, DisplayOrientation::Portrait);
        assert_eq!(geometry.density_dpi, 560);
        assert_eq!(geometry.scale_factor, 3.5);
    }

    #[test]
    fn derives_portrait_and_square_orientations() {
        let portrait = parse_observation_geometry(
            &fixture_png(1080, 2400),
            "Physical size: 1080x2400\n",
            "Physical density: 160\n",
        )
        .expect("portrait");
        assert_eq!(portrait.orientation, DisplayOrientation::Portrait);

        let square = parse_observation_geometry(
            &fixture_png(1024, 1024),
            "Physical size: 1024x1024\n",
            "Physical density: 160\n",
        )
        .expect("square");
        assert_eq!(square.orientation, DisplayOrientation::Square);
    }

    #[test]
    fn enforces_device_screenshot_and_display_resource_limits() {
        assert_eq!(
            validate_png_resource_limits(
                MAX_SCREENSHOT_DIMENSION,
                2048,
                ColorType::Rgba,
                BitDepth::Eight,
            )
            .expect("maximum pixels and decoded bytes"),
            PixelSize {
                width: MAX_SCREENSHOT_DIMENSION,
                height: 2048,
            }
        );
        assert!(matches!(
            validate_png_resource_limits(
                MAX_SCREENSHOT_DIMENSION + 1,
                1,
                ColorType::Grayscale,
                BitDepth::Eight,
            ),
            Err(AndroidAdbError::MalformedPng(_))
        ));
        assert!(matches!(
            validate_png_resource_limits(8193, 4096, ColorType::Grayscale, BitDepth::Eight),
            Err(AndroidAdbError::MalformedPng(_))
        ));
        assert!(matches!(
            validate_png_resource_limits(
                MAX_SCREENSHOT_DIMENSION,
                2048,
                ColorType::Rgba,
                BitDepth::Sixteen,
            ),
            Err(AndroidAdbError::MalformedPng(_))
        ));
        assert_eq!(MAX_DECODED_FRAME_BYTES, 128 * 1024 * 1024);

        let size =
            parse_window_size("Physical size: 16384x2048\n").expect("display dimension boundary");
        assert_eq!(size.physical.width, MAX_SCREENSHOT_DIMENSION);
        assert_eq!(size.physical.height, 2048);
        let density = parse_window_density("Physical density: 10000\n").expect("density boundary");
        assert_eq!(density.physical_dpi, MAX_DISPLAY_DENSITY_DPI);
        assert_eq!(f64::from(density.effective_dpi()) / 160.0, 62.5);

        assert!(parse_window_size("Physical size: 16385x1\n").is_err());
        assert!(parse_window_size("Physical size: 8193x4096\n").is_err());
        assert!(parse_window_density("Physical density: 10001\n").is_err());
    }

    #[test]
    fn rejects_invalid_png_signature_zero_dimensions_and_missing_idat() {
        let signature_error =
            parse_png_dimensions(b"not a png").expect_err("signature must be validated");
        assert!(matches!(signature_error, AndroidAdbError::MalformedPng(_)));
        assert_eq!(signature_error.code(), "android_adb_malformed_png");

        let zero = rewrite_ihdr(&fixture_png(1, 1), 0, 100, 8, 0);
        let zero_error = parse_png_dimensions(&zero).expect_err("zero dimensions are invalid");
        assert!(matches!(zero_error, AndroidAdbError::MalformedPng(_)));

        let source_chunks = chunks(&fixture_png(1, 1));
        let missing_idat = rebuild_png(
            source_chunks
                .into_iter()
                .filter(|(kind, _)| kind != b"IDAT"),
        );
        assert!(matches!(
            parse_png_dimensions(&missing_idat),
            Err(AndroidAdbError::MalformedPng(_))
        ));
    }

    #[test]
    fn rejects_truncated_corrupt_and_trailing_png_bytes() {
        let mut truncated = fixture_png(100, 200);
        truncated.pop();
        assert!(matches!(
            parse_png_dimensions(&truncated),
            Err(AndroidAdbError::MalformedPng(_))
        ));

        let mut corrupt = fixture_png(100, 200);
        let ihdr_width_byte = super::PNG_SIGNATURE.len() + 8;
        corrupt[ihdr_width_byte] ^= 1;
        assert!(matches!(
            parse_png_dimensions(&corrupt),
            Err(AndroidAdbError::MalformedPng(_))
        ));

        let mut trailing = fixture_png(100, 200);
        trailing.extend_from_slice(b"garbage");
        assert!(matches!(
            parse_png_dimensions(&trailing),
            Err(AndroidAdbError::MalformedPng(_))
        ));
    }

    #[test]
    fn rejects_truncated_deflate_bad_adler_invalid_scanline_and_empty_idat() {
        let source = chunks(&fixture_png(2, 1));
        let ihdr = source
            .iter()
            .find(|(kind, _)| kind == b"IHDR")
            .expect("IHDR")
            .1
            .clone();
        let idat = source
            .iter()
            .find(|(kind, _)| kind == b"IDAT")
            .expect("IDAT")
            .1
            .clone();

        let mut truncated = stored_zlib(&[0, 0, 0]);
        truncated.truncate(truncated.len() - 6);
        assert!(parse_png_dimensions(&custom_png(ihdr.clone(), truncated)).is_err());

        let mut bad_adler = idat;
        let last = bad_adler.last_mut().expect("Adler byte");
        *last ^= 1;
        assert!(parse_png_dimensions(&custom_png(ihdr.clone(), bad_adler)).is_err());

        let bad_filter = stored_zlib(&[5, 0, 0]);
        assert!(parse_png_dimensions(&custom_png(ihdr.clone(), bad_filter)).is_err());
        assert!(parse_png_dimensions(&custom_png(ihdr, Vec::new())).is_err());
    }

    #[test]
    fn rejects_indexed_image_without_palette_apng_and_oversized_headers() {
        let mut indexed_ihdr = Vec::new();
        indexed_ihdr.extend_from_slice(&1_u32.to_be_bytes());
        indexed_ihdr.extend_from_slice(&1_u32.to_be_bytes());
        indexed_ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
        let indexed_without_palette = custom_png(indexed_ihdr, stored_zlib(&[0, 0]));
        assert!(parse_png_dimensions(&indexed_without_palette).is_err());

        let animated = rebuild_png(chunks(&fixture_png(1, 1)).into_iter().flat_map(
            |chunk @ (kind, _)| {
                if kind == *b"IDAT" {
                    vec![
                        (
                            *b"acTL",
                            [1_u32.to_be_bytes(), 0_u32.to_be_bytes()].concat(),
                        ),
                        chunk,
                    ]
                } else {
                    vec![chunk]
                }
            },
        ));
        assert!(parse_png_dimensions(&animated).is_err());

        let base = fixture_png(1, 1);
        let too_wide = rewrite_ihdr(&base, MAX_SCREENSHOT_DIMENSION + 1, 1, 8, 0);
        assert!(parse_png_dimensions(&too_wide).is_err());
        let too_many_pixels = rewrite_ihdr(&base, 8193, 4096, 8, 0);
        assert!(parse_png_dimensions(&too_many_pixels).is_err());
        let too_many_decoded_bytes = rewrite_ihdr(&base, MAX_SCREENSHOT_DIMENSION, 2048, 16, 6);
        assert!(parse_png_dimensions(&too_many_decoded_bytes).is_err());

        let flooded = rebuild_png(chunks(&fixture_png(1, 1)).into_iter().flat_map(
            |chunk @ (kind, _)| {
                if kind == *b"IDAT" {
                    let mut chunks = vec![(*b"vpAg", Vec::new()); MAX_PNG_CHUNKS];
                    chunks.push(chunk);
                    chunks
                } else {
                    vec![chunk]
                }
            },
        ));
        assert!(parse_png_dimensions(&flooded).is_err());
    }

    #[test]
    fn rejects_duplicate_zero_overflow_and_unknown_window_lines() {
        for invalid in [
            "Physical size: 1080x2400\nPhysical size: 720x1280\n",
            "Physical size: 0x2400\n",
            "Physical size: 4294967296x2400\n",
            "Physical size: 16385x2400\n",
            "Physical size: 1080x2400\nFuture size: 1x1\n",
            "Override size: 720x1280\n",
        ] {
            let error = parse_window_size(invalid).expect_err("invalid wm size");
            assert!(matches!(
                error,
                AndroidAdbError::MalformedObservation {
                    input: "wm size",
                    ..
                }
            ));
            assert_eq!(error.code(), "android_adb_malformed_observation");
        }

        for invalid in [
            "Physical density: 420\nPhysical density: 560\n",
            "Physical density: 0\n",
            "Physical density: 4294967296\n",
            "Physical density: 10001\n",
            "Physical density: 420\nFuture density: 1\n",
            "Override density: 560\n",
        ] {
            assert!(matches!(
                parse_window_density(invalid),
                Err(AndroidAdbError::MalformedObservation {
                    input: "wm density",
                    ..
                })
            ));
        }
    }
}
