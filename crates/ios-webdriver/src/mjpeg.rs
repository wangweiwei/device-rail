use std::collections::BTreeMap;

use async_trait::async_trait;
use devicerail_core::{DriverResult, ExecutionControl};
use tokio::{io::AsyncWriteExt, net::TcpStream};

use crate::{
    HttpEndpointConfig,
    control::{platform, run_controlled},
    http::{find_subslice, read_http_head, read_more_bounded},
};

const MAX_MJPEG_PREAMBLE_BYTES: usize = 64 * 1024;
const MAX_MJPEG_PART_HEADERS_BYTES: usize = 32 * 1024;
const MAX_JPEG_BYTES: usize = 32 * 1024 * 1024;
const MAX_JPEG_DIMENSION: u32 = 16_384;
const MAX_JPEG_PIXELS: u64 = 64_000_000;
const MAX_BOUNDARY_BYTES: usize = 70;

#[derive(Clone, PartialEq, Eq)]
pub struct MjpegFrame {
    jpeg: Vec<u8>,
    width: u32,
    height: u32,
}

impl std::fmt::Debug for MjpegFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MjpegFrame")
            .field("byte_len", &self.jpeg.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

impl MjpegFrame {
    pub fn from_jpeg(jpeg: Vec<u8>) -> DriverResult<Self> {
        let (width, height) = validate_jpeg(&jpeg)?;
        Ok(Self {
            jpeg,
            width,
            height,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.jpeg
    }

    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.jpeg
    }
}

#[async_trait]
pub trait MjpegFrameSource: Send + Sync {
    /// Reads one complete frame. Implementations must not leave a background
    /// stream/task running after this future is dropped.
    async fn latest_frame(&self, control: &ExecutionControl) -> DriverResult<MjpegFrame>;
}

#[derive(Clone, Debug)]
pub struct SystemMjpegFrameSource {
    endpoint: HttpEndpointConfig,
}

impl SystemMjpegFrameSource {
    pub fn new(endpoint: HttpEndpointConfig) -> Self {
        Self { endpoint }
    }

    pub fn endpoint(&self) -> &HttpEndpointConfig {
        &self.endpoint
    }
}

#[async_trait]
impl MjpegFrameSource for SystemMjpegFrameSource {
    async fn latest_frame(&self, control: &ExecutionControl) -> DriverResult<MjpegFrame> {
        run_controlled(
            control,
            self.endpoint.request_timeout_ms(),
            "mjpeg_transport_timeout",
            read_one_frame(&self.endpoint),
        )
        .await
    }
}

async fn read_one_frame(endpoint: &HttpEndpointConfig) -> DriverResult<MjpegFrame> {
    let mut stream = TcpStream::connect((endpoint.host(), endpoint.port()))
        .await
        .map_err(|_| platform("mjpeg_connect_failed", true))?;
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: multipart/x-mixed-replace\r\nConnection: close\r\n\r\n",
        endpoint.request_path(),
        endpoint.authority()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|_| platform("mjpeg_write_failed", true))?;
    let (status, headers, mut buffered) = read_http_head(&mut stream, "mjpeg_read_failed").await?;
    if !(200..300).contains(&status) {
        return Err(platform("mjpeg_http_status", status >= 500));
    }
    let boundary = multipart_boundary(
        headers
            .get("content-type")
            .ok_or_else(|| platform("mjpeg_invalid_content_type", false))?,
    )?;
    let delimiter = [b"--".as_slice(), boundary.as_bytes()].concat();
    let boundary_at = loop {
        if let Some(index) = find_subslice(&buffered, &delimiter) {
            break index;
        }
        if buffered.len() >= MAX_MJPEG_PREAMBLE_BYTES {
            return Err(platform("mjpeg_boundary_not_found", false));
        }
        read_more_bounded(
            &mut stream,
            &mut buffered,
            MAX_MJPEG_PREAMBLE_BYTES,
            "mjpeg_read_failed",
        )
        .await?;
    };
    let part_start = boundary_at + delimiter.len();
    while buffered.len() < part_start + 2 {
        read_more_bounded(
            &mut stream,
            &mut buffered,
            MAX_MJPEG_PREAMBLE_BYTES + MAX_MJPEG_PART_HEADERS_BYTES,
            "mjpeg_read_failed",
        )
        .await?;
    }
    if &buffered[part_start..part_start + 2] != b"\r\n" {
        return Err(platform("mjpeg_invalid_boundary", false));
    }
    let headers_start = part_start + 2;
    let headers_end = loop {
        if let Some(index) = find_subslice(&buffered[headers_start..], b"\r\n\r\n") {
            break headers_start + index;
        }
        if buffered.len().saturating_sub(headers_start) >= MAX_MJPEG_PART_HEADERS_BYTES {
            return Err(platform("mjpeg_part_headers_too_large", false));
        }
        read_more_bounded(
            &mut stream,
            &mut buffered,
            headers_start + MAX_MJPEG_PART_HEADERS_BYTES,
            "mjpeg_read_failed",
        )
        .await?;
    };
    let part_headers = parse_part_headers(&buffered[headers_start..headers_end])?;
    if !part_headers
        .get("content-type")
        .is_some_and(|value| value.eq_ignore_ascii_case("image/jpeg"))
    {
        return Err(platform("mjpeg_invalid_part_type", false));
    }
    let content_length = part_headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|length| (1..=MAX_JPEG_BYTES).contains(length))
        .ok_or_else(|| platform("mjpeg_invalid_content_length", false))?;
    let frame_start = headers_end + 4;
    let frame_end = frame_start
        .checked_add(content_length)
        .ok_or_else(|| platform("mjpeg_invalid_content_length", false))?;
    while buffered.len() < frame_end {
        read_more_bounded(&mut stream, &mut buffered, frame_end, "mjpeg_read_failed").await?;
    }
    MjpegFrame::from_jpeg(buffered[frame_start..frame_end].to_vec())
}

