//! The window's viewer: what a chat links to on the public web, opened in a
//! tab beside the chat. RAST's page may only load its own addresses, so it
//! comes through AI Agent's server (`GET /api/ai/view?url=&as=`): only
//! public addresses (also after redirects, also at connect time), only the
//! types asked for, and served sandboxed (no scripts, even when opened as a
//! page):
//!
//! * `media` (the default): images, videos and PDFs, a byte range passed on
//!   for seeking;
//! * `text`: text and code, served as `text/plain`, at most 4 MiB;
//! * `document`: Word, spreadsheet, slides, OpenDocument and RTF files, as
//!   bytes for the window to turn into pages (`crate::convert`), at most
//!   64 MiB.
//!
//! A web page itself opens in a frame where the site allows that; first
//! [`page_info`] reads its title, description and picture and whether it
//! may be framed (`GET /api/ai/view/page?url=`).

use std::io::Read;
use std::time::Duration;

use serde::Serialize;

use crate::tools::{check_public, public_media_agent, WebUrl};

/// The most one media answer carries.
pub const MAX_VIEW_BYTES: u64 = 512 * 1024 * 1024;
/// The most of a text file shown.
pub const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
/// The largest document turned into pages.
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
/// How much of a page is read for its title and description.
const MAX_HEAD_BYTES: u64 = 512 * 1024;
/// Redirects followed (each checked again).
const MAX_REDIRECTS: usize = 6;

/// What the viewer shows a type as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Pdf,
}

/// What the window asked for (`as=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    Media,
    Text,
    Document,
}

impl Wanted {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("media") {
            "" | "media" => Ok(Self::Media),
            "text" => Ok(Self::Text),
            "document" => Ok(Self::Document),
            other => Err(format!(
                "the viewer opens media, text or documents, not {other:?}"
            )),
        }
    }

    fn accept(self) -> &'static str {
        match self {
            Self::Media => "image/*,video/*,application/pdf;q=0.9",
            Self::Text => "text/plain,text/markdown,text/*;q=0.9,application/json;q=0.8,*/*;q=0.1",
            Self::Document => "application/vnd.openxmlformats-officedocument.*,application/vnd.oasis.opendocument.*,application/msword,application/vnd.ms-excel,application/vnd.ms-powerpoint,application/rtf,*/*;q=0.5",
        }
    }

    fn cap(self) -> u64 {
        match self {
            Self::Media => MAX_VIEW_BYTES,
            Self::Text => MAX_TEXT_BYTES,
            Self::Document => MAX_DOCUMENT_BYTES,
        }
    }

    /// Whether the site's `Content-Type` is something this asks for.
    pub fn takes(self, content_type: &str) -> bool {
        let essence = essence(content_type);
        match self {
            Self::Media => media_kind(content_type).is_some(),
            Self::Text => {
                (essence.starts_with("text/") && essence != "text/html")
                    || matches!(
                        essence.as_str(),
                        "application/json"
                            | "application/ld+json"
                            | "application/x-ndjson"
                            | "application/xml"
                            | "application/yaml"
                            | "application/x-yaml"
                            | "application/toml"
                            | "application/javascript"
                            | "application/x-javascript"
                            | "application/typescript"
                            | "application/x-sh"
                            | "application/sql"
                    )
            }
            Self::Document => {
                essence.starts_with("application/vnd.openxmlformats-officedocument.")
                    || essence.starts_with("application/vnd.oasis.opendocument.")
                    || matches!(
                        essence.as_str(),
                        "application/msword"
                            | "application/vnd.ms-excel"
                            | "application/vnd.ms-powerpoint"
                            | "application/vnd.ms-office"
                            | "application/rtf"
                            | "text/rtf"
                            | "application/x-rtf"
                            // Many sites send documents as plain bytes;
                            // the converter checks what they are.
                            | "application/octet-stream"
                            | "application/zip"
                            | "application/x-zip-compressed"
                            | "binary/octet-stream"
                    )
            }
        }
    }

    fn what(self) -> &'static str {
        match self {
            Self::Media => "images, videos and PDFs",
            Self::Text => "text files",
            Self::Document => "documents",
        }
    }
}

