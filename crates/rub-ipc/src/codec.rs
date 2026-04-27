use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// NDJSON (Newline-Delimited JSON) codec.
///
/// Framing: each message is a single JSON object followed by `\n`.
/// No length prefix. Parser reads until `\n`, then deserializes.
///
/// The trailing newline is the transport commit fence. A payload that never
/// reaches `\n` must be treated as incomplete transport state rather than a
/// successfully framed protocol message.
pub struct NdJsonCodec;

/// Maximum on-wire NDJSON frame size, including the trailing newline commit fence.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
struct NdJsonFrameTooLargeError {
    max_frame_bytes: usize,
}

impl std::fmt::Display for NdJsonFrameTooLargeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "NDJSON frame exceeds maximum on-wire size of {} bytes",
            self.max_frame_bytes
        )
    }
}

impl std::error::Error for NdJsonFrameTooLargeError {}

/// Compute the on-wire frame size for an already serialized JSON payload.
pub fn encoded_frame_len_from_payload_len(payload_json_len: usize) -> usize {
    payload_json_len.saturating_add(1)
}

/// Compute the on-wire NDJSON frame size for a serializable value, including
/// the newline commit fence.
pub fn encoded_frame_len<T: Serialize>(value: &T) -> Result<usize, serde_json::Error> {
    Ok(encoded_frame_len_from_payload_len(
        serde_json::to_vec(value)?.len(),
    ))
}

/// Construct the canonical typed IO error used when a frame exceeds the codec's
/// maximum on-wire budget.
pub fn oversized_frame_io_error(max_frame_bytes: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        NdJsonFrameTooLargeError { max_frame_bytes },
    )
}

/// Detect whether an IO error came from this codec's oversized-frame fence.
pub fn is_oversized_frame_io_error(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::InvalidData
        && error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<NdJsonFrameTooLargeError>())
            .is_some()
}

impl NdJsonCodec {
    /// Encode a value to NDJSON bytes (JSON + newline).
    pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
        let mut bytes = serde_json::to_vec(value)?;
        ensure_payload_within_limit(bytes.len()).map_err(serde_json::Error::io)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Write a value as NDJSON to an async writer.
    pub async fn write<T: Serialize, W: tokio::io::AsyncWrite + Unpin>(
        writer: &mut W,
        value: &T,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = Self::encode(value)?;
        writer.write_all(&bytes).await?;
        writer.flush().await?;
        Ok(())
    }

    /// Read one NDJSON message from an async reader.
    /// Returns `None` on EOF.
    pub async fn read<T: DeserializeOwned, R: tokio::io::AsyncRead + Unpin>(
        reader: &mut BufReader<R>,
    ) -> Result<Option<T>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(frame) = Self::read_frame_bytes(reader).await? else {
            return Ok(None);
        };
        let value = serde_json::from_slice(&frame)?;
        Ok(Some(value))
    }

    /// Read one committed NDJSON frame as raw JSON bytes without attempting to
    /// decode it into a typed value. This is for owner layers that must retain
    /// framed request bytes to recover protocol correlation on downstream
    /// decode failure.
    pub async fn read_frame_bytes<R: tokio::io::AsyncRead + Unpin>(
        reader: &mut BufReader<R>,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        Self::read_committed_frame(reader).await
    }

    /// Read one NDJSON message from a blocking reader using the same frame-size
    /// fence as the async transport.
    pub fn read_blocking<T: DeserializeOwned, R: std::io::BufRead>(
        reader: &mut R,
    ) -> Result<Option<T>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(frame) = Self::read_frame_blocking(reader)? else {
            return Ok(None);
        };
        let value = serde_json::from_slice(&frame)?;
        Ok(Some(value))
    }

    async fn read_committed_frame<R: tokio::io::AsyncRead + Unpin>(
        reader: &mut BufReader<R>,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let mut frame = Vec::new();

        loop {
            let (consume_len, done) = {
                let chunk = reader.fill_buf().await?;
                if chunk.is_empty() {
                    if frame.is_empty() {
                        return Ok(None);
                    }
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "NDJSON frame terminated before newline commit fence",
                    )
                    .into());
                }

                if let Some(newline_pos) = chunk.iter().position(|byte| *byte == b'\n') {
                    let slice = &chunk[..newline_pos];
                    ensure_payload_within_limit(frame.len() + slice.len())?;
                    frame.extend_from_slice(slice);
                    (newline_pos + 1, true)
                } else {
                    ensure_payload_within_limit(frame.len() + chunk.len())?;
                    frame.extend_from_slice(chunk);
                    (chunk.len(), false)
                }
            };

            reader.consume(consume_len);
            if done {
                break;
            }
        }

        Ok(Some(frame))
    }

    fn read_frame_blocking<R: std::io::BufRead>(
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let mut frame = Vec::new();

        loop {
            let (consume_len, done) = {
                let chunk = reader.fill_buf()?;
                if chunk.is_empty() {
                    if frame.is_empty() {
                        return Ok(None);
                    }
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "NDJSON frame terminated before newline commit fence",
                    )
                    .into());
                }

                if let Some(newline_pos) = chunk.iter().position(|byte| *byte == b'\n') {
                    let slice = &chunk[..newline_pos];
                    ensure_payload_within_limit(frame.len() + slice.len())?;
                    frame.extend_from_slice(slice);
                    (newline_pos + 1, true)
                } else {
                    ensure_payload_within_limit(frame.len() + chunk.len())?;
                    frame.extend_from_slice(chunk);
                    (chunk.len(), false)
                }
            };

            reader.consume(consume_len);
            if done {
                break;
            }
        }

        Ok(Some(frame))
    }
}

