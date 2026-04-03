use std::{
    future::Future,
    marker::PhantomData,
    sync::{Arc, Mutex},
};

use futures::{SinkExt, StreamExt};
use rmcp::{
    service::{RoleServer, RxJsonRpcMessage, ServiceRole, TxJsonRpcMessage},
    transport::Transport,
};
use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Mutex as AsyncMutex,
};
use tokio_util::{
    bytes::{Buf, BufMut, BytesMut},
    codec::{Decoder, Encoder, FramedRead, FramedWrite},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WireProtocol {
    JsonLine,
    ContentLength,
}

#[derive(Debug, Clone)]
struct SharedProtocol(Arc<Mutex<Option<WireProtocol>>>);

impl SharedProtocol {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    fn get(&self) -> Option<WireProtocol> {
        *self.0.lock().expect("protocol mutex poisoned")
    }

    fn set_if_unset(&self, protocol: WireProtocol) {
        let mut guard = self.0.lock().expect("protocol mutex poisoned");
        if guard.is_none() {
            *guard = Some(protocol);
        }
    }
}

pub type TransportWriter<Role, W> =
    FramedWrite<W, HybridJsonRpcMessageCodec<TxJsonRpcMessage<Role>>>;

pub struct HybridStdioTransport<Role: ServiceRole, R: AsyncRead, W: AsyncWrite> {
    read: FramedRead<R, HybridJsonRpcMessageCodec<RxJsonRpcMessage<Role>>>,
    write: Arc<AsyncMutex<Option<TransportWriter<Role, W>>>>,
}

impl<Role: ServiceRole, R, W> HybridStdioTransport<Role, R, W>
where
    R: Send + AsyncRead + Unpin,
    W: Send + AsyncWrite + Unpin + 'static,
{
    pub fn new(read: R, write: W) -> Self {
        let protocol = SharedProtocol::new();
        let read = FramedRead::new(
            read,
            HybridJsonRpcMessageCodec::<RxJsonRpcMessage<Role>>::new(protocol.clone()),
        );
        let write = Arc::new(AsyncMutex::new(Some(FramedWrite::new(
            write,
            HybridJsonRpcMessageCodec::<TxJsonRpcMessage<Role>>::new(protocol),
        ))));
        Self { read, write }
    }
}

impl<R, W> HybridStdioTransport<RoleServer, R, W>
where
    R: Send + AsyncRead + Unpin,
    W: Send + AsyncWrite + Unpin + 'static,
{
    pub fn new_server(read: R, write: W) -> Self {
        Self::new(read, write)
    }
}

impl<Role: ServiceRole, R, W> Transport<Role> for HybridStdioTransport<Role, R, W>
where
    R: Send + AsyncRead + Unpin,
    W: Send + AsyncWrite + Unpin + 'static,
{
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<Role>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let lock = self.write.clone();
        async move {
            let mut write = lock.lock().await;
            if let Some(ref mut write) = *write {
                write.send(item).await.map_err(Into::into)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "Transport is closed",
                ))
            }
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<Role>>> + Send {
        let next = self.read.next();
        async {
            next.await.and_then(|result| {
                result
                    .inspect_err(|error| {
                        tracing::error!("Error reading from stream: {}", error);
                    })
                    .ok()
            })
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        let mut write = self.write.lock().await;
        drop(write.take());
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct HybridJsonRpcMessageCodec<T> {
    _marker: PhantomData<fn() -> T>,
    next_index: usize,
    max_length: usize,
    is_discarding: bool,
    protocol: SharedProtocol,
}

impl<T> HybridJsonRpcMessageCodec<T> {
    fn new(protocol: SharedProtocol) -> Self {
        Self {
            _marker: PhantomData,
            next_index: 0,
            max_length: usize::MAX,
            is_discarding: false,
            protocol,
        }
    }
}

fn without_carriage_return(s: &[u8]) -> &[u8] {
    if let Some(&b'\r') = s.last() {
        &s[..s.len() - 1]
    } else {
        s
    }
}

fn is_standard_method(method: &str) -> bool {
    matches!(
        method,
        "initialize"
            | "ping"
            | "prompts/get"
            | "prompts/list"
            | "resources/list"
            | "resources/read"
            | "resources/subscribe"
            | "resources/unsubscribe"
            | "resources/templates/list"
            | "tools/call"
            | "tools/list"
            | "completion/complete"
            | "logging/setLevel"
            | "roots/list"
            | "sampling/createMessage"
    ) || is_standard_notification(method)
}

fn is_standard_notification(method: &str) -> bool {
    matches!(
        method,
        "notifications/cancelled"
            | "notifications/initialized"
            | "notifications/message"
            | "notifications/progress"
            | "notifications/prompts/list_changed"
            | "notifications/resources/list_changed"
            | "notifications/resources/updated"
            | "notifications/roots/list_changed"
            | "notifications/tools/list_changed"
    )
}

fn should_ignore_notification(json_value: &serde_json::Value, method: &str) -> bool {
    let is_notification = json_value.get("id").is_none();
    if is_notification && !is_standard_method(method) {
        tracing::trace!(
            "Ignoring non-MCP notification '{}' for compatibility",
            method
        );
        return true;
    }

    matches!(
        (
            method.starts_with("notifications/"),
            is_standard_notification(method)
        ),
        (true, false)
    )
}

fn try_parse_with_compatibility<T: DeserializeOwned>(
    payload: &[u8],
    context: &str,
) -> Result<Option<T>, HybridCodecError> {
    if let Ok(line_str) = std::str::from_utf8(payload) {
        match serde_json::from_slice(payload) {
            Ok(item) => Ok(Some(item)),
            Err(error) => {
                if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(line_str) {
                    if let Some(method) =
                        json_value.get("method").and_then(serde_json::Value::as_str)
                    {
                        if should_ignore_notification(&json_value, method) {
                            return Ok(None);
                        }
                    }
                }

                tracing::debug!(
                    "Failed to parse message {}: {} | Error: {}",
                    context,
                    line_str,
                    error
                );
                Err(HybridCodecError::Serde(error))
            }
        }
    } else {
        serde_json::from_slice(payload)
            .map(Some)
            .map_err(HybridCodecError::Serde)
    }
}

#[derive(Debug, Error)]
pub enum HybridCodecError {
    #[error("max line length exceeded")]
    MaxLineLengthExceeded,
    #[error("missing Content-Length header")]
    MissingContentLength,
    #[error("invalid Content-Length value: {0}")]
    InvalidContentLength(String),
    #[error("invalid header frame: {0}")]
    InvalidHeaderFrame(String),
    #[error("serde error {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error {0}")]
    Io(#[from] std::io::Error),
}

impl From<HybridCodecError> for std::io::Error {
    fn from(value: HybridCodecError) -> Self {
        match value {
            HybridCodecError::MaxLineLengthExceeded
            | HybridCodecError::MissingContentLength
            | HybridCodecError::InvalidContentLength(_)
            | HybridCodecError::InvalidHeaderFrame(_) => {
                std::io::Error::new(std::io::ErrorKind::InvalidData, value)
            }
            HybridCodecError::Serde(error) => error.into(),
            HybridCodecError::Io(error) => error,
        }
    }
}

fn looks_like_content_length_frame(buf: &BytesMut) -> bool {
    let prefix = &buf[..buf.len().min(32)];
    prefix
        .windows(b"content-length".len())
        .next()
        .map(|candidate| candidate.eq_ignore_ascii_case(b"content-length"))
        .unwrap_or(false)
}

fn find_header_terminator(buf: &BytesMut) -> Option<(usize, usize)> {
    if let Some(index) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some((index, 4));
    }
    buf.windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2))
}

fn parse_content_length(header: &str) -> Result<usize, HybridCodecError> {
    for raw_line in header.lines() {
        let line = raw_line.trim_end_matches('\r');
        let (name, value) = match line.split_once(':') {
            Some(parts) => parts,
            None => continue,
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|_| HybridCodecError::InvalidContentLength(value.trim().to_string()));
        }
    }

    Err(HybridCodecError::MissingContentLength)
}