fn essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// The viewer's kind of a `Content-Type`, or `None` for anything else.
pub fn media_kind(content_type: &str) -> Option<MediaKind> {
    match essence(content_type).as_str() {
        "image/png"
        | "image/jpeg"
        | "image/jpg"
        | "image/gif"
        | "image/webp"
        | "image/avif"
        | "image/bmp"
        | "image/svg+xml"
        | "image/x-icon"
        | "image/vnd.microsoft.icon" => Some(MediaKind::Image),
        "video/mp4" | "video/webm" | "video/ogg" | "video/quicktime" | "video/x-m4v" => {
            Some(MediaKind::Video)
        }
        "application/pdf" => Some(MediaKind::Pdf),
        _ => None,
    }
}

/// A `Range` header worth passing on: one `bytes=` range.
pub fn usable_range(range: &str) -> Option<&str> {
    let range = range.trim();
    let spec = range.strip_prefix("bytes=")?;
    let (from, to) = spec.split_once('-')?;
    let digits = |s: &str| s.len() <= 19 && s.bytes().all(|b| b.is_ascii_digit());
    (digits(from) && digits(to) && !(from.is_empty() && to.is_empty())).then_some(range)
}

/// An answer from the web the viewer may show.
pub struct Media {
    /// 200, or 206 for a range.
    pub status: u16,
    /// As the site said it (a type that was asked for).
    pub content_type: String,
    pub length: Option<u64>,
    pub content_range: Option<String>,
    pub accepts_ranges: bool,
    pub body: Box<dyn Read + Send>,
}

/// Open `raw` (http or https, public) for the viewer (blocking).
pub fn open(raw: &str, range: Option<&str>, wanted: Wanted) -> Result<Media, String> {
    open_with(raw, range, wanted, &public_media_agent(), check_public)
}

/// The site's answer after redirects (each address checked).
fn fetch(
    raw: &str,
    accept: &str,
    range: Option<&str>,
    agent: &ureq::Agent,
    check: fn(&WebUrl) -> Result<(), String>,
) -> Result<(WebUrl, ureq::http::Response<ureq::Body>), String> {
    let mut url = WebUrl::parse(raw)?;
    for _ in 0..=MAX_REDIRECTS {
        check(&url)?;
        let address = url.to_url();
        let mut request = agent.get(&address).header("Accept", accept);
        if let Some(range) = range.and_then(usable_range) {
            request = request.header("Range", range);
        }
        let response = request
            .call()
            .map_err(|e| format!("cannot open {address}: {e}"))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            let location = header(&response, "location")
                .ok_or_else(|| format!("{address} redirects without saying where"))?;
            url = url.join(&location)?;
            continue;
        }
        return Ok((url, response));
    }
    Err(format!("{raw} redirects more than {MAX_REDIRECTS} times"))
}

fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// [`open`] with its agent and its check of each address (tests open a
/// local site).
fn open_with(
    raw: &str,
    range: Option<&str>,
    wanted: Wanted,
    agent: &ureq::Agent,
    check: fn(&WebUrl) -> Result<(), String>,
) -> Result<Media, String> {
    // Seeking is for media; a text or document comes whole.
    let range = range.filter(|_| wanted == Wanted::Media);
    let (url, response) = fetch(raw, wanted.accept(), range, agent, check)?;
    let address = url.to_url();
    let status = response.status().as_u16();
    if status != 200 && status != 206 {
        return Err(format!("{address} answered HTTP {status}"));
    }
    let content_type = header(&response, "content-type").unwrap_or_default();
    if !wanted.takes(&content_type) {
        let said = if content_type.is_empty() {
            "no type".to_string()
        } else {
            content_type
        };
        return Err(format!(
            "{address} is {said}; the viewer shows {} here",
            wanted.what()
        ));
    }
    let length = header(&response, "content-length").and_then(|v| v.trim().parse::<u64>().ok());
    let accepts_ranges = wanted == Wanted::Media
        && header(&response, "accept-ranges").is_some_and(|v| v.contains("bytes"));
    let too_large = length.is_some_and(|n| n > wanted.cap());
    // A long text shows its start; media seeks in ranges; a document must
    // come whole.
    if too_large && status == 200 && wanted != Wanted::Text && !accepts_ranges {
        return Err(format!(
            "{address} is larger than {} MB",
            wanted.cap() / 1024 / 1024
        ));
    }
    let length = if wanted == Wanted::Text && too_large {
        None
    } else {
        length
    };
    Ok(Media {
        status,
        content_type,
        length,
        content_range: header(&response, "content-range").filter(|_| status == 206),
        accepts_ranges,
        body: Box::new(response.into_body().into_reader().take(wanted.cap())),
    })
}

