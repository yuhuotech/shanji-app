use crate::config::NetworkConfig;
use crate::error::{AppError, Result};
use crate::system_proxy::{resolve_system_proxy, SystemProxySettings};
use log::info;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, ACCEPT_LANGUAGE};
use reqwest::{Proxy, Url};
use std::net::IpAddr;
use std::time::Duration;

pub const PROXY_MODE_SYSTEM: &str = "system";
pub const PROXY_MODE_CUSTOM: &str = "custom";
pub const PROXY_MODE_DIRECT: &str = "direct";

pub const PROXY_TYPE_HTTP: &str = "http";
pub const PROXY_TYPE_HTTPS: &str = "https";
pub const PROXY_TYPE_SOCKS5: &str = "socks5";
pub const PROXY_TYPE_SOCKS5H: &str = "socks5h";

const APP_USER_AGENT: &str = "shanji-app/0.1 (+https://github.com/yuhuotech/shanji)";
const BASIC_TEST_URL: &str = "http://captive.apple.com";
const GITHUB_TEST_URL: &str = "https://github.com/robots.txt";
const PROXY_SECRET_KEY: &[u8] = b"sh4nj1-pr0xy-0bfusc4t10n-k3y-2026";

pub fn normalize_proxy_mode(value: &str) -> &'static str {
    match value.trim() {
        PROXY_MODE_CUSTOM => PROXY_MODE_CUSTOM,
        PROXY_MODE_DIRECT => PROXY_MODE_DIRECT,
        _ => PROXY_MODE_SYSTEM,
    }
}

pub fn normalize_proxy_type(value: &str) -> &'static str {
    match value.trim().to_ascii_lowercase().as_str() {
        PROXY_TYPE_HTTPS => PROXY_TYPE_HTTPS,
        PROXY_TYPE_SOCKS5 => PROXY_TYPE_SOCKS5,
        PROXY_TYPE_SOCKS5H => PROXY_TYPE_SOCKS5H,
        _ => PROXY_TYPE_HTTP,
    }
}

pub fn encrypt_proxy_password(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ PROXY_SECRET_KEY[i % PROXY_SECRET_KEY.len()])
        .map(|b| format!("{:02x}", b))
        .collect()
}

pub fn decrypt_proxy_password(hex: &str) -> String {
    if hex.len() % 2 != 0 {
        return String::new();
    }

    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect();
    let decrypted: Vec<u8> = bytes
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ PROXY_SECRET_KEY[i % PROXY_SECRET_KEY.len()])
        .collect();
    String::from_utf8(decrypted).unwrap_or_default()
}

pub fn build_async_client(network: Option<&NetworkConfig>) -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .default_headers(default_headers())
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(APP_USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120));

    log_proxy_builder("async", network);
    configure_async_proxy(builder, network)?
        .build()
        .map_err(|e| AppError::Network(format!("Failed to build HTTP client: {}", e)))
}

pub fn build_blocking_client(network: Option<&NetworkConfig>) -> Result<reqwest::blocking::Client> {
    let builder = reqwest::blocking::Client::builder()
        .default_headers(default_headers())
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(APP_USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120));

    log_proxy_builder("blocking", network);
    configure_blocking_proxy(builder, network)?
        .build()
        .map_err(|e| AppError::Network(format!("Failed to build blocking HTTP client: {}", e)))
}

pub fn build_download_blocking_client(
    network: Option<&NetworkConfig>,
) -> Result<reqwest::blocking::Client> {
    let builder = reqwest::blocking::Client::builder()
        .default_headers(default_headers())
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(APP_USER_AGENT)
        .http1_only()
        .connect_timeout(Duration::from_secs(30));

    log_proxy_builder("download", network);
    configure_blocking_proxy(builder, network)?
        .build()
        .map_err(|e| AppError::Network(format!("Failed to build download HTTP client: {}", e)))
}

pub fn test_proxy_connection(network: &NetworkConfig) -> Result<String> {
    info!("proxy test starting: {}", describe_network_config(network));
    let builder = reqwest::blocking::Client::builder()
        .default_headers(default_headers())
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(APP_USER_AGENT)
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(15));

    let client = configure_blocking_proxy(builder, Some(network))?
        .build()
        .map_err(|e| AppError::Network(format!("Failed to build proxy test client: {}", e)))?;

    let basic_ok = send_test_request(&client, BASIC_TEST_URL)?;

    if !basic_ok {
        info!("proxy test failed: target={BASIC_TEST_URL}, reason=non-success-status");
        return Err(AppError::Network("基础网络不可用".to_string()));
    }

    let github_ok = send_test_request(&client, GITHUB_TEST_URL)?;

    if !github_ok {
        info!("proxy test failed: target={GITHUB_TEST_URL}, reason=non-success-status");
        return Err(AppError::Network(
            "基础网络可用 · GitHub 不可达".to_string(),
        ));
    }

    info!("proxy test succeeded: basic_network=true github=true");
    Ok("基础网络 ✓  ·  GitHub ✓".to_string())
}

