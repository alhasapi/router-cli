//! Authenticated HTTP session with the router.
//!
//! The router runs a quirky Boa/0.94 web server over plain HTTP and sends some
//! obsolete folded headers (e.g. `Content-Type: text/html\r\n<spaces>`), which
//! strict HTTP clients reject. We therefore speak a minimal, tolerant HTTP/1.1
//! directly over TCP instead of using a general-purpose HTTP library.
//!
//! Login is challenge–response HMAC-MD5; the session is a single `webuicookie`
//! cookie reused on every subsequent request.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use md5::{Digest, Md5};
use serde::Deserialize;

/// Response from `POST /boafrm/formLoginKey`.
#[derive(Debug, Deserialize)]
struct LoginKeyResponse {
    #[serde(rename = "Challenge")]
    challenge: String,
    #[serde(rename = "PublicKey")]
    public_key: String,
}

/// A logged-in session: host/port + session cookie.
pub struct Session {
    host: String,
    port: u16,
    cookie: String,
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Session {
    /// Perform the full login handshake and return a ready-to-use session.
    pub fn login(base_url: &str, username: &str, password: &str) -> Result<Self> {
        let (host, port) = parse_base_url(base_url)?;

        // Step 1: request a challenge.
        let key_body = format!("username={}", url_encode(username));
        let resp = http_request(
            &host,
            port,
            "POST",
            "/boafrm/formLoginKey",
            &[("Content-Type", "application/x-www-form-urlencoded")],
            key_body.as_bytes(),
        )
        .context("requesting login challenge")?;

        let key: LoginKeyResponse =
            serde_json::from_slice(&resp.body).context("parsing login challenge")?;

        // Step 2: compute the password proof (must match js/hmac_md5.js).
        let private_key = hmac_md5_hex(
            format!("{}{}", key.public_key, password).as_bytes(),
            key.challenge.as_bytes(),
        );
        let login_passwd = hmac_md5_hex(private_key.as_bytes(), key.challenge.as_bytes());

        // Step 3: submit login and capture the session cookie.
        let login_body = format!(
            "username={}&password={}",
            url_encode(username),
            url_encode(&login_passwd)
        );
        let resp = http_request(
            &host,
            port,
            "POST",
            "/boafrm/formLoginSetup",
            &[("Content-Type", "application/x-www-form-urlencoded")],
            login_body.as_bytes(),
        )
        .context("submitting login")?;

        let cookie = get_header(&resp.headers, "set-cookie")
            .ok_or_else(|| anyhow!("login rejected (HTTP {}): no session cookie", resp.status))?;
        // Take just the `name=value` part (before the first `;`).
        let cookie = cookie
            .split(';')
            .next()
            .unwrap_or(cookie)
            .trim()
            .to_string();

        Ok(Session { host, port, cookie })
    }

    /// `POST` a form to a path and return the final response body as text,
    /// following any redirects the server sends.
    pub fn post_form(&self, path: &str, params: &[(&str, &str)]) -> Result<String> {
        let body = form_encode(params);
        self.request_following_redirects(
            "POST",
            path,
            &[("Content-Type", "application/x-www-form-urlencoded")],
            body.as_bytes(),
        )
    }

    /// `GET` a path (usually a `.htm` page) and return the response body as text,
    /// following any redirects the server sends.
    pub fn get(&self, path: &str) -> Result<String> {
        self.request_following_redirects("GET", path, &[], &[])
    }

    fn request_following_redirects(
        &self,
        method: &str,
        path: &str,
        extra_headers: &[(&str, &str)],
        body: &[u8],
    ) -> Result<String> {
        let mut current_path = path.to_string();
        let mut current_method = method.to_string();
        let mut current_body = body.to_vec();

        for _ in 0..6 {
            let mut headers: Vec<(&str, &str)> = vec![("Cookie", self.cookie.as_str())];
            headers.extend_from_slice(extra_headers);
            let resp = http_request(
                &self.host,
                self.port,
                &current_method,
                &current_path,
                &headers,
                &current_body,
            )
            .with_context(|| format!("{current_method} {current_path}"))?;

            if is_redirect(resp.status) {
                if let Some(loc) = get_header(&resp.headers, "location") {
                    current_path = redirect_path(loc);
                    // Routers redirect form POSTs to a GET of the result page.
                    current_method = "GET".to_string();
                    current_body.clear();
                    continue;
                }
            }
            return Ok(String::from_utf8_lossy(&resp.body).into_owned());
        }
        anyhow::bail!("too many redirects while requesting {path}")
    }

    /// Router host (no port).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Router port.
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Poll until a TCP connection to `host:port` succeeds, or `timeout` elapses.
pub fn wait_online(host: &str, port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect((host, port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    false
}

/// Poll until a TCP connection to `host:port` FAILS, or `timeout` elapses.
pub fn wait_offline(host: &str, port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect((host, port)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}

/// Parse `http://host[:port]` into host and port.
fn parse_base_url(url: &str) -> Result<(String, u16)> {
    let url = url.trim_end_matches('/');
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| anyhow!("invalid base URL (expected http://host[:port]): {url}"))?;
    if scheme != "http" {
        return Err(anyhow!("only http:// is supported (got {scheme}://)"));
    }
    let (host, port) = match rest.split_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().context("invalid port")?),
        None => (rest.to_string(), 80),
    };
    if host.is_empty() {
        return Err(anyhow!("invalid base URL: empty host"));
    }
    Ok((host, port))
}

/// Minimal, tolerant HTTP/1.1 request. Sends `Connection: close` and reads the
/// response until the server closes the connection.
fn http_request(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<HttpResponse> {
    let mut stream =
        TcpStream::connect((host, port)).with_context(|| format!("connecting to {host}:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;

    let mut req = String::new();
    req.push_str(&format!("{method} {path} HTTP/1.1\r\n"));
    req.push_str(&format!("Host: {host}\r\n"));
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    req.push_str("Connection: close\r\n\r\n");

    stream
        .write_all(req.as_bytes())
        .context("writing request")?;
    stream.write_all(body).context("writing request body")?;

    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(e) => return Err(e).context("reading response"),
        }
    }

    parse_response(&buf)
}

/// Parse an HTTP/1.1 response, tolerating obsolete folded headers.
fn parse_response(raw: &[u8]) -> Result<HttpResponse> {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("malformed response: missing header terminator"))?;
    let (head, body) = (&raw[..sep], &raw[sep + 4..]);

    let head = String::from_utf8_lossy(head);
    let mut lines = head.split("\r\n");

    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("malformed status line: {status_line:?}"))?;

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            // Obsolete folded continuation line: append to the previous value.
            if let Some((_, v)) = headers.last_mut() {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        if let Some(idx) = line.find(':') {
            let name = line[..idx].trim().to_string();
            let value = line[idx + 1..].trim().to_string();
            headers.push((name, value));
        }
    }

    Ok(HttpResponse {
        status,
        headers,
        body: body.to_vec(),
    })
}

/// Look up a header by name (case-insensitive).
fn get_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Resolve a redirect `Location` header to a path on this host.
fn redirect_path(location: &str) -> String {
    if let Some(rest) = location.split_once("://") {
        // Absolute URL: keep only the path portion.
        return rest
            .1
            .find('/')
            .map(|i| &rest.1[i..])
            .unwrap_or("/")
            .to_string();
    }
    if location.starts_with('/') {
        location.to_string()
    } else {
        format!("/{location}")
    }
}

/// Minimal `application/x-www-form-urlencoded` encoder.
fn form_encode(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// HMAC-MD5 (RFC 2104) returning an uppercase hex string.
///
/// Mirrors `hex_hmac_md5(key, data)` in the router's `js/hmac_md5.js`.
fn hmac_md5_hex(key: &[u8], data: &[u8]) -> String {
    const BLOCK: usize = 64;

    // Key block: if the key is longer than the block size, hash it first.
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = md5_bytes(key);
        key_block[..hashed.len()].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = Vec::with_capacity(BLOCK + data.len());
    let mut opad = Vec::with_capacity(BLOCK + 16);
    for b in key_block {
        ipad.push(b ^ 0x36);
        opad.push(b ^ 0x5c);
    }
    ipad.extend_from_slice(data);

    let inner = md5_bytes(&ipad);
    opad.extend_from_slice(&inner);

    let outer = md5_bytes(&opad);
    hex_upper(&outer)
}

fn md5_bytes(data: &[u8]) -> Vec<u8> {
    let mut hasher = Md5::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

fn hex_upper(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02X}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_md5_matches_rfc2202_vector() {
        // RFC 2202 test case 1: key = 0x0b repeated 16 times, data = "Hi There".
        let key = [0x0bu8; 16];
        assert_eq!(
            hmac_md5_hex(&key, b"Hi There"),
            "9294727A3638BB1C13F48EF8158BFC9D"
        );
    }

    #[test]
    fn url_encoding_handles_ussd_codes() {
        assert_eq!(url_encode("*100#"), "%2A100%23");
        assert_eq!(url_encode("a b"), "a+b");
        assert_eq!(url_encode("admin"), "admin");
    }
}