/// What a web page is, for its tab: a frame when the site allows one, else
/// a card with its title, description and picture.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    /// Where it ended up (after redirects).
    pub url: String,
    pub title: String,
    pub description: String,
    pub site_name: String,
    /// Absolute http(s) addresses, or "".
    pub image: String,
    pub icon: String,
    /// The site's type (`text/html` for a page; a PDF or picture opens as
    /// one instead).
    pub content_type: String,
    /// Whether the site lets other sites show it in a frame.
    pub frameable: bool,
    /// Why not, when it does not.
    pub refused: String,
}

/// Read what a page is (blocking).
pub fn page_info(raw: &str) -> Result<PageInfo, String> {
    page_info_with(raw, &public_media_agent(), check_public)
}

fn page_info_with(
    raw: &str,
    agent: &ureq::Agent,
    check: fn(&WebUrl) -> Result<(), String>,
) -> Result<PageInfo, String> {
    let (url, response) = fetch(
        raw,
        "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5",
        None,
        agent,
        check,
    )?;
    let address = url.to_url();
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("{address} answered HTTP {status}"));
    }
    let content_type = essence(&header(&response, "content-type").unwrap_or_default());
    let csp: Vec<String> = response
        .headers()
        .get_all("content-security-policy")
        .iter()
        .filter_map(|v| v.to_str().ok().map(str::to_string))
        .collect();
    let refused = frame_refusal(header(&response, "x-frame-options").as_deref(), &csp);
    let mut info = PageInfo {
        url: address,
        content_type: content_type.clone(),
        frameable: refused.is_none(),
        refused: refused.unwrap_or_default(),
        ..PageInfo::default()
    };
    if content_type == "text/html" || content_type == "application/xhtml+xml" {
        let mut head = Vec::new();
        let _ = response
            .into_body()
            .into_reader()
            .take(MAX_HEAD_BYTES)
            .read_to_end(&mut head);
        read_head(&String::from_utf8_lossy(&head), &url, &mut info);
    }
    Ok(info)
}

/// Why a site refuses to be framed by other sites (`X-Frame-Options`, or a
/// `frame-ancestors` that names only some places), or `None`.
pub fn frame_refusal(frame_options: Option<&str>, policies: &[String]) -> Option<String> {
    if let Some(value) = frame_options.map(str::trim).filter(|v| !v.is_empty()) {
        if !value.eq_ignore_ascii_case("allowall") {
            return Some(format!(
                "the site shows itself only on its own pages (X-Frame-Options: {value})"
            ));
        }
    }
    for policy in policies {
        for directive in policy.split(';') {
            let mut words = directive.split_whitespace();
            if !words
                .next()
                .is_some_and(|name| name.eq_ignore_ascii_case("frame-ancestors"))
            {
                continue;
            }
            let sources: Vec<&str> = words.collect();
            let anywhere = sources
                .iter()
                .any(|s| *s == "*" || s.eq_ignore_ascii_case("https:"));
            if !anywhere {
                return Some(
                    "the site shows itself only on the pages it names (frame-ancestors)".into(),
                );
            }
        }
    }
    None
}

