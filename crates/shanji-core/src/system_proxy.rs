use crate::error::{AppError, Result};
use reqwest::Url;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::process::Command;

#[derive(Debug, Clone, Default)]
pub struct SystemProxySettings {
    pub source: &'static str,
    pub http_proxy: Option<Url>,
    pub https_proxy: Option<Url>,
    pub socks_proxy: Option<Url>,
    pub bypass_rules: Vec<String>,
}

impl SystemProxySettings {
    pub fn proxy_for_url(&self, url: &Url) -> Option<Url> {
        if self.should_bypass(url) {
            return None;
        }

        match url.scheme() {
            "http" => self
                .http_proxy
                .clone()
                .or_else(|| self.https_proxy.clone())
                .or_else(|| self.socks_proxy.clone()),
            "https" => self
                .https_proxy
                .clone()
                .or_else(|| self.http_proxy.clone())
                .or_else(|| self.socks_proxy.clone()),
            _ => self
                .socks_proxy
                .clone()
                .or_else(|| self.https_proxy.clone())
                .or_else(|| self.http_proxy.clone()),
        }
    }

    pub fn should_bypass(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return true;
        };

        self.bypass_rules
            .iter()
            .any(|rule| bypass_rule_matches(rule, host))
    }

    pub fn describe(&self) -> String {
        let mut parts = vec![format!("source={}", self.source)];
        if let Some(url) = &self.http_proxy {
            parts.push(format!("http={}", url));
        }
        if let Some(url) = &self.https_proxy {
            parts.push(format!("https={}", url));
        }
        if let Some(url) = &self.socks_proxy {
            parts.push(format!("socks={}", url));
        }
        if !self.bypass_rules.is_empty() {
            parts.push(format!("bypass={}", self.bypass_rules.join(",")));
        }
        parts.join(" ")
    }

    fn has_any_proxy(&self) -> bool {
        self.http_proxy.is_some() || self.https_proxy.is_some() || self.socks_proxy.is_some()
    }
}

pub fn resolve_system_proxy() -> Result<Option<SystemProxySettings>> {
    #[cfg(target_os = "macos")]
    {
        return resolve_macos_system_proxy();
    }

    #[cfg(target_os = "windows")]
    {
        return resolve_windows_system_proxy();
    }

    #[cfg(target_os = "linux")]
    {
        return resolve_linux_system_proxy();
    }

    #[allow(unreachable_code)]
    Ok(None)
}

