//! Bounded public Web capture with SSRF-safe DNS and redirect handling.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use encoding_rs::Encoding;
use lumi_core::{
    content_hash, extract_web_snapshot_fields, now_timestamp_ms, snapshot_artifact_checksum,
    ImportDiagnostic, RenderedPageSnapshot, WebRedirectHop,
};
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, CONTENT_TYPE, LOCATION};
use reqwest::{redirect::Policy, Client};
use url::{Host, Url};

const MAX_URL_BYTES: usize = 2_048;
const MAX_REDIRECTS: usize = 4;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_HEADER_BYTES: usize = 32 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Transport-independent Web capture boundary used by durable imports.
#[async_trait]
pub(crate) trait WebCapture: Send + Sync {
    async fn capture(&self, raw_url: &str) -> Result<RenderedPageSnapshot, WebFetchError>;
}

/// Production bounded raw HTTP capture and opt-in committed-fixture provider.
#[derive(Clone)]
pub(crate) struct BoundedWebFetcher {
    fixture_root: Option<PathBuf>,
    resolver: Arc<dyn HostResolver>,
}

impl BoundedWebFetcher {
    pub(crate) fn from_env() -> Self {
        Self {
            fixture_root: std::env::var_os("LUMI_WEB_FIXTURE_ROOT").map(PathBuf::from),
            resolver: Arc::new(SystemResolver),
        }
    }

    #[cfg(test)]
    fn production() -> Self {
        Self {
            fixture_root: None,
            resolver: Arc::new(SystemResolver),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixtures(root: PathBuf) -> Self {
        Self {
            fixture_root: Some(root),
            resolver: Arc::new(SystemResolver),
        }
    }

    async fn capture_inner(&self, raw_url: &str) -> Result<RenderedPageSnapshot, WebFetchError> {
        let original = normalize_public_url(raw_url)?;
        if original.host_str() == Some("fixtures.lumi.test") {
            return self.capture_fixture(original).await;
        }
        let mut current = original.clone();
        let mut redirect_chain = Vec::new();
        loop {
            let addresses = resolve_and_validate(&current, self.resolver.as_ref()).await?;
            let response = pinned_client(&current, addresses[0])?
                .get(current.clone())
                .header(ACCEPT, "text/html,application/xhtml+xml")
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(|_| WebFetchError::Network)?;
            validate_headers(response.headers())?;
            if response.status().is_redirection() {
                if redirect_chain.len() >= MAX_REDIRECTS {
                    return Err(WebFetchError::RedirectLimit);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or(WebFetchError::InvalidRedirect)?
                    .to_str()
                    .map_err(|_| WebFetchError::InvalidRedirect)?;
                if location.len() > MAX_URL_BYTES {
                    return Err(WebFetchError::InvalidRedirect);
                }
                let next = normalize_public_url(
                    current
                        .join(location)
                        .map_err(|_| WebFetchError::InvalidRedirect)?
                        .as_str(),
                )?;
                // Resolve and validate now, then repeat at request time. The
                // request itself is pinned to the second validated result.
                resolve_and_validate(&next, self.resolver.as_ref()).await?;
                redirect_chain.push(WebRedirectHop {
                    from_url: current.to_string(),
                    status: response.status().as_u16(),
                    to_url: next.to_string(),
                });
                current = next;
                continue;
            }
            if !response.status().is_success() {
                return Err(WebFetchError::HttpStatus(response.status().as_u16()));
            }
            let (content_type, charset) = validate_html_content_type(response.headers())?;
            let status = response.status().as_u16();
            let body = read_bounded_body(response).await?;
            let html = decode_html(&body, &charset)?;
            return tokio::task::spawn_blocking(move || {
                assemble_snapshot(
                    original,
                    current,
                    redirect_chain,
                    status,
                    content_type,
                    charset,
                    html,
                    "bounded-raw-fetch",
                )
            })
            .await
            .map_err(|_| WebFetchError::Snapshot)?;
        }
    }

    async fn capture_fixture(&self, url: Url) -> Result<RenderedPageSnapshot, WebFetchError> {
        let root = self
            .fixture_root
            .as_ref()
            .ok_or(WebFetchError::FixtureProviderDisabled)?;
        let slug = url.path().trim_matches('/');
        if slug.is_empty()
            || !slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(WebFetchError::InvalidUrl);
        }
        let path = root.join(format!("{slug}.html"));
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| WebFetchError::FixtureNotFound)?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(WebFetchError::ResponseTooLarge);
        }
        let html = String::from_utf8(bytes).map_err(|_| WebFetchError::UnsupportedCharset)?;
        tokio::task::spawn_blocking(move || {
            assemble_snapshot(
                url.clone(),
                url,
                Vec::new(),
                200,
                "text/html".to_owned(),
                "utf-8".to_owned(),
                html,
                "committed-fixture",
            )
        })
        .await
        .map_err(|_| WebFetchError::Snapshot)?
    }
}

