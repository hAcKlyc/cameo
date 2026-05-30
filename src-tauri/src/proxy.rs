//! HTTP/SOCKS5 proxy injection for the Codex sidecar (ported from a sibling
//! Tauri app's `proxy.rs`).
//!
//! GUI-launched apps inherit the Finder/launchd env, not the user's terminal
//! env — so a proxy exported in the shell never reaches the `codex app-server`
//! child. This module lets the user configure one proxy in Settings and inject
//! it reliably into every Codex spawn. Key invariants:
//!
//! 1. Inject **both casings** of HTTP_PROXY / HTTPS_PROXY / NO_PROXY — different
//!    HTTP stacks (reqwest, curl, openssl) read different ones.
//! 2. When ENABLED, set `ALL_PROXY` to the SAME url as HTTP(S)_PROXY. Codex
//!    streams model responses over a WebSocket (`wss://`), and WS transports in
//!    many stacks ignore HTTP(S)_PROXY and only honor ALL_PROXY — so without it
//!    the HTTPS handshake is proxied (works) but the stream goes direct and gets
//!    dropped ("websocket closed by server before response.completed"). Setting
//!    ALL_PROXY to the same value can't shadow HTTP(S)_PROXY (they match), so
//!    we get full coverage without the stale-value footgun. When DISABLED we
//!    strip inherited proxy env so the setting and the sidecar path agree.
//! 3. Always inject `NO_PROXY=localhost,...` — even when disabled — so local
//!    IPC never routes through a system proxy.
//! 4. Fail-safe on invalid config: strip all proxy env rather than leaving a
//!    partial / dangerous state.

use std::net::Ipv6Addr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// Localhost coverage: DNS names + IPv4 loopback range + IPv6 loopback.
const LOCALHOST_NO_PROXY: &str = "localhost,localhost.localdomain,127.0.0.1,127.0.0.0/8,::1,[::1]";

const ALLOWED_PROTOCOLS: &[&str] = &["http", "socks5"];

pub const GOOGLE_CONNECTIVITY_CHECK_URL: &str = "http://connectivitycheck.gstatic.com/generate_204";

const PROBE_TARGET_HOST: &str = "connectivitycheck.gstatic.com";
const PROBE_TARGET_PATH: &str = "/generate_204";
const PROBE_TARGET_PORT: u16 = 80;
const PROBE_CONNECT_TIMEOUT_MS: u64 = 1_500;
const PROBE_HANDSHAKE_TIMEOUT_MS: u64 = 5_000;
const PROBE_RESPONSE_HEAD_MAX_BYTES: usize = 4096;
const PROBE_USER_AGENT: &str = "Cameo-Proxy-Probe/1.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxySettings {
    pub enabled: bool,
    /// "http" | "socks5". The configured proxy is used for HTTP, HTTPS, and WSS
    /// traffic via HTTP(S)_PROXY + ALL_PROXY env vars.
    pub protocol: String,
    pub host: String,
    pub port: u16,
}

impl Default for ProxySettings {
    fn default() -> Self {
        // Mirrors the common local proxy default (Clash/Mihomo mixed port).
        Self {
            enabled: false,
            protocol: "http".into(),
            host: "127.0.0.1".into(),
            port: 7897,
        }
    }
}

impl ProxySettings {
    /// Compose `<protocol>://<host>:<port>` if valid; Err on any validation
    /// failure (empty host, port 0, unknown protocol, URL-shaped host).
    pub fn proxy_url(&self) -> Result<String, &'static str> {
        let proto = self.protocol.trim().to_lowercase();
        if !ALLOWED_PROTOCOLS.contains(&proto.as_str()) {
            return Err("invalid protocol (use http / socks5)");
        }
        let (_, url_host) = proxy_host_parts(&self.host)?;
        if self.port == 0 {
            return Err("port must be 1-65535");
        }
        Ok(format!("{proto}://{url_host}:{port}", port = self.port))
    }
}

fn proxy_host_parts(raw_host: &str) -> Result<(String, String), &'static str> {
    let host = raw_host.trim();
    if host.is_empty() {
        return Err("empty host");
    }
    if host.contains("://") || host.contains('@') || host.contains('/') {
        return Err("host should not contain scheme / path");
    }
    if host.len() > 253 {
        return Err("host too long");
    }
    if host.starts_with('[') || host.ends_with(']') {
        if !(host.starts_with('[') && host.ends_with(']')) {
            return Err("invalid IPv6 host brackets");
        }
        let inner = &host[1..host.len() - 1];
        if inner.parse::<Ipv6Addr>().is_err() {
            return Err("bracketed host must be an IPv6 address");
        }
        return Ok((inner.to_string(), format!("[{inner}]")));
    }
    if host.contains('[') || host.contains(']') {
        return Err("invalid IPv6 host brackets");
    }
    if host.contains(':') {
        if host.parse::<Ipv6Addr>().is_err() {
            return Err("host should not include a port");
        }
        return Ok((host.to_string(), format!("[{host}]")));
    }
    Ok((host.to_string(), host.to_string()))
}

