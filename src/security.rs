//! Host-header allowlist + CSP header constant.

// CSP allowlist:
//   - esm.sh ............. React/react-dom/htm (ESM modules)
//   - cdn.jsdelivr.net ... Ant Design / dayjs (UMD), antd reset.css
//
// `'unsafe-inline'` on script-src is required for the inline `<script
// type="importmap">` in index.html — browsers don't reliably support a
// remote import map yet. CDN URLs are HTTPS-only and pinned to exact
// package versions; the server is bound to 127.0.0.1 with a strict Host
// allowlist, so the residual injection surface is minimal.
//
// Ant Design's CSS-in-JS runtime injects <style> tags at runtime, hence
// `style-src 'unsafe-inline'`.
pub const CSP_HEADER: &str = "default-src 'self'; \
    script-src 'self' 'unsafe-inline' https://esm.sh https://cdn.jsdelivr.net; \
    style-src 'self' 'unsafe-inline' https://esm.sh https://cdn.jsdelivr.net; \
    font-src 'self' data: https://esm.sh https://cdn.jsdelivr.net; \
    img-src 'self' data:; \
    connect-src 'self' https://esm.sh https://cdn.jsdelivr.net; \
    object-src 'none'; \
    base-uri 'self'";

/// Allow only `localhost` / `127.0.0.1` (with or without the configured port).
pub fn host_allowed(host_header: Option<&str>, port: u16) -> bool {
    let Some(h) = host_header else {
        return false;
    };
    let h = h.trim().to_ascii_lowercase();
    let allow = [
        format!("localhost:{port}"),
        format!("127.0.0.1:{port}"),
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ];
    allow.iter().any(|a| a == &h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_basic() {
        assert!(host_allowed(Some("localhost:8081"), 8081));
        assert!(host_allowed(Some("127.0.0.1:8081"), 8081));
        assert!(host_allowed(Some("LocalHost:8081"), 8081));
        assert!(!host_allowed(Some("evil.example:8081"), 8081));
        assert!(!host_allowed(Some("0.0.0.0:8081"), 8081));
        assert!(!host_allowed(None, 8081));
        assert!(!host_allowed(Some("localhost:9999"), 8081));
    }
}