fn default_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
    );
    headers
}

fn configure_async_proxy(
    builder: reqwest::ClientBuilder,
    network: Option<&NetworkConfig>,
) -> Result<reqwest::ClientBuilder> {
    let network = network.cloned().unwrap_or_default();
    match normalize_proxy_mode(&network.proxy_mode) {
        PROXY_MODE_DIRECT => {
            info!("proxy mode applied: mode=direct, all_requests_bypass_proxy");
            Ok(builder.no_proxy())
        }
        PROXY_MODE_CUSTOM => {
            let proxy_url = custom_proxy_url(&network)?;
            info!(
                "proxy mode applied: mode=custom, proxy={}",
                redact_proxy_url(&proxy_url)
            );
            Ok(builder.no_proxy().proxy(proxy_rule_for_url(proxy_url)))
        }
        _ => apply_async_system_proxy(builder),
    }
}

fn configure_blocking_proxy(
    builder: reqwest::blocking::ClientBuilder,
    network: Option<&NetworkConfig>,
) -> Result<reqwest::blocking::ClientBuilder> {
    let network = network.cloned().unwrap_or_default();
    match normalize_proxy_mode(&network.proxy_mode) {
        PROXY_MODE_DIRECT => {
            info!("proxy mode applied: mode=direct, all_requests_bypass_proxy");
            Ok(builder.no_proxy())
        }
        PROXY_MODE_CUSTOM => {
            let proxy_url = custom_proxy_url(&network)?;
            info!(
                "proxy mode applied: mode=custom, proxy={}",
                redact_proxy_url(&proxy_url)
            );
            Ok(builder.no_proxy().proxy(proxy_rule_for_url(proxy_url)))
        }
        _ => apply_blocking_system_proxy(builder),
    }
}

fn proxy_rule_for_url(proxy_url: Url) -> Proxy {
    Proxy::custom(move |url| {
        if should_bypass_proxy(url) {
            info!("proxy decision: bypass target={}", sanitize_target_url(url));
            None
        } else {
            info!(
                "proxy decision: use_proxy target={} proxy={}",
                sanitize_target_url(url),
                redact_proxy_url(&proxy_url)
            );
            Some(proxy_url.clone())
        }
    })
}

fn system_proxy_rule(settings: SystemProxySettings) -> Proxy {
    Proxy::custom(move |url| {
        if settings.should_bypass(url) {
            info!(
                "system proxy decision: bypass target={} source={} reason=system_bypass_rule",
                sanitize_target_url(url),
                settings.source
            );
            return None;
        }

        match settings.proxy_for_url(url) {
            Some(proxy_url) => {
                info!(
                    "system proxy decision: use_proxy target={} source={} proxy={}",
                    sanitize_target_url(url),
                    settings.source,
                    redact_proxy_url(&proxy_url)
                );
                Some(proxy_url)
            }
            None => {
                info!(
                    "system proxy decision: direct target={} source={} reason=no_matching_system_proxy",
                    sanitize_target_url(url),
                    settings.source
                );
                None
            }
        }
    })
}

fn custom_proxy_url(network: &NetworkConfig) -> Result<Url> {
    let host = network.proxy_host.trim();
    if host.is_empty() {
        return Err(AppError::InvalidInput("请先填写代理地址".to_string()));
    }

    let port_text = network.proxy_port.trim();
    if port_text.is_empty() {
        return Err(AppError::InvalidInput("请先填写代理端口".to_string()));
    }

    let port = port_text
        .parse::<u16>()
        .map_err(|_| AppError::InvalidInput(format!("代理端口无效：{}", port_text)))?;

    let scheme = normalize_proxy_type(&network.proxy_type);
    let mut url = Url::parse(&format!("{scheme}://{host}:{port}"))
        .map_err(|e| AppError::InvalidInput(format!("代理地址格式无效：{}", e)))?;

    let username = network.proxy_username.trim();
    let password = decrypt_proxy_password(&network.proxy_password_encrypted);
    if username.is_empty() && !password.is_empty() {
        return Err(AppError::InvalidInput(
            "设置代理密码前请先填写用户名".to_string(),
        ));
    }

    if !username.is_empty() {
        url.set_username(username)
            .map_err(|_| AppError::InvalidInput("代理用户名包含非法字符".to_string()))?;
        if !password.is_empty() {
            url.set_password(Some(&password))
                .map_err(|_| AppError::InvalidInput("代理密码包含非法字符".to_string()))?;
        }
    }

    Ok(url)
}

