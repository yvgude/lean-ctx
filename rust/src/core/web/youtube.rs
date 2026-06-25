//! `YouTube` transcript adapter (no API key required).
//!
//! Transcripts are obtained via `YouTube`'s **`InnerTube`** `player` endpoint — the
//! same internal API the official apps use. We POST an `ANDROID` client context
//! and read the `captionTracks` it returns; those caption URLs are
//! server-fetchable, unlike the session-bound ones embedded in the watch page
//! (which now return empty bodies for non-browser callers). The chosen track is
//! downloaded as JSON3 (with an srv3/XML fallback) and flattened to text.
//!
//! No Data API key is needed (that is only required for *search*/metadata, which
//! is configured through the config-provider layer). All requests still flow
//! through the SSRF-guarded [`super::fetch`].

use serde::Deserialize;

use super::{fetch, html_to_text, url_guard};

/// `InnerTube` `player` endpoint (same host the SSRF guard already permits).
const INNERTUBE_PLAYER: &str = "https://www.youtube.com/youtubei/v1/player";
/// Android client identity. `InnerTube` cross-checks the UA against `clientName`.
const ANDROID_CLIENT_VERSION: &str = "20.10.38";
const ANDROID_UA: &str = "com.google.android.youtube/20.10.38 (Linux; U; Android 14) gzip";

/// A flattened transcript ready for compression / distillation.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub video_id: String,
    pub title: Option<String>,
    pub source_url: String,
    pub full_text: String,
}

/// Extract a `YouTube` video id from common URL shapes, or `None`.
#[must_use]
pub fn video_id(url: &str) -> Option<String> {
    let safe = url_guard::validate(url).ok()?;
    let host = safe.host.to_ascii_lowercase();
    let (path, query) = split_path_query(path_and_query(&safe));

    if host == "youtu.be" || host.ends_with(".youtu.be") {
        return clean_id(path.trim_start_matches('/'));
    }
    if host == "youtube.com" || host.ends_with(".youtube.com") {
        if path.starts_with("/watch")
            && let Some(v) = query_param(query, "v")
        {
            return clean_id(&v);
        }
        for prefix in ["/shorts/", "/embed/", "/v/", "/live/"] {
            if let Some(rest) = path.strip_prefix(prefix) {
                return clean_id(rest);
            }
        }
    }
    None
}

/// Download and flatten the transcript for `video_id`.
pub fn fetch_transcript(video_id: &str, timeout_secs: u64) -> Result<Transcript, String> {
    let player = innertube_player(video_id, timeout_secs)?;

    let tracks = player.caption_tracks();
    if tracks.is_empty() {
        return Err(format!(
            "no captions available for video {video_id}{}",
            player.unavailable_reason()
        ));
    }

    let track = select_caption_track(&tracks);
    let url = json3_url(&track.base_url);
    let data = fetch::fetch(&url, fetch::DEFAULT_MAX_BYTES, timeout_secs)?;
    if data.status >= 400 {
        return Err(format!(
            "failed to download transcript (HTTP {})",
            data.status
        ));
    }

    let full_text = parse_timedtext(&data.body_text())?;
    if full_text.trim().is_empty() {
        return Err(format!("transcript for video {video_id} was empty"));
    }

    Ok(Transcript {
        video_id: video_id.to_string(),
        title: player.title(),
        source_url: format!("https://www.youtube.com/watch?v={video_id}"),
        full_text,
    })
}

// ── InnerTube player ───────────────────────────────────────────────────────

fn innertube_player(video_id: &str, timeout_secs: u64) -> Result<PlayerResponse, String> {
    let body = serde_json::json!({
        "context": {
            "client": {
                "clientName": "ANDROID",
                "clientVersion": ANDROID_CLIENT_VERSION,
                "androidSdkVersion": 34,
                "hl": "en"
            }
        },
        "videoId": video_id
    })
    .to_string();

    let resp = fetch::post(
        INNERTUBE_PLAYER,
        "application/json",
        ANDROID_UA,
        &body,
        fetch::DEFAULT_MAX_BYTES,
        timeout_secs,
    )?;
    if resp.status >= 400 {
        return Err(format!("InnerTube player returned HTTP {}", resp.status));
    }

    serde_json::from_str::<PlayerResponse>(&resp.body_text())
        .map_err(|e| format!("could not parse InnerTube player response: {e}"))
}