/// Mutate `cmd` so the Codex child inherits the configured proxy (or no proxy +
/// localhost protection when none is configured).
pub fn apply_to_subprocess(cmd: &mut tokio::process::Command, cfg: Option<&ProxySettings>) {
    match cfg {
        Some(cfg) if cfg.enabled => match cfg.proxy_url() {
            Ok(url) => {
                cmd.env("HTTP_PROXY", &url);
                cmd.env("HTTPS_PROXY", &url);
                cmd.env("http_proxy", &url);
                cmd.env("https_proxy", &url);
                // ALL_PROXY (same url) so the wss model stream is proxied too —
                // see invariant 2. Safe to set since it matches HTTP(S)_PROXY.
                cmd.env("ALL_PROXY", &url);
                cmd.env("all_proxy", &url);
                cmd.env("NO_PROXY", LOCALHOST_NO_PROXY);
                cmd.env("no_proxy", LOCALHOST_NO_PROXY);
                tracing::info!(module = "proxy", proxy_url = %url, "proxy injected into codex sidecar env (incl. ALL_PROXY for wss stream)");
            }
            Err(e) => {
                tracing::warn!(
                    module = "proxy",
                    "proxy enabled but invalid ({e}); stripping all proxy env"
                );
                strip_all_proxy_env(cmd);
            }
        },
        _ => {
            strip_all_proxy_env(cmd);
            cmd.env("NO_PROXY", LOCALHOST_NO_PROXY);
            cmd.env("no_proxy", LOCALHOST_NO_PROXY);
        }
    }
}