impl<T: DeserializeOwned> HybridJsonRpcMessageCodec<T> {
    fn decode_content_length(&mut self, buf: &mut BytesMut) -> Result<Option<T>, HybridCodecError> {
        let Some((header_end, delimiter_len)) = find_header_terminator(buf) else {
            return Ok(None);
        };

        let header = std::str::from_utf8(&buf[..header_end])
            .map_err(|error| HybridCodecError::InvalidHeaderFrame(error.to_string()))?;
        let content_length = parse_content_length(header)?;
        let body_start = header_end + delimiter_len;
        let frame_len = body_start + content_length;
        if buf.len() < frame_len {
            return Ok(None);
        }

        let frame = buf.split_to(frame_len);
        let payload = &frame[body_start..];
        self.protocol.set_if_unset(WireProtocol::ContentLength);

        try_parse_with_compatibility(payload, "decode_content_length")
    }

    fn decode_json_line(&mut self, buf: &mut BytesMut) -> Result<Option<T>, HybridCodecError> {
        loop {
            let read_to = std::cmp::min(self.max_length.saturating_add(1), buf.len());
            let newline_offset = buf[self.next_index..read_to]
                .iter()
                .position(|byte| *byte == b'\n');

            match (self.is_discarding, newline_offset) {
                (true, Some(offset)) => {
                    buf.advance(offset + self.next_index + 1);
                    self.is_discarding = false;
                    self.next_index = 0;
                }
                (true, None) => {
                    buf.advance(read_to);
                    self.next_index = 0;
                    if buf.is_empty() {
                        return Ok(None);
                    }
                }
                (false, Some(offset)) => {
                    let newline_index = offset + self.next_index;
                    self.next_index = 0;
                    let line = buf.split_to(newline_index + 1);
                    let line = &line[..line.len() - 1];
                    let payload = without_carriage_return(line);
                    self.protocol.set_if_unset(WireProtocol::JsonLine);

                    if let Some(item) = try_parse_with_compatibility(payload, "decode_json_line")? {
                        return Ok(Some(item));
                    }
                }
                (false, None) if buf.len() > self.max_length => {
                    self.is_discarding = true;
                    return Err(HybridCodecError::MaxLineLengthExceeded);
                }
                (false, None) => {
                    self.next_index = read_to;
                    return Ok(None);
                }
            }
        }
    }
}