#[derive(Deserialize)]
struct PlayerResponse {
    captions: Option<CaptionsBlock>,
    #[serde(rename = "videoDetails")]
    video_details: Option<VideoDetails>,
    #[serde(rename = "playabilityStatus")]
    playability: Option<Playability>,
}

impl PlayerResponse {
    fn caption_tracks(&self) -> Vec<CaptionTrack> {
        self.captions
            .as_ref()
            .and_then(|c| c.renderer.as_ref())
            .map(|r| r.caption_tracks.clone())
            .unwrap_or_default()
    }

    fn title(&self) -> Option<String> {
        self.video_details
            .as_ref()
            .and_then(|v| v.title.clone())
            .filter(|t| !t.is_empty())
    }

    /// A human-readable reason suffix when no captions are present.
    fn unavailable_reason(&self) -> String {
        match self.playability.as_ref() {
            Some(p) if p.status.as_deref().is_some_and(|s| s != "OK") => {
                let status = p.status.as_deref().unwrap_or("");
                let reason = p.reason.as_deref().unwrap_or("");
                format!(
                    " ({status}{}{reason})",
                    if reason.is_empty() { "" } else { ": " }
                )
            }
            _ => " (captions disabled or none published)".to_string(),
        }
    }
}

#[derive(Deserialize)]
struct CaptionsBlock {
    #[serde(rename = "playerCaptionsTracklistRenderer")]
    renderer: Option<TracklistRenderer>,
}

#[derive(Deserialize)]
struct TracklistRenderer {
    #[serde(rename = "captionTracks", default)]
    caption_tracks: Vec<CaptionTrack>,
}

#[derive(Deserialize)]
struct VideoDetails {
    title: Option<String>,
}

#[derive(Deserialize)]
struct Playability {
    status: Option<String>,
    reason: Option<String>,
}

// ── Caption track selection ────────────────────────────────────────────────

#[derive(Deserialize, Clone)]
struct CaptionTrack {
    #[serde(rename = "baseUrl")]
    base_url: String,
    #[serde(rename = "languageCode")]
    language_code: Option<String>,
    kind: Option<String>,
}

impl CaptionTrack {
    fn is_english(&self) -> bool {
        self.language_code
            .as_deref()
            .is_some_and(|c| c.starts_with("en"))
    }

    fn is_auto_generated(&self) -> bool {
        self.kind.as_deref() == Some("asr")
    }
}

/// Prefer a manual English track, then any English, then any manual, else first.
/// The caller guarantees `tracks` is non-empty.
fn select_caption_track(tracks: &[CaptionTrack]) -> &CaptionTrack {
    tracks
        .iter()
        .find(|t| t.is_english() && !t.is_auto_generated())
        .or_else(|| tracks.iter().find(|t| t.is_english()))
        .or_else(|| tracks.iter().find(|t| !t.is_auto_generated()))
        .unwrap_or(&tracks[0])
}

/// Force the JSON3 caption format: drop any pre-set `fmt=` and request `json3`.
fn json3_url(base_url: &str) -> String {
    let stripped: String = base_url
        .split('&')
        .filter(|seg| !seg.starts_with("fmt="))
        .collect::<Vec<_>>()
        .join("&");
    format!("{stripped}&fmt=json3")
}

// ── Transcript parsing (JSON3 primary, srv3/XML fallback) ───────────────────

fn parse_timedtext(body: &str) -> Result<String, String> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('{') {
        parse_json3(body)
    } else if trimmed.starts_with('<') {
        Ok(parse_srv3_xml(body))
    } else {
        Err("transcript response was neither JSON3 nor srv3/XML".to_string())
    }
}

#[derive(Deserialize)]
struct Json3 {
    #[serde(default)]
    events: Vec<Json3Event>,
}

#[derive(Deserialize)]
struct Json3Event {
    #[serde(default)]
    segs: Vec<Json3Seg>,
}

#[derive(Deserialize)]
struct Json3Seg {
    #[serde(default)]
    utf8: String,
}

fn parse_json3(body: &str) -> Result<String, String> {
    let parsed: Json3 =
        serde_json::from_str(body).map_err(|e| format!("could not parse transcript json: {e}"))?;

    let mut out = String::new();
    for event in parsed.events {
        let line: String = event.segs.iter().map(|s| s.utf8.as_str()).collect();
        let line = line.replace('\n', " ");
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    Ok(out)
}

/// Flatten the srv3/XML timedtext format (`<p ...>text</p>`), entity-decoded.
fn parse_srv3_xml(xml: &str) -> String {
    let mut raw = String::with_capacity(xml.len() / 2);
    let mut in_tag = false;
    let mut tag = String::new();
    for c in xml.chars() {
        match c {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = false;
                // A closing paragraph marks a caption-line boundary.
                if tag.starts_with("/p") {
                    raw.push('\n');
                }
            }
            _ if in_tag => tag.push(c),
            _ => raw.push(c),
        }
    }

    let decoded = html_to_text::decode_entities(&raw);
    let mut out = String::new();
    for line in decoded.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    out
}