#[cfg(target_os = "macos")]
fn resolve_macos_system_proxy() -> Result<Option<SystemProxySettings>> {
    let output = run_command("scutil", &["--proxy"])?;
    parse_macos_scutil_proxy_output(&output)
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn resolve_macos_system_proxy() -> Result<Option<SystemProxySettings>> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn resolve_linux_system_proxy() -> Result<Option<SystemProxySettings>> {
    let mode = run_command_optional("gsettings", &["get", "org.gnome.system.proxy", "mode"])?;
    let Some(mode) = mode else {
        return Ok(None);
    };
    parse_linux_gsettings_proxy_mode(mode.trim())
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn resolve_linux_system_proxy() -> Result<Option<SystemProxySettings>> {
    Ok(None)
}

#[cfg(target_os = "windows")]
fn resolve_windows_system_proxy() -> Result<Option<SystemProxySettings>> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let settings = hkcu
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
        .map_err(|e| AppError::Network(format!("读取 Windows 系统代理失败: {e}")))?;

    let proxy_enable: u32 = settings.get_value("ProxyEnable").unwrap_or(0);
    let proxy_server: String = settings.get_value("ProxyServer").unwrap_or_default();
    let proxy_override: String = settings.get_value("ProxyOverride").unwrap_or_default();
    let auto_config_url: String = settings.get_value("AutoConfigURL").unwrap_or_default();
    let auto_detect: u32 = settings.get_value("AutoDetect").unwrap_or(0);

    if proxy_enable == 0 {
        if !auto_config_url.trim().is_empty() || auto_detect != 0 {
            return Err(AppError::Network(
                "Windows 系统代理启用了自动配置或自动检测，当前版本暂不支持".to_string(),
            ));
        }
        return Ok(None);
    }

    let mut config = parse_windows_proxy_server(&proxy_server)?;
    config.source = "windows_registry";
    config.bypass_rules = split_windows_bypass_list(&proxy_override);
    if config.has_any_proxy() {
        Ok(Some(config))
    } else {
        Ok(None)
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
fn resolve_windows_system_proxy() -> Result<Option<SystemProxySettings>> {
    Ok(None)
}

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn parse_macos_scutil_proxy_output(output: &str) -> Result<Option<SystemProxySettings>> {
    let mut config = SystemProxySettings {
        source: "macos_scutil",
        ..Default::default()
    };
    let mut in_exceptions = false;
    let mut auto_config_enabled = false;
    let mut auto_discovery_enabled = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "<dictionary> {" {
            continue;
        }

        if trimmed.starts_with("ExceptionsList : <array> {") {
            in_exceptions = true;
            continue;
        }

        if in_exceptions {
            if trimmed == "}" {
                in_exceptions = false;
                continue;
            }
            if let Some((_, value)) = split_key_value(trimmed) {
                config.bypass_rules.push(value.to_string());
            }
            continue;
        }

        let Some((key, value)) = split_key_value(trimmed) else {
            continue;
        };

        match key {
            "HTTPEnable" if value == "1" => {
                config.http_proxy = parse_host_port_proxy(
                    "http",
                    find_scutil_value(output, "HTTPProxy"),
                    find_scutil_value(output, "HTTPPort"),
                )?;
            }
            "HTTPSEnable" if value == "1" => {
                config.https_proxy = parse_host_port_proxy(
                    "http",
                    find_scutil_value(output, "HTTPSProxy"),
                    find_scutil_value(output, "HTTPSPort"),
                )?;
            }
            "SOCKSEnable" if value == "1" => {
                config.socks_proxy = parse_host_port_proxy(
                    "socks5",
                    find_scutil_value(output, "SOCKSProxy"),
                    find_scutil_value(output, "SOCKSPort"),
                )?;
            }
            "ProxyAutoConfigEnable" if value == "1" => auto_config_enabled = true,
            "ProxyAutoDiscoveryEnable" if value == "1" => auto_discovery_enabled = true,
            _ => {}
        }
    }

    if !config.has_any_proxy() {
        if auto_config_enabled || auto_discovery_enabled {
            return Err(AppError::Network(
                "macOS 系统代理启用了 PAC 或自动发现，当前版本暂不支持".to_string(),
            ));
        }
        return Ok(None);
    }

    Ok(Some(config))
}

#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
fn parse_linux_gsettings_proxy_mode(mode: &str) -> Result<Option<SystemProxySettings>> {
    let normalized = trim_quoted(mode);
    match normalized {
        "none" | "" => Ok(None),
        "manual" => parse_linux_manual_proxy(),
        "auto" => Err(AppError::Network(
            "Linux 系统代理启用了自动配置（PAC），当前版本暂不支持".to_string(),
        )),
        other => Err(AppError::Network(format!(
            "未知的 Linux 系统代理模式: {other}"
        ))),
    }
}

#[cfg(target_os = "linux")]
fn parse_linux_manual_proxy() -> Result<Option<SystemProxySettings>> {
    let use_same_proxy = run_command_optional(
        "gsettings",
        &["get", "org.gnome.system.proxy", "use-same-proxy"],
    )?
    .map(|value| value.trim().to_string())
    .unwrap_or_else(|| "false".to_string());
    let bypass_rules = run_command_optional(
        "gsettings",
        &["get", "org.gnome.system.proxy", "ignore-hosts"],
    )?
    .map(|value| parse_gsettings_list(&value))
    .unwrap_or_default();

    let mut config = SystemProxySettings {
        source: "linux_gsettings",
        bypass_rules,
        ..Default::default()
    };

    let http_host =
        run_command_optional("gsettings", &["get", "org.gnome.system.proxy.http", "host"])?;
    let http_port =
        run_command_optional("gsettings", &["get", "org.gnome.system.proxy.http", "port"])?;
    config.http_proxy = parse_host_port_proxy("http", http_host.as_deref(), http_port.as_deref())?;

    if trim_quoted(&use_same_proxy) == "true" {
        config.https_proxy = config.http_proxy.clone();
    } else {
        let https_host = run_command_optional(
            "gsettings",
            &["get", "org.gnome.system.proxy.https", "host"],
        )?;
        let https_port = run_command_optional(
            "gsettings",
            &["get", "org.gnome.system.proxy.https", "port"],
        )?;
        config.https_proxy =
            parse_host_port_proxy("http", https_host.as_deref(), https_port.as_deref())?;
    }

    let socks_host = run_command_optional(
        "gsettings",
        &["get", "org.gnome.system.proxy.socks", "host"],
    )?;
    let socks_port = run_command_optional(
        "gsettings",
        &["get", "org.gnome.system.proxy.socks", "port"],
    )?;
    config.socks_proxy =
        parse_host_port_proxy("socks5", socks_host.as_deref(), socks_port.as_deref())?;

    if config.has_any_proxy() {
        Ok(Some(config))
    } else {
        Ok(None)
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn parse_linux_manual_proxy() -> Result<Option<SystemProxySettings>> {
    Ok(None)
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn parse_windows_proxy_server(value: &str) -> Result<SystemProxySettings> {
    let mut config = SystemProxySettings::default();
    for segment in value
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if let Some((scheme, address)) = segment.split_once('=') {
            match scheme.trim().to_ascii_lowercase().as_str() {
                "http" => {
                    config.http_proxy = Some(parse_proxy_address("http", address)?);
                }
                "https" => {
                    config.https_proxy = Some(parse_proxy_address("http", address)?);
                }
                "socks" | "socks5" => {
                    config.socks_proxy = Some(parse_proxy_address("socks5", address)?);
                }
                _ => {}
            }
        } else {
            let proxy = parse_proxy_address("http", segment)?;
            config.http_proxy = Some(proxy.clone());
            config.https_proxy = Some(proxy);
        }
    }
    Ok(config)
}

#[cfg_attr(not(any(test, target_os = "windows")), allow(dead_code))]
fn split_windows_bypass_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn parse_host_port_proxy(
    scheme: &str,
    host: Option<&str>,
    port: Option<&str>,
) -> Result<Option<Url>> {
    let host = host.map(trim_quoted).unwrap_or_default();
    let port = port.map(trim_quoted).unwrap_or_default();
    if host.is_empty() || port.is_empty() || port == "0" {
        return Ok(None);
    }
    parse_proxy_address(scheme, &format!("{host}:{port}")).map(Some)
}

fn parse_proxy_address(scheme: &str, address: &str) -> Result<Url> {
    let normalized = address.trim();
    let with_scheme = if normalized.contains("://") {
        normalized.to_string()
    } else {
        format!("{scheme}://{normalized}")
    };
    Url::parse(&with_scheme)
        .map_err(|e| AppError::Network(format!("系统代理地址格式无效: {with_scheme} ({e})")))
}

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn find_scutil_value<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        let (current_key, value) = split_key_value(trimmed)?;
        (current_key == key).then_some(value)
    })
}

#[cfg_attr(not(any(test, target_os = "macos")), allow(dead_code))]
fn split_key_value(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    Some((key.trim(), value.trim()))
}

#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
fn parse_gsettings_list(raw: &str) -> Vec<String> {
    let trimmed = raw.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return Vec::new();
    }

    trimmed
        .split(',')
        .map(trim_quoted)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn trim_quoted(value: &str) -> &str {
    value.trim().trim_matches('\'').trim_matches('"')
}

fn bypass_rule_matches(rule: &str, host: &str) -> bool {
    let rule = rule.trim();
    if rule.is_empty() {
        return false;
    }

    if rule.eq_ignore_ascii_case("<local>") {
        return !host.contains('.');
    }

    if let Some((network, prefix_len)) = parse_cidr(rule) {
        return match host.parse::<IpAddr>() {
            Ok(ip) => cidr_contains(network, prefix_len, ip),
            Err(_) => false,
        };
    }

    if let Some(stripped) = rule.strip_prefix("*.") {
        return host.eq_ignore_ascii_case(stripped)
            || host
                .to_ascii_lowercase()
                .ends_with(&format!(".{}", stripped.to_ascii_lowercase()));
    }

    if let Some(stripped) = rule.strip_prefix('.') {
        return host
            .to_ascii_lowercase()
            .ends_with(&stripped.to_ascii_lowercase());
    }

    if rule.contains('*') {
        let suffix = rule.trim_start_matches('*');
        return host
            .to_ascii_lowercase()
            .ends_with(&suffix.to_ascii_lowercase());
    }

    host.eq_ignore_ascii_case(rule)
}

fn parse_cidr(value: &str) -> Option<(IpAddr, u8)> {
    let (network, prefix_len) = value.split_once('/')?;
    let network = network.parse::<IpAddr>().ok()?;
    let prefix_len = prefix_len.parse::<u8>().ok()?;
    Some((network, prefix_len))
}

fn cidr_contains(network: IpAddr, prefix_len: u8, ip: IpAddr) -> bool {
    match (network, ip) {
        (IpAddr::V4(network), IpAddr::V4(ip)) => cidr_contains_v4(network, prefix_len, ip),
        (IpAddr::V6(network), IpAddr::V6(ip)) => cidr_contains_v6(network, prefix_len, ip),
        _ => false,
    }
}

fn cidr_contains_v4(network: Ipv4Addr, prefix_len: u8, ip: Ipv4Addr) -> bool {
    if prefix_len > 32 {
        return false;
    }
    let mask = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len)
    };
    (u32::from(network) & mask) == (u32::from(ip) & mask)
}

