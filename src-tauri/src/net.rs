//! The single proxied HTTP client for ALL product network egress.
//!
//! WebView `fetch()` / `<img>` can't honor the app's Settings → Proxy (only the
//! OS proxy), and only an explicitly-configured `reqwest` client can. So every
//! outbound request that must respect the user's proxy — the cloud JSON API
//! (gallery, telemetry), proxied gallery images (cmnet:// scheme), and anything
//! added later — goes through [`client`].
//!
//! **Pit of success:** this is the only sanctioned way for Rust to make a
//! request, so a new network feature can't accidentally bypass the proxy. The
//! client is cached and rebuilt only when the proxy settings change.

use crate::proxy::ProxySettings;
use parking_lot::Mutex;
use std::time::Duration;

static CLIENT: Mutex<Option<(ProxySettings, reqwest::Client)>> = Mutex::new(None);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A `reqwest::Client` with the current Settings → Proxy applied. Cheap to call
/// repeatedly: returns a cached clone, rebuilding only when the proxy config
/// changed since the last build.
pub fn client() -> reqwest::Client {
    let proxy = crate::config::load().proxy;
    let mut guard = CLIENT.lock();
    if let Some((cached, c)) = guard.as_ref() {
        if *cached == proxy {
            return c.clone();
        }
    }
    let c = build(&proxy);
    *guard = Some((proxy, c.clone()));
    c
}

fn build(proxy: &ProxySettings) -> reqwest::Client {
    let mut b = reqwest::Client::builder().timeout(REQUEST_TIMEOUT);
    if proxy.enabled {
        match proxy.proxy_url() {
            Ok(url) => match reqwest::Proxy::all(&url) {
                Ok(p) => {
                    tracing::info!(module = "net", proxy = %url, "HTTP client using configured proxy");
                    b = b.proxy(p);
                }
                Err(e) => tracing::warn!(module = "net", "proxy url rejected ({e}); going direct"),
            },
            Err(e) => tracing::warn!(module = "net", "proxy enabled but invalid ({e}); going direct"),
        }
    } else {
        // Disabled → ignore any inherited env proxy so the setting and the
        // actual transport agree (mirrors proxy.rs's env discipline).
        b = b.no_proxy();
    }
    b.build().unwrap_or_else(|e| {
        tracing::warn!(module = "net", "client build failed ({e}); using default client");
        reqwest::Client::new()
    })
}