/// Fill `info` from a page's `<head>`: its title, description, site name,
/// picture and icon.
fn read_head(html: &str, base: &WebUrl, info: &mut PageInfo) {
    let lower = html.to_ascii_lowercase();
    let meta = |key: &str| -> Option<String> {
        let mut from = 0;
        while let Some(at) = lower[from..].find("<meta") {
            let start = from + at;
            let end = lower[start..]
                .find('>')
                .map(|e| start + e)
                .unwrap_or(lower.len());
            let attrs = attributes(&html[start + 5..end]);
            let names = ["property", "name", "itemprop"];
            if names.iter().any(|n| {
                attrs
                    .iter()
                    .any(|(k, v)| k == n && v.eq_ignore_ascii_case(key))
            }) {
                if let Some((_, content)) = attrs.iter().find(|(k, _)| k == "content") {
                    let content = content.trim();
                    if !content.is_empty() {
                        return Some(content.to_string());
                    }
                }
            }
            from = end.max(start + 5);
        }
        None
    };
    let title_tag = lower.find("<title").and_then(|at| {
        let open = at + lower[at..].find('>')? + 1;
        let close = open + lower[open..].find("</title")?;
        Some(decode_entities(html[open..close].trim()))
    });
    info.title = clip(
        &meta("og:title")
            .or_else(|| meta("twitter:title"))
            .map(|t| decode_entities(&t))
            .or(title_tag)
            .unwrap_or_default(),
        300,
    );
    info.description = clip(
        &meta("og:description")
            .or_else(|| meta("description"))
            .or_else(|| meta("twitter:description"))
            .map(|t| decode_entities(&t))
            .unwrap_or_default(),
        600,
    );
    info.site_name = clip(
        &meta("og:site_name")
            .map(|t| decode_entities(&t))
            .unwrap_or_default(),
        120,
    );
    let absolute = |href: &str| -> String {
        let href = decode_entities(href.trim());
        if href.is_empty() || href.starts_with("data:") {
            return String::new();
        }
        base.join(&href).map(|u| u.to_url()).unwrap_or_default()
    };
    info.image = meta("og:image")
        .or_else(|| meta("twitter:image"))
        .map(|src| absolute(&src))
        .unwrap_or_default();
    // The first <link rel="…icon…">, else /favicon.ico.
    let mut from = 0;
    while let Some(at) = lower[from..].find("<link") {
        let start = from + at;
        let end = lower[start..]
            .find('>')
            .map(|e| start + e)
            .unwrap_or(lower.len());
        let attrs = attributes(&html[start + 5..end]);
        let icon = attrs.iter().any(|(k, v)| {
            k == "rel"
                && v.split_whitespace().any(|r| {
                    r.eq_ignore_ascii_case("icon") || r.eq_ignore_ascii_case("apple-touch-icon")
                })
        });
        if icon {
            if let Some((_, href)) = attrs.iter().find(|(k, _)| k == "href") {
                info.icon = absolute(href);
                if !info.icon.is_empty() {
                    break;
                }
            }
        }
        from = end.max(start + 5);
    }
    if info.icon.is_empty() {
        info.icon = absolute("/favicon.ico");
    }
}

/// `name="value"` pairs of a tag (names lowercased).
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = tag.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let name = tag[start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let from = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let value = &tag[from..i];
                i += 1;
                value
            } else {
                let from = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                &tag[from..i]
            };
            out.push((name, value.to_string()));
        } else {
            out.push((name, String::new()));
        }
    }
    out
}