fn multipart_boundary(content_type: &str) -> DriverResult<String> {
    let mut parts = content_type.split(';');
    if !parts.next().is_some_and(|value| {
        value
            .trim()
            .eq_ignore_ascii_case("multipart/x-mixed-replace")
    }) {
        return Err(platform("mjpeg_invalid_content_type", false));
    }
    let boundary = parts.find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.eq_ignore_ascii_case("boundary")
            .then_some(value.trim())
    });
    let boundary = boundary
        .map(|value| value.trim_matches('"').trim_start_matches("--"))
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_BOUNDARY_BYTES
                && value.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric()
                        || matches!(
                            byte,
                            b'\''
                                | b'('
                                | b')'
                                | b'+'
                                | b'_'
                                | b','
                                | b'-'
                                | b'.'
                                | b'/'
                                | b':'
                                | b'='
                                | b'?'
                        )
                })
        })
        .ok_or_else(|| platform("mjpeg_invalid_boundary", false))?;
    Ok(boundary.to_owned())
}

fn parse_part_headers(bytes: &[u8]) -> DriverResult<BTreeMap<String, String>> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| platform("mjpeg_invalid_part_headers", false))?;
    let mut headers = BTreeMap::new();
    for line in text.split("\r\n") {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| platform("mjpeg_invalid_part_headers", false))?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_owned();
        if name.is_empty() || headers.insert(name, value).is_some() {
            return Err(platform("mjpeg_invalid_part_headers", false));
        }
    }
    Ok(headers)
}