fn strip_all_proxy_env(cmd: &mut tokio::process::Command) {
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        cmd.env_remove(var);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyProbeResult {
    pub ok: bool,
    pub stage: String,
    pub kind: String,
    pub message: String,
    pub detail: Option<String>,
    pub http_status: Option<u16>,
    pub url: String,
}

fn probe_result(
    ok: bool,
    stage: &str,
    kind: &str,
    message: impl Into<String>,
    detail: Option<String>,
    http_status: Option<u16>,
    url: impl Into<String>,
) -> ProxyProbeResult {
    ProxyProbeResult {
        ok,
        stage: stage.to_string(),
        kind: kind.to_string(),
        message: message.into(),
        detail,
        http_status,
        url: url.into(),
    }
}

/// Probe the proxy endpoint currently shown in Settings. The probe connects to
/// the configured local proxy first, then asks Google's lightweight connectivity
/// endpoint for a 204 response through that proxy.
pub async fn probe_connectivity(protocol: String, host: String, port: u16) -> ProxyProbeResult {
    let protocol = protocol.trim().to_lowercase();
    let host = host.trim().to_string();
    let fallback_proxy_url = format!("{protocol}://{host}:{port}");

    if !matches!(protocol.as_str(), "http" | "socks5") {
        return probe_result(
            false,
            "local_proxy",
            "invalid_proxy",
            "Proxy protocol must be http or socks5",
            None,
            None,
            fallback_proxy_url,
        );
    }

    let cfg = ProxySettings {
        enabled: true,
        protocol: protocol.clone(),
        host: host.clone(),
        port,
    };
    let proxy_url = match cfg.proxy_url() {
        Ok(url) => url,
        Err(e) => {
            return probe_result(
                false,
                "local_proxy",
                "invalid_proxy",
                "Proxy host or port is invalid",
                Some(e.to_string()),
                None,
                fallback_proxy_url,
            )
        }
    };
    let (connect_host, _) = match proxy_host_parts(&host) {
        Ok(parts) => parts,
        Err(e) => {
            return probe_result(
                false,
                "local_proxy",
                "invalid_proxy",
                "Proxy host or port is invalid",
                Some(e.to_string()),
                None,
                proxy_url,
            )
        }
    };

    tracing::info!(
        module = "proxy",
        proxy_url = %proxy_url,
        target = %format!("{PROBE_TARGET_HOST}:{PROBE_TARGET_PORT}"),
        "probing proxy connectivity"
    );

    let stream = match timeout(
        Duration::from_millis(PROBE_CONNECT_TIMEOUT_MS),
        TcpStream::connect((connect_host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            return probe_result(
                false,
                "local_proxy",
                "proxy_unreachable",
                "No proxy is listening at this host and port",
                Some(e.to_string()),
                None,
                proxy_url,
            );
        }
        Err(_) => {
            return probe_result(
                false,
                "local_proxy",
                "timeout",
                "Timed out connecting to the local proxy",
                None,
                None,
                proxy_url,
            );
        }
    };

    match protocol.as_str() {
        "http" => probe_http_proxy_get(stream, &proxy_url).await,
        "socks5" => probe_socks5_proxy_get(stream, &proxy_url).await,
        _ => unreachable!("protocol was validated above"),
    }
}

/// Run a lightweight connectivity diagnostic: direct when proxy is off, or
/// through the configured proxy when proxy is on. This does not prove
/// OpenAI/Codex service reachability.
pub async fn probe_codex_connectivity(cfg: &ProxySettings) -> ProxyProbeResult {
    if cfg.enabled {
        return probe_connectivity(cfg.protocol.clone(), cfg.host.clone(), cfg.port).await;
    }

    tracing::info!(
        module = "proxy",
        url = GOOGLE_CONNECTIVITY_CHECK_URL,
        "probing direct internet connectivity"
    );

    let stream = match timeout(
        Duration::from_millis(PROBE_CONNECT_TIMEOUT_MS),
        TcpStream::connect((PROBE_TARGET_HOST, PROBE_TARGET_PORT)),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => {
            return probe_result(
                false,
                "external_http",
                "internet_unreachable",
                "Cannot reach the internet connectivity check",
                Some(e.to_string()),
                None,
                GOOGLE_CONNECTIVITY_CHECK_URL,
            );
        }
        Err(_) => {
            return probe_result(
                false,
                "external_http",
                "timeout",
                "Timed out reaching the internet connectivity check",
                None,
                None,
                GOOGLE_CONNECTIVITY_CHECK_URL,
            );
        }
    };

    send_generate_204_request(
        stream,
        PROBE_TARGET_PATH,
        "external_http",
        GOOGLE_CONNECTIVITY_CHECK_URL,
    )
    .await
}

async fn write_probe(stream: &mut TcpStream, bytes: &[u8]) -> Result<(), String> {
    match timeout(
        Duration::from_millis(PROBE_HANDSHAKE_TIMEOUT_MS),
        stream.write_all(bytes),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timeout".to_string()),
    }
}

async fn read_probe(stream: &mut TcpStream, buf: &mut [u8]) -> Result<usize, String> {
    match timeout(
        Duration::from_millis(PROBE_HANDSHAKE_TIMEOUT_MS),
        stream.read(buf),
    )
    .await
    {
        Ok(Ok(n)) => Ok(n),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timeout".to_string()),
    }
}

async fn read_http_response_head(stream: &mut TcpStream) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 256];

    loop {
        let n = read_probe(stream, &mut chunk).await?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.windows(2).any(|w| w == b"\r\n")
            || bytes.contains(&b'\n')
            || bytes.len() >= PROBE_RESPONSE_HEAD_MAX_BYTES
        {
            break;
        }
    }

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

async fn read_exact_probe(stream: &mut TcpStream, buf: &mut [u8]) -> Result<(), String> {
    match timeout(
        Duration::from_millis(PROBE_HANDSHAKE_TIMEOUT_MS),
        stream.read_exact(buf),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("timeout".to_string()),
    }
}

async fn send_generate_204_request(
    mut stream: TcpStream,
    request_target: &str,
    stage: &str,
    url: &str,
) -> ProxyProbeResult {
    let request = format!(
        "HEAD {request_target} HTTP/1.1\r\nHost: {PROBE_TARGET_HOST}\r\nUser-Agent: {PROBE_USER_AGENT}\r\nConnection: close\r\n\r\n"
    );

    if let Err(e) = write_probe(&mut stream, request.as_bytes()).await {
        return probe_result(
            false,
            stage,
            "network_error",
            "Could not write the connectivity probe request",
            Some(e),
            None,
            url,
        );
    }

    let response = match read_http_response_head(&mut stream).await {
        Ok(response) if response.is_empty() => {
            return probe_result(
                false,
                stage,
                "network_error",
                "The connectivity probe closed without a response",
                None,
                None,
                url,
            );
        }
        Ok(response) => response,
        Err(e) => {
            return probe_result(
                false,
                stage,
                if e == "timeout" {
                    "timeout"
                } else {
                    "network_error"
                },
                "The connectivity probe did not complete",
                Some(e),
                None,
                url,
            );
        }
    };

    let status = parse_http_status(&response);
    let first_line = first_response_line(&response);

    match status {
        Some(204) => probe_result(
            true,
            stage,
            "internet_reachable",
            "Internet connectivity check returned 204",
            Some(GOOGLE_CONNECTIVITY_CHECK_URL.to_string()),
            Some(204),
            url,
        ),
        Some(407) => probe_result(
            false,
            stage,
            "proxy_auth_required",
            "The proxy requires authentication",
            Some(first_line),
            Some(407),
            url,
        ),
        Some(code) if (300..400).contains(&code) => probe_result(
            false,
            stage,
            "captive_portal",
            "Connectivity check was redirected",
            Some(first_line),
            Some(code),
            url,
        ),
        Some(code) => probe_result(
            false,
            stage,
            "unexpected_status",
            "Connectivity check returned an unexpected HTTP status",
            Some(first_line),
            Some(code),
            url,
        ),
        None => probe_result(
            false,
            stage,
            "protocol_mismatch",
            "Connectivity check response did not look like HTTP",
            Some(first_line),
            None,
            url,
        ),
    }
}

async fn probe_http_proxy_get(stream: TcpStream, proxy_url: &str) -> ProxyProbeResult {
    send_generate_204_request(
        stream,
        GOOGLE_CONNECTIVITY_CHECK_URL,
        "external_http",
        proxy_url,
    )
    .await
}

async fn probe_socks5_proxy_get(mut stream: TcpStream, proxy_url: &str) -> ProxyProbeResult {
    if let Err(e) = write_probe(&mut stream, &[0x05, 0x01, 0x00]).await {
        return probe_result(
            false,
            "local_proxy",
            "protocol_mismatch",
            "Connected to the port, but it did not accept a SOCKS5 greeting",
            Some(e),
            None,
            proxy_url,
        );
    }

    let mut method = [0_u8; 2];
    if let Err(e) = read_exact_probe(&mut stream, &mut method).await {
        return probe_result(
            false,
            "local_proxy",
            if e == "timeout" {
                "timeout"
            } else {
                "protocol_mismatch"
            },
            "The SOCKS5 proxy did not answer the greeting",
            Some(e),
            None,
            proxy_url,
        );
    }
    if method[0] != 0x05 {
        return probe_result(
            false,
            "local_proxy",
            "protocol_mismatch",
            "Connected to the port, but it did not look like SOCKS5",
            Some(format!("version byte: {}", method[0])),
            None,
            proxy_url,
        );
    }
    if method[1] == 0xff {
        return probe_result(
            false,
            "local_proxy",
            "proxy_auth_required",
            "The SOCKS5 proxy requires authentication",
            None,
            None,
            proxy_url,
        );
    }
    if method[1] != 0x00 {
        return probe_result(
            false,
            "local_proxy",
            "proxy_auth_required",
            "The SOCKS5 proxy selected an unsupported auth method",
            Some(format!("method byte: {}", method[1])),
            None,
            proxy_url,
        );
    }

    let host_bytes = PROBE_TARGET_HOST.as_bytes();
    let mut request = Vec::with_capacity(7 + host_bytes.len());
    request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    request.extend_from_slice(host_bytes);
    request.extend_from_slice(&PROBE_TARGET_PORT.to_be_bytes());

    if let Err(e) = write_probe(&mut stream, &request).await {
        return probe_result(
            false,
            "external_connect",
            "upstream_unreachable",
            "The SOCKS5 proxy did not accept the CONNECT request",
            Some(e),
            None,
            proxy_url,
        );
    }

    let mut head = [0_u8; 4];
    if let Err(e) = read_exact_probe(&mut stream, &mut head).await {
        return probe_result(
            false,
            "external_connect",
            if e == "timeout" {
                "timeout"
            } else {
                "upstream_unreachable"
            },
            "The SOCKS5 proxy did not complete the CONNECT probe",
            Some(e),
            None,
            proxy_url,
        );
    }
    if head[0] != 0x05 {
        return probe_result(
            false,
            "local_proxy",
            "protocol_mismatch",
            "Connected to the port, but it did not look like SOCKS5",
            Some(format!("version byte: {}", head[0])),
            None,
            proxy_url,
        );
    }
    if head[1] != 0x00 {
        return probe_result(
            false,
            "external_connect",
            "upstream_unreachable",
            "The SOCKS5 proxy responded but could not reach the target",
            Some(socks5_reply_label(head[1]).to_string()),
            None,
            proxy_url,
        );
    }

    match drain_socks5_bind_addr(&mut stream, head[3]).await {
        Ok(()) => {
            send_generate_204_request(stream, PROBE_TARGET_PATH, "external_http", proxy_url).await
        }
        Err(e) => probe_result(
            false,
            "external_connect",
            "upstream_unreachable",
            "The SOCKS5 proxy returned an incomplete CONNECT response",
            Some(e),
            None,
            proxy_url,
        ),
    }
}

async fn drain_socks5_bind_addr(stream: &mut TcpStream, atyp: u8) -> Result<(), String> {
    let addr_len = match atyp {
        0x01 => 4,
        0x03 => {
            let mut len = [0_u8; 1];
            read_exact_probe(stream, &mut len).await?;
            len[0] as usize
        }
        0x04 => 16,
        _ => return Err(format!("unknown address type: {atyp}")),
    };
    let mut rest = vec![0_u8; addr_len + 2];
    read_exact_probe(stream, &mut rest).await
}

fn parse_http_status(response: &str) -> Option<u16> {
    let mut parts = response.lines().next()?.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse().ok()
}

fn first_response_line(response: &str) -> String {
    response
        .lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(160)
        .collect()
}

fn socks5_reply_label(code: u8) -> &'static str {
    match code {
        0x01 => "general SOCKS server failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unknown SOCKS5 reply",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(enabled: bool, proto: &str, host: &str, port: u16) -> ProxySettings {
        ProxySettings {
            enabled,
            protocol: proto.into(),
            host: host.into(),
            port,
        }
    }

    #[test]
    fn url_compose_http() {
        assert_eq!(
            make(true, "http", "127.0.0.1", 7890).proxy_url().unwrap(),
            "http://127.0.0.1:7890"
        );
    }

    #[test]
    fn url_compose_socks5() {
        assert_eq!(
            make(true, "socks5", "proxy.example.com", 1080)
                .proxy_url()
                .unwrap(),
            "socks5://proxy.example.com:1080"
        );
    }

    #[test]
    fn url_brackets_ipv6_hosts() {
        assert_eq!(
            make(true, "http", "::1", 7890).proxy_url().unwrap(),
            "http://[::1]:7890"
        );
        assert_eq!(
            make(true, "http", "[::1]", 7890).proxy_url().unwrap(),
            "http://[::1]:7890"
        );
    }

    #[test]
    fn url_normalizes_protocol_case() {
        assert_eq!(
            make(true, "HTTP", "127.0.0.1", 7890).proxy_url().unwrap(),
            "http://127.0.0.1:7890"
        );
    }

    #[test]
    fn url_trims_host_whitespace() {
        assert_eq!(
            make(true, "http", "  127.0.0.1  ", 7890)
                .proxy_url()
                .unwrap(),
            "http://127.0.0.1:7890"
        );
    }

    #[test]
    fn url_rejects_empty_host() {
        assert!(make(true, "http", "", 7890).proxy_url().is_err());
    }

    #[test]
    fn url_rejects_unknown_protocol() {
        assert!(make(true, "ftp", "127.0.0.1", 21).proxy_url().is_err());
        assert!(make(true, "https", "127.0.0.1", 7890).proxy_url().is_err());
    }

    #[test]
    fn url_rejects_zero_port() {
        assert!(make(true, "http", "127.0.0.1", 0).proxy_url().is_err());
    }

    #[test]
    fn url_rejects_url_shaped_host() {
        assert!(make(true, "http", "http://127.0.0.1", 8080)
            .proxy_url()
            .is_err());
        assert!(make(true, "http", "user@proxy", 8080).proxy_url().is_err());
        assert!(make(true, "http", "host/path", 8080).proxy_url().is_err());
        assert!(make(true, "http", "127.0.0.1:8080", 8080)
            .proxy_url()
            .is_err());
        assert!(make(true, "http", "[localhost]", 8080).proxy_url().is_err());
    }

    #[test]
    fn http_status_parses_success() {
        assert_eq!(
            parse_http_status("HTTP/1.1 204 No Content\r\n\r\n"),
            Some(204)
        );
    }

    #[test]
    fn http_status_rejects_non_http() {
        assert_eq!(parse_http_status("SOCKS5"), None);
    }

    #[test]
    fn socks5_reply_labels_common_failures() {
        assert_eq!(socks5_reply_label(0x05), "connection refused");
        assert_eq!(socks5_reply_label(0xff), "unknown SOCKS5 reply");
    }
}
