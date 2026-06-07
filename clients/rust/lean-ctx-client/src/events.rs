//! Blocking Server-Sent Events stream for `GET /v1/events`.
//!
//! [`EventStream`] is an [`Iterator`] over [`ContextEventV1`]: it reads SSE
//! frames off the response body, joins `data:` lines, and JSON-decodes each
//! frame. I/O failures surface as `Err`; non-event frames (comments,
//! heartbeats, unparseable payloads) are skipped — matching the TypeScript SDK.

use std::io::{BufRead, BufReader, Read};

use crate::error::{LeanCtxError, Result};
use crate::types::ContextEventV1;

/// A lazily-consumed stream of context events.
///
/// The stream lives as long as the underlying HTTP connection. Dropping it
/// closes the connection. Iteration is blocking.
pub struct EventStream {
    reader: BufReader<Box<dyn Read + Send + Sync + 'static>>,
    line: String,
}

impl EventStream {
    pub(crate) fn new(reader: Box<dyn Read + Send + Sync + 'static>) -> Self {
        Self {
            reader: BufReader::new(reader),
            line: String::new(),
        }
    }
}

impl Iterator for EventStream {
    type Item = Result<ContextEventV1>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut frame = String::new();
        loop {
            self.line.clear();
            match self.reader.read_line(&mut self.line) {
                Ok(0) => return take_event(&frame).map(Ok),
                Ok(_) => {}
                Err(e) => {
                    return Some(Err(LeanCtxError::Decode {
                        method: "GET".into(),
                        url: "/v1/events".into(),
                        message: e.to_string(),
                    }))
                }
            }

            let trimmed = self.line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                if let Some(ev) = take_event(&frame) {
                    return Some(Ok(ev));
                }
                frame.clear();
                continue;
            }
            frame.push_str(trimmed);
            frame.push('\n');
        }
    }
}

/// Decode a complete SSE frame into an event, or `None` when the frame carries
/// no decodable event (comment-only, heartbeat, or malformed payload).
fn take_event(frame: &str) -> Option<ContextEventV1> {
    let data = parse_sse_data(frame)?;
    serde_json::from_str::<ContextEventV1>(&data).ok()
}

/// Join the `data:` field(s) of an SSE frame, ignoring comments and other
/// fields (`id:`, `event:`). Returns `None` when the frame has no data.
fn parse_sse_data(frame: &str) -> Option<String> {
    let mut data: Vec<&str> = Vec::new();
    for line in frame.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if data.is_empty() {
        None
    } else {
        Some(data.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_data_and_ignores_other_fields() {
        let frame = ":comment\nid: 7\nevent: ctx\ndata: {\"a\":1}\n";
        assert_eq!(parse_sse_data(frame).as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn joins_multiline_data() {
        let frame = "data: line1\ndata: line2\n";
        assert_eq!(parse_sse_data(frame).as_deref(), Some("line1\nline2"));
    }

    #[test]
    fn comment_only_frame_has_no_data() {
        assert_eq!(parse_sse_data(": keep-alive\n"), None);
    }

    #[test]
    fn streams_events_and_skips_heartbeats() {
        let body = "data: {\"id\":1,\"workspaceId\":\"w\",\"channelId\":\"c\",\"kind\":\"tool_call\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"consistencyLevel\":\"local\",\"payload\":{}}\n\n: heartbeat\n\ndata: {\"id\":2,\"workspaceId\":\"w\",\"channelId\":\"c\",\"kind\":\"session_update\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"consistencyLevel\":\"eventual\",\"payload\":{}}\n\n";
        let reader: Box<dyn Read + Send + Sync + 'static> = Box::new(Cursor::new(body));
        let events: Vec<_> = EventStream::new(reader).map(|r| r.unwrap()).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, 1);
        assert_eq!(events[0].kind, "tool_call");
        assert_eq!(events[1].id, 2);
        assert_eq!(events[1].consistency_level, "eventual");
    }
}