fn validate_jpeg(bytes: &[u8]) -> DriverResult<(u32, u32)> {
    if bytes.len() < 4
        || bytes.len() > MAX_JPEG_BYTES
        || !bytes.starts_with(&[0xff, 0xd8])
        || !bytes.ends_with(&[0xff, 0xd9])
    {
        return Err(platform("mjpeg_invalid_jpeg", false));
    }
    let mut cursor = 2_usize;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] != 0xff {
            return Err(platform("mjpeg_invalid_jpeg", false));
        }
        while cursor < bytes.len() && bytes[cursor] == 0xff {
            cursor += 1;
        }
        let marker = *bytes
            .get(cursor)
            .ok_or_else(|| platform("mjpeg_invalid_jpeg", false))?;
        cursor += 1;
        if marker == 0xd9 {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = bytes
            .get(cursor..cursor + 2)
            .map(|pair| usize::from(u16::from_be_bytes([pair[0], pair[1]])))
            .filter(|length| *length >= 2)
            .ok_or_else(|| platform("mjpeg_invalid_jpeg", false))?;
        let segment_end = cursor
            .checked_add(length)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| platform("mjpeg_invalid_jpeg", false))?;
        if matches!(
            marker,
            0xc0 | 0xc1
                | 0xc2
                | 0xc3
                | 0xc5
                | 0xc6
                | 0xc7
                | 0xc9
                | 0xca
                | 0xcb
                | 0xcd
                | 0xce
                | 0xcf
        ) {
            let segment = &bytes[cursor + 2..segment_end];
            if segment.len() < 5 {
                return Err(platform("mjpeg_invalid_jpeg", false));
            }
            let height = u32::from(u16::from_be_bytes([segment[1], segment[2]]));
            let width = u32::from(u16::from_be_bytes([segment[3], segment[4]]));
            let pixels = u64::from(width) * u64::from(height);
            if width == 0
                || height == 0
                || width > MAX_JPEG_DIMENSION
                || height > MAX_JPEG_DIMENSION
                || pixels > MAX_JPEG_PIXELS
            {
                return Err(platform("mjpeg_invalid_jpeg", false));
            }
            return Ok((width, height));
        }
        if marker == 0xda {
            break;
        }
        cursor = segment_end;
    }
    Err(platform("mjpeg_invalid_jpeg", false))
}

#[cfg(test)]
mod tests {
    use devicerail_core::ExecutionControl;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::{MjpegFrame, MjpegFrameSource, SystemMjpegFrameSource, multipart_boundary};
    use crate::HttpEndpointConfig;

    fn one_pixel_jpeg() -> Vec<u8> {
        vec![
            0xff, 0xd8, // SOI
            0xff, 0xc0, 0x00, 0x0b, 0x08, 0x00, 0x01, 0x00, 0x01, 0x01, 0x01, 0x11,
            0x00, // SOF0
            0xff, 0xd9, // EOI (enough for structural dimension validation)
        ]
    }

    #[test]
    fn boundary_parser_handles_wda_style_prefix() {
        assert_eq!(
            multipart_boundary("multipart/x-mixed-replace; boundary=--BoundaryString")
                .expect("boundary"),
            "BoundaryString"
        );
        assert!(multipart_boundary("image/jpeg").is_err());
        assert!(multipart_boundary("multipart/x-mixed-replace; boundary=bad space").is_err());
    }

    #[test]
    fn jpeg_frame_requires_bounded_dimensions() {
        let frame = MjpegFrame::from_jpeg(one_pixel_jpeg()).expect("frame");
        assert_eq!((frame.width(), frame.height()), (1, 1));
        assert!(MjpegFrame::from_jpeg(vec![0xff, 0xd8, 0xff, 0xd9]).is_err());
    }

    #[tokio::test]
    async fn system_source_reads_exactly_one_multipart_frame() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let jpeg = one_pixel_jpeg();
        let expected = jpeg.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut request = [0_u8; 1024];
            let read = socket.read(&mut request).await.expect("request");
            assert!(
                std::str::from_utf8(&request[..read])
                    .expect("HTTP request")
                    .starts_with("GET /stream HTTP/1.1\r\n")
            );
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=--frame\r\n\r\n--frame\r\nContent-Type: image/jpeg\r\n",
                )
                .await
                .expect("first response chunk");
            socket
                .write_all(format!("Content-Length: {}\r\n\r\n", jpeg.len()).as_bytes())
                .await
                .expect("part length");
            socket.write_all(&jpeg).await.expect("JPEG");
            socket
                .write_all(b"\r\n--frame\r\n")
                .await
                .expect("next boundary");
        });
        let source = SystemMjpegFrameSource::new(
            HttpEndpointConfig::new(format!("http://{address}/stream")).expect("endpoint"),
        );
        let frame = source
            .latest_frame(&ExecutionControl::unbounded())
            .await
            .expect("frame");
        assert_eq!(frame.bytes(), expected);
        server.await.expect("server task");
    }
}