#[async_trait]
impl WebCapture for BoundedWebFetcher {
    async fn capture(&self, raw_url: &str) -> Result<RenderedPageSnapshot, WebFetchError> {
        tokio::time::timeout(TOTAL_TIMEOUT, self.capture_inner(raw_url))
            .await
            .map_err(|_| WebFetchError::TotalTimeout)?
    }
}

/// Safe Web capture failures mapped to redacted import diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum WebFetchError {
    #[error("URL must be an unambiguous public HTTP or HTTPS address")]
    InvalidUrl,
    #[error("URL credentials are forbidden")]
    CredentialsForbidden,
    #[error("URL resolves to a non-public network address")]
    NonPublicAddress,
    #[error("DNS resolution failed")]
    Dns,
    #[error("remote request failed")]
    Network,
    #[error("web capture exceeded the total timeout")]
    TotalTimeout,
    #[error("web capture exceeded the redirect limit")]
    RedirectLimit,
    #[error("redirect destination is invalid")]
    InvalidRedirect,
    #[error("remote server returned HTTP {0}")]
    HttpStatus(u16),
    #[error("response headers exceed the configured limit")]
    HeadersTooLarge,
    #[error("response is not HTML")]
    UnsupportedContentType,
    #[error("response charset is unsupported")]
    UnsupportedCharset,
    #[error("response exceeds the configured byte limit")]
    ResponseTooLarge,
    #[error("committed fixture provider is disabled")]
    FixtureProviderDisabled,
    #[error("committed fixture was not found")]
    FixtureNotFound,
    #[error("snapshot serialization failed")]
    Snapshot,
}

impl WebFetchError {
    pub(crate) fn diagnostic(&self) -> ImportDiagnostic {
        ImportDiagnostic {
            severity: lumi_core::DiagnosticSeverity::Error,
            code: match self {
                Self::InvalidUrl | Self::CredentialsForbidden => "web_invalid_url",
                Self::NonPublicAddress => "web_ssrf_blocked",
                Self::Dns => "web_dns_failed",
                Self::Network | Self::TotalTimeout => "web_fetch_failed",
                Self::RedirectLimit | Self::InvalidRedirect => "web_redirect_rejected",
                Self::HttpStatus(_) => "web_http_status",
                Self::HeadersTooLarge | Self::ResponseTooLarge => "web_response_limit",
                Self::UnsupportedContentType | Self::UnsupportedCharset => {
                    "web_response_unsupported"
                }
                Self::FixtureProviderDisabled | Self::FixtureNotFound => "web_fixture_unavailable",
                Self::Snapshot => "web_snapshot_failed",
            }
            .to_owned(),
            message: self.to_string(),
            source_path: None,
        }
    }
}

fn normalize_public_url(raw: &str) -> Result<Url, WebFetchError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_URL_BYTES || trimmed.chars().any(char::is_control)
    {
        return Err(WebFetchError::InvalidUrl);
    }
    let mut url = Url::parse(trimmed).map_err(|_| WebFetchError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(WebFetchError::InvalidUrl);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(WebFetchError::CredentialsForbidden);
    }
    if let Some(host) = url.host_str() {
        let lowered = host.trim_end_matches('.').to_ascii_lowercase();
        if lowered != host.to_ascii_lowercase()
            || matches!(
                lowered.as_str(),
                "localhost" | "metadata.google.internal" | "metadata"
            )
        {
            return Err(WebFetchError::InvalidUrl);
        }
    }
    url.set_fragment(None);
    Ok(url)
}

pub(crate) fn validate_public_url_input(raw: &str) -> Result<String, WebFetchError> {
    normalize_public_url(raw).map(Into::into)
}

#[async_trait]
trait HostResolver: Send + Sync {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, WebFetchError>;
}

struct SystemResolver;

#[async_trait]
impl HostResolver for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, WebFetchError> {
        tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| WebFetchError::Dns)
            .map(|addresses| {
                addresses
                    .map(|socket| socket.ip())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect()
            })
    }
}

