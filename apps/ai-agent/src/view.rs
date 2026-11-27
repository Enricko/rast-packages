//! The window's viewer: images, videos and PDFs on the public web, opened
//! beside the chat (`GET /api/ai/view?url=`). RAST's page may only load its
//! own addresses, so the media comes through AI Agent's server: only public
//! addresses (also after redirects, also at connect time), only image,
//! video and PDF types, a byte range passed on for seeking, and served
//! sandboxed (no scripts, even when opened as a page).

use std::io::Read;
use std::time::Duration;

use crate::tools::{check_public, public_media_agent, WebUrl};

/// The most one answer carries.
pub const MAX_VIEW_BYTES: u64 = 512 * 1024 * 1024;
/// Redirects followed (each checked again).
const MAX_REDIRECTS: usize = 6;

/// What the viewer shows a type as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Pdf,
}

/// The viewer's kind of a `Content-Type`, or `None` for anything else.
pub fn media_kind(content_type: &str) -> Option<MediaKind> {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match essence.as_str() {
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
    /// As the site said it (an allowed type).
    pub content_type: String,
    pub length: Option<u64>,
    pub content_range: Option<String>,
    pub accepts_ranges: bool,
    pub body: Box<dyn Read + Send>,
}

/// Open `raw` (http or https, public) for the viewer (blocking).
pub fn open(raw: &str, range: Option<&str>) -> Result<Media, String> {
    open_with(raw, range, &public_media_agent(), check_public)
}

/// [`open`] with its agent and its check of each address (tests open a
/// local site).
fn open_with(
    raw: &str,
    range: Option<&str>,
    agent: &ureq::Agent,
    check: fn(&WebUrl) -> Result<(), String>,
) -> Result<Media, String> {
    let mut url = WebUrl::parse(raw)?;
    for _ in 0..=MAX_REDIRECTS {
        check(&url)?;
        let address = url.to_url();
        let mut request = agent
            .get(&address)
            .header("Accept", "image/*,video/*,application/pdf;q=0.9");
        if let Some(range) = range.and_then(usable_range) {
            request = request.header("Range", range);
        }
        let response = request
            .call()
            .map_err(|e| format!("cannot open {address}: {e}"))?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        };
        if (300..400).contains(&status) {
            let location = header("location")
                .ok_or_else(|| format!("{address} redirects without saying where"))?;
            url = url.join(&location)?;
            continue;
        }
        if status != 200 && status != 206 {
            return Err(format!("{address} answered HTTP {status}"));
        }
        let content_type = header("content-type").unwrap_or_default();
        if media_kind(&content_type).is_none() {
            let said = if content_type.is_empty() {
                "no type".to_string()
            } else {
                content_type
            };
            return Err(format!(
                "{address} is {said}; the viewer shows images, videos and PDFs"
            ));
        }
        let length = header("content-length").and_then(|v| v.trim().parse::<u64>().ok());
        if length.is_some_and(|n| n > MAX_VIEW_BYTES) && status == 200 {
            let accepts = header("accept-ranges").is_some_and(|v| v.contains("bytes"));
            if !accepts {
                return Err(format!(
                    "{address} is larger than {} MB",
                    MAX_VIEW_BYTES / 1024 / 1024
                ));
            }
        }
        return Ok(Media {
            status,
            content_type,
            length,
            content_range: header("content-range").filter(|_| status == 206),
            accepts_ranges: header("accept-ranges").is_some_and(|v| v.contains("bytes")),
            body: Box::new(response.into_body().into_reader().take(MAX_VIEW_BYTES)),
        });
    }
    Err(format!("{raw} redirects more than {MAX_REDIRECTS} times"))
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

    /// A site: `/r` redirects to `/pic.png` (10 bytes, ranges too),
    /// `/page` is HTML, `/gone` is 404.
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
                    "/page" => b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi".to_vec(),
                    _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                };
                let _ = stream.write_all(&answer);
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn the_viewer_follows_redirects_passes_ranges_and_keeps_to_media() {
        fn anywhere(_: &WebUrl) -> Result<(), String> {
            Ok(())
        }
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .new_agent();
        let base = site();
        let mut media = open_with(&format!("{base}/r"), None, &agent, anywhere).unwrap();
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
            &agent,
            anywhere,
        )
        .unwrap();
        assert_eq!(part.status, 206);
        assert_eq!(part.content_range.as_deref(), Some("bytes 0-3/10"));
        let mut bytes = Vec::new();
        part.body.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"\x89PNG");

        let page = open_with(&format!("{base}/page"), None, &agent, anywhere)
            .err()
            .unwrap();
        assert!(
            page.contains("text/html") && page.contains("images, videos and PDFs"),
            "{page}"
        );
        let gone = open_with(&format!("{base}/gone"), None, &agent, anywhere)
            .err()
            .unwrap();
        assert!(gone.contains("HTTP 404"), "{gone}");
        // The check runs for every address, the redirect's too.
        fn nowhere(_: &WebUrl) -> Result<(), String> {
            Err("refused".into())
        }
        assert_eq!(
            open_with(&format!("{base}/r"), None, &agent, nowhere)
                .err()
                .as_deref(),
            Some("refused")
        );
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
            assert!(open(raw, None).is_err(), "{raw}");
        }
    }
}