// ── Small URL helpers ──────────────────────────────────────────────────────

fn path_and_query(safe: &url_guard::SafeUrl) -> &str {
    let prefix = safe.scheme.len() + 3 + safe.authority.len();
    safe.normalized.get(prefix..).unwrap_or("")
}

fn split_path_query(pq: &str) -> (&str, &str) {
    let pq = pq.split('#').next().unwrap_or(pq);
    match pq.split_once('?') {
        Some((p, q)) => (if p.is_empty() { "/" } else { p }, q),
        None => (if pq.is_empty() { "/" } else { pq }, ""),
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        if k == key { Some(v.to_string()) } else { None }
    })
}

fn clean_id(raw: &str) -> Option<String> {
    let id: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if id.is_empty() { None } else { Some(id) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_id_from_watch_url() {
        assert_eq!(
            video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42"),
            Some("dQw4w9WgXcQ".to_string())
        );
    }

    #[test]
    fn extracts_id_from_short_and_shorts_and_embed() {
        assert_eq!(
            video_id("https://youtu.be/dQw4w9WgXcQ?si=abc"),
            Some("dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            video_id("https://www.youtube.com/shorts/abc123DEF45"),
            Some("abc123DEF45".to_string())
        );
        assert_eq!(
            video_id("https://www.youtube.com/embed/xyz789ABCde"),
            Some("xyz789ABCde".to_string())
        );
    }

    #[test]
    fn non_youtube_url_returns_none() {
        assert_eq!(video_id("https://example.com/watch?v=abc"), None);
        assert_eq!(video_id("https://vimeo.com/12345"), None);
    }

    #[test]
    fn selects_manual_english_track_over_asr() {
        let tracks = vec![
            CaptionTrack {
                base_url: "https://t/asr".into(),
                language_code: Some("en".into()),
                kind: Some("asr".into()),
            },
            CaptionTrack {
                base_url: "https://t/manual".into(),
                language_code: Some("en".into()),
                kind: None,
            },
            CaptionTrack {
                base_url: "https://t/de".into(),
                language_code: Some("de".into()),
                kind: None,
            },
        ];
        assert_eq!(select_caption_track(&tracks).base_url, "https://t/manual");
    }

    #[test]
    fn selects_any_when_no_english() {
        let tracks = vec![CaptionTrack {
            base_url: "https://t/fr".into(),
            language_code: Some("fr".into()),
            kind: Some("asr".into()),
        }];
        assert_eq!(select_caption_track(&tracks).base_url, "https://t/fr");
    }

    #[test]
    fn json3_url_forces_format() {
        assert_eq!(
            json3_url("https://yt/api/timedtext?v=x&ei=y&fmt=srv3&hl=en"),
            "https://yt/api/timedtext?v=x&ei=y&hl=en&fmt=json3"
        );
        assert_eq!(
            json3_url("https://yt/api/timedtext?v=x"),
            "https://yt/api/timedtext?v=x&fmt=json3"
        );
    }

    #[test]
    fn parses_json3_into_joined_text() {
        let body = r#"{"events":[
            {"tStartMs":0,"segs":[{"utf8":"Hello"},{"utf8":" world"}]},
            {"tStartMs":1000,"segs":[{"utf8":"second\n"},{"utf8":"line"}]},
            {"tStartMs":2000,"segs":[{"utf8":"\n"}]}
        ]}"#;
        assert_eq!(parse_json3(body).unwrap(), "Hello world second line");
    }

    #[test]
    fn parses_srv3_xml_into_joined_text() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?><timedtext format="3">
<body>
<p t="0" d="1680">We&#39;re no strangers</p>
<p t="1680" d="2000">to <s>love</s></p>
</body></timedtext>"#;
        assert_eq!(parse_srv3_xml(xml), "We're no strangers to love");
    }

    #[test]
    fn parse_timedtext_dispatches_on_shape() {
        assert!(parse_timedtext("not json or xml").is_err());
        assert_eq!(parse_timedtext("{\"events\":[]}").unwrap(), "");
    }
}