async fn resolve_and_validate(
    url: &Url,
    resolver: &dyn HostResolver,
) -> Result<Vec<IpAddr>, WebFetchError> {
    let host = url.host().ok_or(WebFetchError::InvalidUrl)?;
    let addresses = match host {
        Host::Ipv4(address) => vec![IpAddr::V4(address)],
        Host::Ipv6(address) => vec![IpAddr::V6(address)],
        Host::Domain(domain) => {
            let port = url
                .port_or_known_default()
                .ok_or(WebFetchError::InvalidUrl)?;
            resolver.resolve(domain, port).await?
        }
    };
    validate_resolved_addresses(&addresses)?;
    Ok(addresses)
}

fn validate_resolved_addresses(addresses: &[IpAddr]) -> Result<(), WebFetchError> {
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(*address)) {
        return Err(WebFetchError::NonPublicAddress);
    }
    Ok(())
}

fn pinned_client(url: &Url, address: IpAddr) -> Result<Client, WebFetchError> {
    let host = url.host_str().ok_or(WebFetchError::InvalidUrl)?;
    let port = url
        .port_or_known_default()
        .ok_or(WebFetchError::InvalidUrl)?;
    Client::builder()
        .no_proxy()
        .redirect(Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .user_agent("Lumi/0.1 bounded-web-capture")
        .resolve(host, SocketAddr::new(address, port))
        .build()
        .map_err(|_| WebFetchError::Network)
}

fn validate_headers(headers: &reqwest::header::HeaderMap) -> Result<(), WebFetchError> {
    let bytes = headers
        .iter()
        .map(|(name, value)| name.as_str().len().saturating_add(value.as_bytes().len()))
        .sum::<usize>();
    if headers.len() > MAX_HEADERS || bytes > MAX_HEADER_BYTES {
        Err(WebFetchError::HeadersTooLarge)
    } else {
        Ok(())
    }
}

fn validate_html_content_type(
    headers: &reqwest::header::HeaderMap,
) -> Result<(String, String), WebFetchError> {
    let value = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(WebFetchError::UnsupportedContentType)?;
    if value.len() > 256 {
        return Err(WebFetchError::UnsupportedContentType);
    }
    let mut parts = value.split(';');
    let media_type = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    if !matches!(media_type.as_str(), "text/html" | "application/xhtml+xml") {
        return Err(WebFetchError::UnsupportedContentType);
    }
    let charset = parts
        .filter_map(|part| part.trim().split_once('='))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("charset"))
        .map_or_else(
            || "utf-8".to_owned(),
            |(_, value)| value.trim_matches([' ', '"', '\'']).to_ascii_lowercase(),
        );
    if !matches!(
        charset.as_str(),
        "utf-8" | "utf8" | "us-ascii" | "windows-1251" | "windows-1252"
    ) {
        return Err(WebFetchError::UnsupportedCharset);
    }
    Ok((media_type, charset))
}