fn ensure_payload_within_limit(payload_json_len: usize) -> Result<(), std::io::Error> {
    if encoded_frame_len_from_payload_len(payload_json_len) > MAX_FRAME_BYTES {
        return Err(oversized_frame_io_error(MAX_FRAME_BYTES));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::io::Cursor;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct TestMsg {
        foo: String,
        bar: u32,
    }

    fn frame_len_for_foo_len(foo_len: usize) -> usize {
        encoded_frame_len(&TestMsg {
            foo: "a".repeat(foo_len),
            bar: 1,
        })
        .expect("frame length should serialize")
    }

    fn max_fitting_msg() -> TestMsg {
        let mut low = 0usize;
        let mut high = MAX_FRAME_BYTES;
        while low < high {
            let mid = (low + high).div_ceil(2);
            if frame_len_for_foo_len(mid) <= MAX_FRAME_BYTES {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        let msg = TestMsg {
            foo: "a".repeat(low),
            bar: 1,
        };
        assert_eq!(
            encoded_frame_len(&msg).expect("frame length"),
            MAX_FRAME_BYTES
        );
        msg
    }

    #[test]
    fn encode_produces_ndjson() {
        let msg = TestMsg {
            foo: "hello".into(),
            bar: 42,
        };
        let bytes = NdJsonCodec::encode(&msg).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.ends_with('\n'));
        assert_eq!(s.matches('\n').count(), 1);
        let decoded: TestMsg = serde_json::from_str(s.trim()).unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn write_read_roundtrip() {
        let msg = TestMsg {
            foo: "world".into(),
            bar: 99,
        };

        // Write to buffer
        let mut buf = Vec::new();
        NdJsonCodec::write(&mut buf, &msg).await.unwrap();

        // Read back from the written bytes
        let mut reader = BufReader::new(buf.as_slice());
        let decoded: TestMsg = NdJsonCodec::read(&mut reader).await.unwrap().unwrap();
        assert_eq!(decoded, msg);
    }

    #[tokio::test]
    async fn read_returns_none_on_eof() {
        let empty: &[u8] = &[];
        let mut reader = BufReader::new(empty);
        let result: Option<TestMsg> = NdJsonCodec::read(&mut reader).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn read_rejects_frames_larger_than_limit() {
        let fitting = max_fitting_msg();
        let oversized = format!("{{\"foo\":\"{}a\",\"bar\":1}}\n", fitting.foo);
        let mut reader = BufReader::new(oversized.as_bytes());

        let error = NdJsonCodec::read::<TestMsg, _>(&mut reader)
            .await
            .expect_err("oversized frame should fail");
        let io_error = error
            .downcast::<std::io::Error>()
            .expect("oversized frame should surface io error");
        assert!(is_oversized_frame_io_error(&io_error), "{io_error}");
    }

    #[tokio::test]
    async fn read_rejects_partial_frame_without_newline() {
        let partial = br#"{"foo":"partial","bar":1}"#;
        let mut reader = BufReader::new(partial.as_slice());

        let error = NdJsonCodec::read::<TestMsg, _>(&mut reader)
            .await
            .expect_err("partial frame should fail");
        assert!(
            error
                .to_string()
                .contains("NDJSON frame terminated before newline"),
            "{error}"
        );
    }

    #[test]
    fn encode_rejects_frames_larger_than_limit() {
        let fitting = max_fitting_msg();
        let msg = TestMsg {
            foo: format!("{}a", fitting.foo),
            bar: 1,
        };
        assert_eq!(
            encoded_frame_len(&msg).expect("frame length"),
            MAX_FRAME_BYTES + 1
        );
        let error = NdJsonCodec::encode(&msg).expect_err("oversized encode should fail");
        assert_eq!(error.io_error_kind(), Some(std::io::ErrorKind::InvalidData));
        assert!(
            error.to_string().contains("maximum on-wire size"),
            "{error}"
        );
    }

    #[test]
    fn encode_accepts_exact_on_wire_boundary() {
        let msg = max_fitting_msg();
        let encoded = NdJsonCodec::encode(&msg).expect("exact boundary should succeed");
        assert_eq!(encoded.len(), MAX_FRAME_BYTES);
    }

    #[test]
    fn blocking_read_rejects_frames_larger_than_limit() {
        let fitting = max_fitting_msg();
        let oversized = format!("{{\"foo\":\"{}a\",\"bar\":1}}\n", fitting.foo);
        let mut reader = Cursor::new(oversized.into_bytes());

        let error = NdJsonCodec::read_blocking::<TestMsg, _>(&mut reader)
            .expect_err("oversized blocking frame should fail");
        let io_error = error
            .downcast::<std::io::Error>()
            .expect("oversized blocking frame should surface io error");
        assert!(is_oversized_frame_io_error(&io_error), "{io_error}");
    }
}