/// The usual HTML entities in titles and descriptions.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => name
                .strip_prefix("#x")
                .or_else(|| name.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

/// How long the viewer waits for the site, and lets one answer run.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
pub const BODY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viewer_takes_images_videos_and_pdfs_only() {
        for (kind, expected) in [
            ("image/png", Some(MediaKind::Image)),
            ("IMAGE/JPEG; charset=binary", Some(MediaKind::Image)),
            ("image/svg+xml", Some(MediaKind::Image)),
            ("video/mp4", Some(MediaKind::Video)),
            ("video/webm", Some(MediaKind::Video)),
            ("application/pdf", Some(MediaKind::Pdf)),
            ("text/html; charset=utf-8", None),
            ("application/javascript", None),
            ("application/octet-stream", None),
            ("", None),
        ] {
            assert_eq!(media_kind(kind), expected, "{kind}");
        }
    }

    #[test]
    fn text_and_documents_take_their_own_types() {
        assert!(Wanted::Text.takes("text/plain; charset=utf-8"));
        assert!(Wanted::Text.takes("text/markdown"));
        assert!(Wanted::Text.takes("application/json"));
        assert!(!Wanted::Text.takes("text/html"));
        assert!(!Wanted::Text.takes("application/octet-stream"));
        assert!(!Wanted::Text.takes("image/png"));
        assert!(Wanted::Document
            .takes("application/vnd.openxmlformats-officedocument.wordprocessingml.document"));
        assert!(Wanted::Document.takes("application/vnd.oasis.opendocument.text"));
        assert!(Wanted::Document.takes("application/msword"));
        assert!(Wanted::Document.takes("application/octet-stream"));
        assert!(!Wanted::Document.takes("text/html"));
        assert!(!Wanted::Document.takes("image/png"));
        assert!(!Wanted::Media.takes("application/octet-stream"));
        assert_eq!(Wanted::parse(None), Ok(Wanted::Media));
        assert_eq!(Wanted::parse(Some("text")), Ok(Wanted::Text));
        assert!(Wanted::parse(Some("html")).is_err());
    }

    #[test]
    fn only_one_byte_range_is_passed_on() {
        assert_eq!(usable_range("bytes=0-"), Some("bytes=0-"));
        assert_eq!(usable_range("bytes=100-199"), Some("bytes=100-199"));
        assert_eq!(usable_range("bytes=-500"), Some("bytes=-500"));
        for bad in [
            "bytes=-",
            "bytes=0-1,5-9",
            "items=0-1",
            "bytes=a-b",
            "bytes=0-1\r\nX: y",
        ] {
            assert_eq!(usable_range(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_site_that_names_where_it_shows_refuses_frames() {
        assert_eq!(frame_refusal(None, &[]), None);
        assert!(frame_refusal(Some("DENY"), &[]).is_some());
        assert!(frame_refusal(Some("sameorigin"), &[]).is_some());
        assert_eq!(frame_refusal(Some("ALLOWALL"), &[]), None);
        let policy = |p: &str| vec![p.to_string()];
        assert!(
            frame_refusal(None, &policy("default-src 'self'; frame-ancestors 'none'")).is_some()
        );
        assert!(frame_refusal(None, &policy("frame-ancestors 'self' https://a.example")).is_some());
        assert_eq!(frame_refusal(None, &policy("frame-ancestors *")), None);
        assert_eq!(frame_refusal(None, &policy("frame-ancestors https:")), None);
        assert_eq!(frame_refusal(None, &policy("default-src 'self'")), None);
    }

    #[test]
    fn a_pages_head_gives_its_title_description_picture_and_icon() {
        let base = WebUrl::parse("https://news.example/2026/story.html").unwrap();
        let mut info = PageInfo::default();
        read_head(
            r#"<!doctype html><html><head><TITLE> Plain &amp; simple </TITLE>
            <meta property="og:title" content="Lake &quot;Toba&quot; at dawn">
            <meta name=description content='A walk by the lake.'>
            <meta property="og:site_name" content="News">
            <meta property="og:image" content="/img/lake.jpg">
            <link rel="shortcut icon" href="favicon.png"></head>"#,
            &base,
            &mut info,
        );
        assert_eq!(info.title, "Lake \"Toba\" at dawn");
        assert_eq!(info.description, "A walk by the lake.");
        assert_eq!(info.site_name, "News");
        assert_eq!(info.image, "https://news.example/img/lake.jpg");
        assert_eq!(info.icon, "https://news.example/2026/favicon.png");

        let mut plain = PageInfo::default();
        read_head("<title>Only &#x41;&#66;</title>", &base, &mut plain);
        assert_eq!(plain.title, "Only AB");
        assert_eq!(plain.icon, "https://news.example/favicon.ico");
    }

    /// A site: `/r` redirects to `/pic.png` (10 bytes, ranges too),
    /// `/page` is HTML (`X-Frame-Options: DENY`), `/notes.md` text,
    /// `/report.docx` a document sent as bytes, `/gone` is 404.
    fn site() -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut range = None;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("range") {
                            range = Some(value.trim().to_string());
                        }
                    }
                }
                let png = b"\x89PNG-data!";
                let page = "<html><head><title>Hello</title><meta name=\"description\" content=\"A page\"></head></html>";
                let answer: Vec<u8> = match line.split(' ').nth(1).unwrap_or("") {
                    "/r" => b"HTTP/1.1 302 Found\r\nLocation: /pic.png\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    "/pic.png" if range.as_deref() == Some("bytes=0-3") => {
                        let mut out = b"HTTP/1.1 206 Partial Content\r\nContent-Type: image/png\r\nContent-Range: bytes 0-3/10\r\nAccept-Ranges: bytes\r\nContent-Length: 4\r\nConnection: close\r\n\r\n".to_vec();
                        out.extend_from_slice(&png[..4]);
                        out
                    }
                    "/pic.png" => {
                        let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nAccept-Ranges: bytes\r\nContent-Length: 10\r\nConnection: close\r\n\r\n".to_vec();
                        out.extend_from_slice(png);
                        out
                    }
                    "/page" => format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nX-Frame-Options: DENY\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).into_bytes(),
                    "/open" => format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len()).into_bytes(),
                    "/notes.md" => b"HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: 7\r\nConnection: close\r\n\r\n# Notes".to_vec(),
                    "/report.docx" => b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 4\r\nConnection: close\r\n\r\nPK\x03\x04".to_vec(),
                    _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&answer);
            }
        });
        format!("http://{addr}")
    }

    fn anywhere(_: &WebUrl) -> Result<(), String> {
        Ok(())
    }

    fn local_agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .new_agent()
    }

    #[test]
    fn the_viewer_follows_redirects_passes_ranges_and_keeps_to_media() {
        let agent = local_agent();
        let base = site();
        let mut media =
            open_with(&format!("{base}/r"), None, Wanted::Media, &agent, anywhere).unwrap();
        assert_eq!(media.status, 200);
        assert_eq!(media_kind(&media.content_type), Some(MediaKind::Image));
        assert_eq!(media.length, Some(10));
        assert!(media.accepts_ranges);
        let mut bytes = Vec::new();
        media.body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"\x89PNG-data!");

        let mut part = open_with(
            &format!("{base}/pic.png"),
            Some("bytes=0-3"),
            Wanted::Media,
            &agent,
            anywhere,
        )
        .unwrap();
        assert_eq!(part.status, 206);
        assert_eq!(part.content_range.as_deref(), Some("bytes 0-3/10"));
        let mut bytes = Vec::new();
        part.body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"\x89PNG");

        let page = open_with(
            &format!("{base}/page"),
            None,
            Wanted::Media,
            &agent,
            anywhere,
        )
        .err()
        .unwrap();
        assert!(
            page.contains("text/html") && page.contains("images, videos and PDFs"),
            "{page}"
        );
        let gone = open_with(
            &format!("{base}/gone"),
            None,
            Wanted::Media,
            &agent,
            anywhere,
        )
        .err()
        .unwrap();
        assert!(gone.contains("HTTP 404"), "{gone}");
        // The check runs for every address, the redirect's too.
        fn nowhere(_: &WebUrl) -> Result<(), String> {
            Err("refused".into())
        }
        assert_eq!(
            open_with(&format!("{base}/r"), None, Wanted::Media, &agent, nowhere)
                .err()
                .as_deref(),
            Some("refused")
        );
    }

    #[test]
    fn text_and_documents_come_whole_with_their_types() {
        let agent = local_agent();
        let base = site();
        let mut text = open_with(
            &format!("{base}/notes.md"),
            Some("bytes=0-1"),
            Wanted::Text,
            &agent,
            anywhere,
        )
        .unwrap();
        assert_eq!(text.status, 200, "no range for text");
        let mut bytes = Vec::new();
        text.body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"# Notes");
        assert!(open_with(
            &format!("{base}/page"),
            None,
            Wanted::Text,
            &agent,
            anywhere
        )
        .is_err());
        let mut doc = open_with(
            &format!("{base}/report.docx"),
            None,
            Wanted::Document,
            &agent,
            anywhere,
        )
        .unwrap();
        let mut bytes = Vec::new();
        doc.body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"PK\x03\x04");
        assert!(open_with(
            &format!("{base}/report.docx"),
            None,
            Wanted::Media,
            &agent,
            anywhere
        )
        .is_err());
    }

    #[test]
    fn a_page_says_what_it_is_and_whether_it_may_be_framed() {
        let agent = local_agent();
        let base = site();
        let denied = page_info_with(&format!("{base}/page"), &agent, anywhere).unwrap();
        assert_eq!(denied.title, "Hello");
        assert_eq!(denied.description, "A page");
        assert!(!denied.frameable);
        assert!(
            denied.refused.contains("X-Frame-Options"),
            "{}",
            denied.refused
        );
        assert_eq!(denied.icon, format!("{base}/favicon.ico"));
        let open = page_info_with(&format!("{base}/open"), &agent, anywhere).unwrap();
        assert!(open.frameable);
        assert_eq!(open.content_type, "text/html");
        let picture = page_info_with(&format!("{base}/r"), &agent, anywhere).unwrap();
        assert_eq!(picture.content_type, "image/png");
        assert_eq!(picture.url, format!("{base}/pic.png"));
        assert!(page_info_with(&format!("{base}/gone"), &agent, anywhere).is_err());
    }

    #[test]
    fn the_viewer_opens_public_addresses_only() {
        for raw in [
            "http://127.0.0.1/a.png",
            "http://localhost/a.png",
            "http://10.0.0.8/a.pdf",
            "http://[::1]/a.mp4",
            "file:///etc/passwd",
            "ftp://example.com/a.png",
        ] {
            assert!(open(raw, None, Wanted::Media).is_err(), "{raw}");
            assert!(page_info(raw).is_err(), "{raw}");
        }
    }
}