async fn read_bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, WebFetchError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(WebFetchError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| WebFetchError::Network)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(WebFetchError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn decode_html(bytes: &[u8], charset: &str) -> Result<String, WebFetchError> {
    let encoding =
        Encoding::for_label(charset.as_bytes()).ok_or(WebFetchError::UnsupportedCharset)?;
    let (decoded, _, had_errors) = encoding.decode(bytes);
    if had_errors {
        Err(WebFetchError::UnsupportedCharset)
    } else {
        Ok(decoded.into_owned())
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "snapshot provenance is explicit and all fields are independently bounded"
)]
fn assemble_snapshot(
    original: Url,
    final_url: Url,
    redirect_chain: Vec<WebRedirectHop>,
    status: u16,
    content_type: String,
    charset: String,
    html: String,
    provider: &str,
) -> Result<RenderedPageSnapshot, WebFetchError> {
    let fields = extract_web_snapshot_fields(&html, final_url.as_str());
    let mut snapshot = RenderedPageSnapshot {
        original_url: original.to_string(),
        final_url: final_url.to_string(),
        base_url: final_url.to_string(),
        canonical_url: fields.canonical_url,
        redirect_chain,
        status,
        content_type,
        charset,
        captured_at: now_timestamp_ms(),
        capture_provider: provider.to_owned(),
        capture_engine: "reqwest-rustls".to_owned(),
        capture_version: "s1.0".to_owned(),
        text_content: fields.text_content,
        metadata: fields.metadata,
        dom_checksum: content_hash(html.as_bytes()),
        checksum: String::new(),
        rendered_dom: html,
        diagnostics: Vec::new(),
    };
    snapshot.checksum =
        snapshot_artifact_checksum(&snapshot).map_err(|_| WebFetchError::Snapshot)?;
    Ok(snapshot)
}

fn is_public_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, d] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224
        || (a == 255 && b == 255 && c == 255 && d == 255))
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if address.is_unspecified() || address.is_loopback() || address.is_multicast() {
        return false;
    }
    let segments = address.segments();
    if (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x0100 && segments[1..4].iter().all(|segment| *segment == 0))
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 1)
        || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0)
        || segments[0] == 0x5f00
        || (segments[0] == 0x2001 && segments[1] < 0x0200)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
        || segments[0] == 0x2002
    {
        return false;
    }
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_public_ipv4(mapped);
    }
    let octets = address.octets();
    if octets[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        || octets[..12] == [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff]
    {
        return is_public_ipv4(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ));
    }
    if octets[..12] == [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0] {
        return is_public_ipv4(Ipv4Addr::new(
            octets[12], octets[13], octets[14], octets[15],
        ));
    }
    if octets[..6] == [0x00, 0x64, 0xff, 0x9b, 0x00, 0x01] {
        return false;
    }
    // Baseline permits assigned global unicast only. Special translation prefixes
    // are handled above, and every 2000::/3 exception is denied explicitly.
    (segments[0] & 0xe000) == 0x2000
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeResolver {
        answers: Mutex<Vec<Vec<IpAddr>>>,
    }

    #[async_trait]
    impl HostResolver for FakeResolver {
        async fn resolve(&self, _host: &str, _port: u16) -> Result<Vec<IpAddr>, WebFetchError> {
            self.answers
                .lock()
                .map_err(|_| WebFetchError::Dns)?
                .pop()
                .ok_or(WebFetchError::Dns)
        }
    }

    #[test]
    fn normalize_url_rejects_credentials_and_non_http_schemes() {
        assert_eq!(
            normalize_public_url("https://user:secret@example.com"),
            Err(WebFetchError::CredentialsForbidden)
        );
        assert_eq!(
            normalize_public_url("file:///etc/passwd"),
            Err(WebFetchError::InvalidUrl)
        );
    }

    #[test]
    fn normalize_url_strips_fragment() -> Result<(), WebFetchError> {
        let url = normalize_public_url("https://example.com/article#section")?;

        assert_eq!(url.as_str(), "https://example.com/article");
        Ok(())
    }

    #[test]
    fn ssrf_policy_rejects_private_metadata_reserved_and_mapped_addresses() {
        let blocked = [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "192.168.1.1",
            "100.64.0.1",
            "198.51.100.2",
            "224.0.0.1",
            "::1",
            "fc00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::a00:1",
            "fec0::1",
            "::127.0.0.1",
            "100::1",
            "100:0:0:1::1",
            "3fff::1",
            "5f00::1",
            "192.88.99.1",
            "1::1",
            "4000::1",
            "8000::1",
            "2001:db8::1",
        ];

        assert!(blocked.iter().all(|value| value
            .parse::<IpAddr>()
            .is_ok_and(|address| !is_public_ip(address))));
        assert!(is_public_ip(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
    }

    #[test]
    fn content_type_policy_is_bounded_and_html_only() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        assert_eq!(
            validate_html_content_type(&headers),
            Err(WebFetchError::UnsupportedContentType)
        );
    }

    #[test]
    fn production_fetcher_does_not_enable_fixture_provider() {
        assert!(BoundedWebFetcher::production().fixture_root.is_none());
    }

    #[tokio::test]
    async fn resolver_rejects_mixed_public_and_private_answers() -> Result<(), WebFetchError> {
        let resolver = FakeResolver {
            answers: Mutex::new(vec![vec![
                IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            ]]),
        };
        let url = Url::parse("https://example.test/").map_err(|_| WebFetchError::InvalidUrl)?;

        let result = resolve_and_validate(&url, &resolver).await;

        assert_eq!(result, Err(WebFetchError::NonPublicAddress));
        Ok(())
    }

    #[tokio::test]
    async fn every_redirect_hop_can_be_resolved_and_revalidated() -> Result<(), WebFetchError> {
        let resolver = FakeResolver {
            answers: Mutex::new(vec![
                vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))],
                vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))],
            ]),
        };
        let first = Url::parse("https://one.example/").map_err(|_| WebFetchError::InvalidUrl)?;
        let redirected =
            Url::parse("https://two.example/").map_err(|_| WebFetchError::InvalidUrl)?;

        assert!(resolve_and_validate(&first, &resolver).await.is_ok());
        assert_eq!(
            resolve_and_validate(&redirected, &resolver).await,
            Err(WebFetchError::NonPublicAddress)
        );
        Ok(())
    }
}