impl<T: DeserializeOwned> Decoder for HybridJsonRpcMessageCodec<T> {
    type Item = T;
    type Error = HybridCodecError;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<T>, HybridCodecError> {
        match self.protocol.get() {
            Some(WireProtocol::ContentLength) => self.decode_content_length(buf),
            Some(WireProtocol::JsonLine) => self.decode_json_line(buf),
            None => {
                if looks_like_content_length_frame(buf) {
                    self.decode_content_length(buf)
                } else {
                    self.decode_json_line(buf)
                }
            }
        }
    }

    fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<T>, HybridCodecError> {
        match self.protocol.get() {
            Some(WireProtocol::ContentLength) if !buf.is_empty() => self.decode_content_length(buf),
            _ => Ok(match self.decode(buf)? {
                Some(frame) => Some(frame),
                None => {
                    self.next_index = 0;
                    if buf.is_empty() || buf == &b"\r"[..] {
                        None
                    } else {
                        let line = buf.split_to(buf.len());
                        let payload = without_carriage_return(&line);
                        try_parse_with_compatibility(payload, "decode_eof")?
                    }
                }
            }),
        }
    }
}

impl<T: Serialize> Encoder<T> for HybridJsonRpcMessageCodec<T> {
    type Error = HybridCodecError;

    fn encode(&mut self, item: T, buf: &mut BytesMut) -> Result<(), HybridCodecError> {
        let payload = serde_json::to_vec(&item)?;

        match self.protocol.get().unwrap_or(WireProtocol::ContentLength) {
            WireProtocol::ContentLength => {
                buf.extend_from_slice(
                    format!("Content-Length: {}\r\n\r\n", payload.len()).as_bytes(),
                );
                buf.extend_from_slice(&payload);
            }
            WireProtocol::JsonLine => {
                buf.extend_from_slice(&payload);
                buf.put_u8(b'\n');
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio_util::bytes::BytesMut;

    fn sample_message() -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "probe",
                    "version": "0.0.0"
                }
            }
        })
    }

    #[test]
    fn decodes_json_line_and_marks_protocol() {
        let protocol = SharedProtocol::new();
        let mut codec = HybridJsonRpcMessageCodec::<serde_json::Value>::new(protocol.clone());
        let payload = serde_json::to_vec(&sample_message()).unwrap();
        let mut buf = BytesMut::from(&payload[..]);
        buf.put_u8(b'\n');

        let item = codec.decode(&mut buf).unwrap();
        assert!(item.is_some());
        assert_eq!(protocol.get(), Some(WireProtocol::JsonLine));
    }

    #[test]
    fn decodes_content_length_and_marks_protocol() {
        let protocol = SharedProtocol::new();
        let mut codec = HybridJsonRpcMessageCodec::<serde_json::Value>::new(protocol.clone());
        let payload = serde_json::to_vec(&sample_message()).unwrap();
        let mut frame = BytesMut::new();
        frame.extend_from_slice(format!("Content-Length: {}\r\n\r\n", payload.len()).as_bytes());
        frame.extend_from_slice(&payload);

        let item = codec.decode(&mut frame).unwrap();
        assert!(item.is_some());
        assert_eq!(protocol.get(), Some(WireProtocol::ContentLength));
    }

    #[test]
    fn encodes_using_content_length_when_protocol_is_detected() {
        let protocol = SharedProtocol::new();
        protocol.set_if_unset(WireProtocol::ContentLength);
        let mut codec = HybridJsonRpcMessageCodec::<serde_json::Value>::new(protocol);
        let mut buf = BytesMut::new();
        codec
            .encode(
                serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"ok":true}}),
                &mut buf,
            )
            .unwrap();

        assert!(std::str::from_utf8(&buf)
            .unwrap()
            .starts_with("Content-Length: "));
    }
}