fn cidr_contains_v6(network: Ipv6Addr, prefix_len: u8, ip: Ipv6Addr) -> bool {
    if prefix_len > 128 {
        return false;
    }
    let mask = if prefix_len == 0 {
        0
    } else {
        u128::MAX << (128 - prefix_len)
    };
    (u128::from(network) & mask) == (u128::from(ip) & mask)
}

fn run_command(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| AppError::Network(format!("执行系统代理命令失败: {program} {e}")))?;
    if !output.status.success() {
        return Err(AppError::Network(format!(
            "执行系统代理命令失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg_attr(not(any(test, target_os = "linux")), allow(dead_code))]
fn run_command_optional(program: &str, args: &[&str]) -> Result<Option<String>> {
    match Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => {
            Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
        }
        Ok(_) => Ok(None),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(AppError::Network(format!(
            "执行系统代理命令失败: {program} {err}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_scutil_parser_extracts_manual_proxies() {
        let output = r#"
<dictionary> {
  ExceptionsList : <array> {
    0 : 127.0.0.1
    1 : *.local
  }
  HTTPEnable : 1
  HTTPPort : 7890
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 7891
  HTTPSProxy : 127.0.0.2
  SOCKSEnable : 1
  SOCKSPort : 1080
  SOCKSProxy : 127.0.0.3
}
"#;

        let settings = parse_macos_scutil_proxy_output(output).unwrap().unwrap();
        assert_eq!(
            settings.http_proxy.unwrap().as_str(),
            "http://127.0.0.1:7890/"
        );
        assert_eq!(
            settings.https_proxy.unwrap().as_str(),
            "http://127.0.0.2:7891/"
        );
        assert_eq!(
            settings.socks_proxy.unwrap().as_str(),
            "socks5://127.0.0.3:1080"
        );
        assert_eq!(settings.bypass_rules, vec!["127.0.0.1", "*.local"]);
    }

    #[test]
    fn windows_proxy_parser_supports_per_scheme_values() {
        let settings = parse_windows_proxy_server(
            "http=127.0.0.1:7890;https=127.0.0.1:7891;socks=127.0.0.1:1080",
        )
        .unwrap();
        assert_eq!(
            settings.http_proxy.unwrap().as_str(),
            "http://127.0.0.1:7890/"
        );
        assert_eq!(
            settings.https_proxy.unwrap().as_str(),
            "http://127.0.0.1:7891/"
        );
        assert_eq!(
            settings.socks_proxy.unwrap().as_str(),
            "socks5://127.0.0.1:1080"
        );
    }

    #[test]
    fn bypass_rules_support_cidr_and_wildcards() {
        assert!(bypass_rule_matches("127.0.0.0/8", "127.0.0.1"));
        assert!(bypass_rule_matches("*.local", "api.local"));
        assert!(bypass_rule_matches("<local>", "printer"));
        assert!(!bypass_rule_matches("*.local", "example.com"));
    }

    #[test]
    fn gsettings_list_parser_trims_quotes() {
        assert_eq!(
            parse_gsettings_list("['localhost', '127.0.0.0/8', '*.local']"),
            vec!["localhost", "127.0.0.0/8", "*.local"]
        );
    }
}
