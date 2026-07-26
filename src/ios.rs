//! Short-lived HTTP playback for SSH clients that cannot consume an audio stream.
//!
//! The server binds to loopback by default and is intended to be reached through
//! an SSH local-forward from the listening device. It exposes a random bearer URL,
//! serves a small HTML player plus the synthesized WAV, then exits after a complete
//! audio transfer or when the link expires.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(40);
#[cfg(not(test))]
const COMPLETE_TRANSFER_GRACE: Duration = Duration::from_secs(2);
#[cfg(test)]
const COMPLETE_TRANSFER_GRACE: Duration = Duration::from_millis(20);

/// A one-shot HTTP server carrying one synthesized WAV.
pub struct IosPlaybackServer {
    listener: TcpListener,
    bind_addr: SocketAddr,
    token: String,
    url: String,
    timeout: Duration,
}

impl IosPlaybackServer {
    /// Bind the playback endpoint before synthesis starts, so port conflicts are
    /// reported before model loading and inference do expensive work.
    pub fn bind(
        bind_addr: SocketAddr,
        public_base_url: Option<&str>,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() {
            bail!("the iOS playback timeout must be greater than zero");
        }

        let listener = TcpListener::bind(bind_addr)
            .with_context(|| format!("could not bind the iOS playback server to {bind_addr}"))?;
        listener
            .set_nonblocking(true)
            .context("could not configure the iOS playback listener")?;
        let actual_addr = listener
            .local_addr()
            .context("could not determine the iOS playback listener address")?;

        let base_url = match public_base_url {
            Some(url) => normalize_base_url(url)?,
            None => base_url_for_listener(actual_addr)?,
        };
        let token = random_token()?;
        let url = format!("{base_url}/{token}");

        Ok(Self {
            listener,
            bind_addr: actual_addr,
            token,
            url,
            timeout,
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn bind_addr(&self) -> SocketAddr {
        self.bind_addr
    }

    /// Serve the page and WAV until a complete transfer finishes or the link
    /// expires. Partial/range requests remain available for the full timeout,
    /// because iOS media players may probe the file before the user presses play.
    pub fn serve(self, wav: &[u8]) -> Result<()> {
        if wav.is_empty() {
            bail!("cannot serve an empty WAV payload");
        }

        let started = Instant::now();
        let mut complete_at: Option<Instant> = None;

        loop {
            let now = Instant::now();
            if let Some(completed) = complete_at {
                if now.duration_since(completed) >= COMPLETE_TRANSFER_GRACE {
                    return Ok(());
                }
            }
            if now.duration_since(started) >= self.timeout {
                eprintln!(
                    "iOS playback link expired after {} seconds.",
                    self.timeout.as_secs()
                );
                return Ok(());
            }

            match self.listener.accept() {
                Ok((mut stream, _peer)) => {
                    // Accepted sockets can inherit the listener's nonblocking
                    // mode on some platforms (including macOS). Restore blocking
                    // I/O so `write_all` waits instead of truncating larger WAVs
                    // on `WouldBlock`; the timeouts below retain the bound.
                    if let Err(err) = stream.set_nonblocking(false) {
                        eprintln!("could not configure the iOS playback connection: {err}");
                        continue;
                    }
                    if let Err(err) = stream.set_read_timeout(Some(IO_TIMEOUT)) {
                        eprintln!("could not set iOS playback read timeout: {err}");
                        continue;
                    }
                    if let Err(err) = stream.set_write_timeout(Some(IO_TIMEOUT)) {
                        eprintln!("could not set iOS playback write timeout: {err}");
                        continue;
                    }
                    match handle_connection(&mut stream, &self.token, wav) {
                        Ok(ConnectionOutcome::PlaybackFinished) => return Ok(()),
                        Ok(ConnectionOutcome::CompleteAudioTransfer) => {
                            complete_at = Some(Instant::now());
                        }
                        Ok(ConnectionOutcome::Other) => {}
                        Err(err) => {
                            // One malformed browser probe should not invalidate the link.
                            eprintln!("iOS playback request failed: {err:#}");
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(err) => return Err(err).context("iOS playback listener failed"),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectionOutcome {
    Other,
    CompleteAudioTransfer,
    PlaybackFinished,
}

#[derive(Debug, PartialEq, Eq)]
struct Request {
    method: Method,
    path: String,
    range: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Method {
    Get,
    Head,
    Post,
}

fn handle_connection(stream: &mut TcpStream, token: &str, wav: &[u8]) -> Result<ConnectionOutcome> {
    let request = match read_request(stream) {
        Ok(request) => request,
        Err(err) => {
            write_text_response(stream, 400, "Bad Request", "invalid HTTP request\n", false)?;
            return Err(err);
        }
    };

    let path = request.path.split('?').next().unwrap_or(&request.path);
    let page_path = format!("/{token}");
    let page_path_slash = format!("/{token}/");
    let audio_path = format!("/{token}/audio.wav");
    let done_path = format!("/{token}/done");

    if path == done_path && request.method == Method::Post {
        write_empty_response(stream, 204, "No Content")?;
        return Ok(ConnectionOutcome::PlaybackFinished);
    }

    if path == page_path || path == page_path_slash {
        if !matches!(request.method, Method::Get | Method::Head) {
            write_method_not_allowed(stream, "GET, HEAD")?;
            return Ok(ConnectionOutcome::Other);
        }
        let body = player_page(token);
        write_html_response(stream, &body, request.method == Method::Head)?;
        return Ok(ConnectionOutcome::Other);
    }

    if path == audio_path {
        if !matches!(request.method, Method::Get | Method::Head) {
            write_method_not_allowed(stream, "GET, HEAD")?;
            return Ok(ConnectionOutcome::Other);
        }
        return write_audio_response(stream, wav, &request);
    }

    if path == "/favicon.ico" {
        write_empty_response(stream, 204, "No Content")?;
    } else {
        write_text_response(
            stream,
            404,
            "Not Found",
            "not found\n",
            request.method == Method::Head,
        )?;
    }
    Ok(ConnectionOutcome::Other)
}

fn read_request(stream: &mut TcpStream) -> Result<Request> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];

    loop {
        let read = stream
            .read(&mut chunk)
            .context("failed to read HTTP request")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > MAX_REQUEST_BYTES {
            bail!("HTTP request headers exceeded {MAX_REQUEST_BYTES} bytes");
        }
    }

    if bytes.is_empty() {
        bail!("HTTP client closed the connection before sending a request");
    }
    if bytes.len() > MAX_REQUEST_BYTES {
        bail!("HTTP request headers exceeded {MAX_REQUEST_BYTES} bytes");
    }

    let text = std::str::from_utf8(&bytes).context("HTTP request headers were not UTF-8")?;
    let header_end = text.find("\r\n\r\n").unwrap_or(text.len());
    let mut lines = text[..header_end].split("\r\n");
    let first = lines.next().context("HTTP request line was missing")?;
    let mut request_line = first.split_whitespace();
    let method = match request_line.next() {
        Some("GET") => Method::Get,
        Some("HEAD") => Method::Head,
        Some("POST") => Method::Post,
        Some(other) => bail!("unsupported HTTP method {other}"),
        None => bail!("HTTP request method was missing"),
    };
    let path = request_line
        .next()
        .context("HTTP request path was missing")?
        .to_string();
    let version = request_line.next().context("HTTP version was missing")?;
    if !version.starts_with("HTTP/1.") {
        bail!("unsupported HTTP version {version}");
    }

    let mut range = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("range") {
            range = Some(value.trim().to_string());
        }
    }

    Ok(Request {
        method,
        path,
        range,
    })
}

fn player_page(token: &str) -> Vec<u8> {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<meta name="color-scheme" content="light dark">
<title>speak</title>
<style>
:root {{ font-family: -apple-system, BlinkMacSystemFont, system-ui, sans-serif; }}
body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: Canvas; color: CanvasText; }}
main {{ width: min(34rem, calc(100% - 2rem)); }}
h1 {{ font-size: 1.4rem; margin: 0 0 .5rem; }}
p {{ opacity: .72; line-height: 1.45; }}
audio {{ display: block; width: 100%; margin-top: 1rem; }}
</style>
</head>
<body>
<main>
<h1>Audio ready</h1>
<p>Playback should start automatically. Tap play if iOS blocks autoplay.</p>
<audio id="audio" controls autoplay preload="auto" src="/{token}/audio.wav"></audio>
</main>
<script>
const audio = document.getElementById('audio');
audio.addEventListener('ended', () => {{
  fetch('/{token}/done', {{ method: 'POST', keepalive: true }}).catch(() => {{}});
}});
audio.play().catch(() => {{}});
</script>
</body>
</html>
"#
    )
    .into_bytes()
}

fn write_html_response(stream: &mut TcpStream, body: &[u8], head_only: bool) -> Result<()> {
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; media-src 'self'; script-src 'unsafe-inline'; connect-src 'self'\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .context("failed to write HTML response headers")?;
    if !head_only {
        stream
            .write_all(body)
            .context("failed to write HTML response body")?;
    }
    stream.flush().ok();
    Ok(())
}

fn write_audio_response(
    stream: &mut TcpStream,
    wav: &[u8],
    request: &Request,
) -> Result<ConnectionOutcome> {
    let selected = match request.range.as_deref() {
        Some(value) => match parse_byte_range(value, wav.len()) {
            Some(range) => range,
            None => {
                let headers = format!(
                    "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                    wav.len()
                );
                stream.write_all(headers.as_bytes())?;
                stream.flush().ok();
                return Ok(ConnectionOutcome::Other);
            }
        },
        None => ByteRange {
            start: 0,
            end_inclusive: wav.len() - 1,
        },
    };

    let body = &wav[selected.start..=selected.end_inclusive];
    let partial = request.range.is_some();
    let status = if partial {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    let mut headers = format!(
        "HTTP/1.1 {status}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nContent-Disposition: inline; filename=\"speak.wav\"\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    if partial {
        writeln!(
            headers,
            "Content-Range: bytes {}-{}/{}\r",
            selected.start,
            selected.end_inclusive,
            wav.len()
        )?;
    }
    headers.push_str("\r\n");

    stream
        .write_all(headers.as_bytes())
        .context("failed to write WAV response headers")?;
    if request.method == Method::Get {
        stream
            .write_all(body)
            .context("failed to write WAV response body")?;
    }
    stream.flush().ok();

    let complete = request.method == Method::Get
        && selected.start == 0
        && selected.end_inclusive + 1 == wav.len();
    Ok(if complete {
        ConnectionOutcome::CompleteAudioTransfer
    } else {
        ConnectionOutcome::Other
    })
}

fn write_text_response(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &str,
    head_only: bool,
) -> Result<()> {
    let headers = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes())?;
    if !head_only {
        stream.write_all(body.as_bytes())?;
    }
    stream.flush().ok();
    Ok(())
}

fn write_empty_response(stream: &mut TcpStream, code: u16, reason: &str) -> Result<()> {
    let headers = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(headers.as_bytes())?;
    stream.flush().ok();
    Ok(())
}

fn write_method_not_allowed(stream: &mut TcpStream, allow: &str) -> Result<()> {
    let headers = format!(
        "HTTP/1.1 405 Method Not Allowed\r\nAllow: {allow}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(headers.as_bytes())?;
    stream.flush().ok();
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByteRange {
    start: usize,
    end_inclusive: usize,
}

fn parse_byte_range(value: &str, len: usize) -> Option<ByteRange> {
    if len == 0 {
        return None;
    }
    let spec = value.strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;

    if start.is_empty() {
        let suffix = end.parse::<usize>().ok()?;
        if suffix == 0 {
            return None;
        }
        let count = suffix.min(len);
        return Some(ByteRange {
            start: len - count,
            end_inclusive: len - 1,
        });
    }

    let start = start.parse::<usize>().ok()?;
    if start >= len {
        return None;
    }
    let end_inclusive = if end.is_empty() {
        len - 1
    } else {
        end.parse::<usize>().ok()?.min(len - 1)
    };
    if end_inclusive < start {
        return None;
    }

    Some(ByteRange {
        start,
        end_inclusive,
    })
}

fn normalize_base_url(value: &str) -> Result<String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        bail!("--ios-url must not contain whitespace or control characters");
    }

    let Some((scheme, authority)) = trimmed.split_once("://") else {
        bail!("--ios-url must begin with http:// or https://");
    };
    if !matches!(scheme, "http" | "https") {
        bail!("--ios-url must begin with http:// or https://");
    }
    if authority.is_empty()
        || authority.contains('/')
        || authority.contains('?')
        || authority.contains('#')
    {
        bail!("--ios-url must contain only a scheme and authority, without a path, query, or fragment");
    }
    Ok(trimmed.to_string())
}

fn base_url_for_listener(addr: SocketAddr) -> Result<String> {
    if addr.ip().is_unspecified() {
        bail!(
            "--ios-bind resolved to {addr}; pass --ios-url with an address reachable from the listening device"
        );
    }
    Ok(match addr.ip() {
        IpAddr::V4(ip) => format!("http://{ip}:{}", addr.port()),
        IpAddr::V6(ip) => format!("http://[{ip}]:{}", addr.port()),
    })
}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(token)
}

#[cfg(unix)]
fn fill_random(bytes: &mut [u8]) -> Result<()> {
    let mut random = std::fs::File::open("/dev/urandom")
        .context("could not open the operating system random source")?;
    random
        .read_exact(bytes)
        .context("could not read the operating system random source")
}

#[cfg(windows)]
fn fill_random(bytes: &mut [u8]) -> Result<()> {
    use std::ffi::c_void;

    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

    #[link(name = "bcrypt")]
    extern "system" {
        #[link_name = "BCryptGenRandom"]
        fn bcrypt_gen_random(
            algorithm: *mut c_void,
            buffer: *mut u8,
            buffer_len: u32,
            flags: u32,
        ) -> i32;
    }

    let len = u32::try_from(bytes.len()).context("random token request was too large")?;
    let status = unsafe {
        bcrypt_gen_random(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            len,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status < 0 {
        bail!(
            "Windows CSPRNG failed with NTSTATUS 0x{:08x}",
            status as u32
        );
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn fill_random(_bytes: &mut [u8]) -> Result<()> {
    bail!("secure random token generation is not implemented on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_get(addr: SocketAddr, path: &str, range: Option<&str>) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if let Some(range) = range {
            request.push_str(&format!("Range: {range}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    #[test]
    fn serves_tokenised_page_and_range_aware_audio() {
        let server =
            IosPlaybackServer::bind("127.0.0.1:0".parse().unwrap(), None, Duration::from_secs(2))
                .unwrap();
        let addr = server.bind_addr();
        let token = server.token.clone();
        let wav = b"RIFF-test-wave".to_vec();
        let handle = std::thread::spawn(move || server.serve(&wav));

        let page = http_get(addr, &format!("/{token}"), None);
        assert!(page.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(page
            .windows(b"<audio".len())
            .any(|window| window == b"<audio"));

        let range = http_get(addr, &format!("/{token}/audio.wav"), Some("bytes=0-3"));
        assert!(range.starts_with(b"HTTP/1.1 206 Partial Content\r\n"));
        assert!(range.ends_with(b"RIFF"));

        let full = http_get(addr, &format!("/{token}/audio.wav"), None);
        assert!(full.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(full.ends_with(b"RIFF-test-wave"));

        handle.join().unwrap().unwrap();
    }

    #[test]
    fn serves_large_audio_without_truncating_the_response() {
        let server =
            IosPlaybackServer::bind("127.0.0.1:0".parse().unwrap(), None, Duration::from_secs(2))
                .unwrap();
        let addr = server.bind_addr();
        let token = server.token.clone();
        let wav = vec![0x5a; 8 * 1024 * 1024];
        let expected_len = wav.len();
        let handle = std::thread::spawn(move || server.serve(&wav));

        let response = http_get(addr, &format!("/{token}/audio.wav"), None);
        handle.join().unwrap().unwrap();

        let body_offset = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|offset| offset + 4)
            .unwrap();
        let body = &response[body_offset..];
        assert_eq!(body.len(), expected_len);
        assert!(body.iter().all(|byte| *byte == 0x5a));
    }

    #[test]
    fn parses_common_byte_ranges() {
        assert_eq!(
            parse_byte_range("bytes=0-9", 100),
            Some(ByteRange {
                start: 0,
                end_inclusive: 9
            })
        );
        assert_eq!(
            parse_byte_range("bytes=90-", 100),
            Some(ByteRange {
                start: 90,
                end_inclusive: 99
            })
        );
        assert_eq!(
            parse_byte_range("bytes=-10", 100),
            Some(ByteRange {
                start: 90,
                end_inclusive: 99
            })
        );
    }

    #[test]
    fn rejects_invalid_or_multiple_ranges() {
        assert_eq!(parse_byte_range("bytes=100-101", 100), None);
        assert_eq!(parse_byte_range("bytes=20-10", 100), None);
        assert_eq!(parse_byte_range("bytes=0-1,3-4", 100), None);
        assert_eq!(parse_byte_range("items=0-1", 100), None);
    }

    #[test]
    fn base_url_normalization_is_strict() {
        assert_eq!(
            normalize_base_url(" http://127.0.0.1:17820/ ").unwrap(),
            "http://127.0.0.1:17820"
        );
        assert!(normalize_base_url("ftp://127.0.0.1").is_err());
        assert!(normalize_base_url("http://").is_err());
        assert!(normalize_base_url("http://127.0.0.1/path?x=1").is_err());
    }

    #[test]
    fn tokens_are_128_bit_hex_values() {
        let token = random_token().unwrap();
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