fn should_bypass_proxy(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };

    host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn apply_async_system_proxy(builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder> {
    match resolve_system_proxy()? {
        Some(settings) => {
            info!("proxy mode applied: mode=system, {}", settings.describe());
            Ok(builder.no_proxy().proxy(system_proxy_rule(settings)))
        }
        None => {
            info!("proxy mode applied: mode=system, system_proxy=none");
            Ok(builder.no_proxy())
        }
    }
}

fn apply_blocking_system_proxy(
    builder: reqwest::blocking::ClientBuilder,
) -> Result<reqwest::blocking::ClientBuilder> {
    match resolve_system_proxy()? {
        Some(settings) => {
            info!("proxy mode applied: mode=system, {}", settings.describe());
            Ok(builder.no_proxy().proxy(system_proxy_rule(settings)))
        }
        None => {
            info!("proxy mode applied: mode=system, system_proxy=none");
            Ok(builder.no_proxy())
        }
    }
}

fn log_proxy_builder(client_kind: &str, network: Option<&NetworkConfig>) {
    let network = network.cloned().unwrap_or_default();
    info!(
        "building {client_kind} http client with {}",
        describe_network_config(&network)
    );
}

fn describe_network_config(network: &NetworkConfig) -> String {
    let mode = normalize_proxy_mode(&network.proxy_mode);
    match mode {
        PROXY_MODE_CUSTOM => match custom_proxy_url(network) {
            Ok(url) => format!("proxy_mode=custom proxy={}", redact_proxy_url(&url)),
            Err(err) => format!("proxy_mode=custom proxy_config_error={err}"),
        },
        PROXY_MODE_DIRECT => "proxy_mode=direct".to_string(),
        _ => "proxy_mode=system".to_string(),
    }
}

fn redact_proxy_url(url: &Url) -> String {
    let mut redacted = url.clone();
    if redacted.password().is_some() {
        let _ = redacted.set_password(Some("***"));
    }
    redacted.to_string()
}

fn sanitize_target_url(url: &Url) -> String {
    let mut sanitized = url.clone();
    sanitized.set_query(None);
    sanitized.set_fragment(None);
    sanitized.to_string()
}

fn send_test_request(client: &reqwest::blocking::Client, url: &str) -> Result<bool> {
    info!("proxy test request: start target={url}");
    let response = client
        .get(url)
        .send()
        .map_err(|e| AppError::Network(format!("请求 {url} 失败: {e}")))?;
    let status = response.status();
    info!(
        "proxy test request: completed target={} status={}",
        url, status
    );
    Ok(status.is_success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_proxy_values_falls_back_to_defaults() {
        assert_eq!(normalize_proxy_mode(""), PROXY_MODE_SYSTEM);
        assert_eq!(normalize_proxy_mode("custom"), PROXY_MODE_CUSTOM);
        assert_eq!(normalize_proxy_type("SOCKS5"), PROXY_TYPE_SOCKS5);
        assert_eq!(normalize_proxy_type("unknown"), PROXY_TYPE_HTTP);
    }

    #[test]
    fn custom_proxy_url_supports_authentication() {
        let cfg = NetworkConfig {
            proxy_mode: PROXY_MODE_CUSTOM.to_string(),
            proxy_type: PROXY_TYPE_SOCKS5.to_string(),
            proxy_host: "127.0.0.1".to_string(),
            proxy_port: "1080".to_string(),
            proxy_username: "alice".to_string(),
            proxy_password_encrypted: encrypt_proxy_password("secret"),
            proxy_test_status: 0,
            proxy_test_status_text: String::new(),
        };

        let url = custom_proxy_url(&cfg).unwrap();

        assert_eq!(url.as_str(), "socks5://alice:secret@127.0.0.1:1080");
    }

    #[test]
    fn proxy_rule_bypasses_loopback_hosts() {
        let external = Url::parse("https://github.com/robots.txt").unwrap();
        let local = Url::parse("http://localhost:11434/v1/chat/completions").unwrap();

        assert!(!should_bypass_proxy(&external));
        assert!(should_bypass_proxy(&local));
    }

    #[test]
    fn redact_proxy_url_hides_password() {
        let url = Url::parse("socks5://alice:secret@127.0.0.1:1080").unwrap();
        assert_eq!(redact_proxy_url(&url), "socks5://alice:***@127.0.0.1:1080");
    }

    #[test]
    fn sanitize_target_url_drops_query_and_fragment() {
        let url = Url::parse("https://example.com/api?q=secret#anchor").unwrap();
        assert_eq!(sanitize_target_url(&url), "https://example.com/api");
    }
}
