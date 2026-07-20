use rand::RngCore;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;

const ENV_CANDIDATES: [&str; 3] = ["codexmanager.env", "CodexManager.env", ".env"];
const DEFAULT_DB_FILENAME: &str = "codexmanager.db";
const DEFAULT_RPC_TOKEN_FILENAME: &str = "codexmanager.rpc-token";
const INSTALLATION_ID_FILENAME: &str = "installation_id";

pub(crate) const ENV_DB_PATH: &str = "CODEXMANAGER_DB_PATH";
pub(crate) const ENV_RPC_TOKEN: &str = "CODEXMANAGER_RPC_TOKEN";
pub(crate) const ENV_RPC_TOKEN_FILE: &str = "CODEXMANAGER_RPC_TOKEN_FILE";

/// 函数 `exe_dir`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 函数 `strip_inline_comment`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - value: 参数 value
///
/// # 返回
/// 返回函数执行结果
fn strip_inline_comment(value: &str) -> &str {
    // Only treat ` #` as comment start (common dotenv behavior).
    let Some(pos) = value.find(" #") else {
        return value;
    };
    value[..pos].trim_end()
}

/// 函数 `parse_dotenv_kv`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - line: 参数 line
///
/// # 返回
/// 返回函数执行结果
fn parse_dotenv_kv(line: &str) -> Option<(String, String)> {
    let mut line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return None;
    }
    if let Some(rest) = line.strip_prefix("export ") {
        line = rest.trim();
    }
    let (key, raw_value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let mut value = raw_value.trim();
    // Handle quoted values: KEY="a b", KEY='a b'
    if (value.starts_with('"') && value.ends_with('"') && value.len() >= 2)
        || (value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2)
    {
        value = &value[1..value.len() - 1];
    } else {
        value = strip_inline_comment(value);
    }
    Some((key.to_string(), value.to_string()))
}

/// 函数 `find_env_file_in_dir`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - dir: 参数 dir
///
/// # 返回
/// 返回函数执行结果
fn find_env_file_in_dir(dir: &Path) -> Option<PathBuf> {
    for name in ENV_CANDIDATES {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 函数 `load_env_from_exe_dir`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 无
pub(crate) fn load_env_from_exe_dir() {
    let dir = exe_dir();
    let Some(path) = find_env_file_in_dir(&dir) else {
        return;
    };

    let Ok(mut f) = fs::File::open(&path) else {
        return;
    };
    let mut text = String::new();
    if f.read_to_string(&mut text).is_err() {
        return;
    }

    let mut applied = 0usize;
    for line in text.lines() {
        let Some((key, value)) = parse_dotenv_kv(line) else {
            continue;
        };
        if std::env::var_os(&key).is_some() {
            continue;
        }
        std::env::set_var(key, value);
        applied += 1;
    }

    if applied > 0 {
        log::info!("Loaded {} env vars from {}", applied, path.display());
    }
}

/// Resolves a fallback proxy for one target URL. Project-level proxy
/// configuration must be checked by the caller before using this fallback.
pub(crate) fn default_proxy_url_for(target_url: &str) -> Option<String> {
    if proxy_bypass_from_env(target_url) {
        return None;
    }
    proxy_url_from_env().or_else(|| system_proxy_url_for(target_url))
}

fn proxy_url_from_env() -> Option<String> {
    [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .and_then(|value| normalize_proxy_candidate(&value))
    })
}

fn normalize_proxy_candidate(raw: &str) -> Option<String> {
    let value = raw.trim().trim_matches('"').trim_matches('\'').trim();
    if value.is_empty() {
        return None;
    }
    let normalized = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    let scheme = normalized
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase())?;
    if !matches!(
        scheme.as_str(),
        "http" | "https" | "socks4" | "socks5" | "socks5h"
    ) {
        return None;
    }
    reqwest::Proxy::all(&normalized).ok()?;
    Some(normalized)
}

#[cfg(windows)]
fn system_proxy_url_for(target_url: &str) -> Option<String> {
    if !windows_proxy_enabled() {
        return None;
    }
    if query_windows_internet_setting("ProxyOverride")
        .as_deref()
        .is_some_and(|rules| proxy_bypass_matches_target(target_url, rules))
    {
        return None;
    }
    query_windows_internet_setting("ProxyServer")
        .as_deref()
        .and_then(parse_windows_proxy_server)
}

#[cfg(not(windows))]
fn system_proxy_url_for(_target_url: &str) -> Option<String> {
    None
}

fn proxy_bypass_from_env(target_url: &str) -> bool {
    ["NO_PROXY", "no_proxy"].iter().any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|rules| proxy_bypass_matches_target(target_url, &rules))
    })
}

#[cfg(windows)]
fn windows_proxy_enabled() -> bool {
    query_windows_internet_setting("ProxyEnable")
        .as_deref()
        .is_some_and(parse_windows_proxy_enabled)
}

#[cfg(windows)]
fn query_windows_internet_setting(name: &str) -> Option<String> {
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            name,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_reg_query_value(&String::from_utf8_lossy(&output.stdout), name)
}

#[cfg(windows)]
fn parse_reg_query_value(output: &str, name: &str) -> Option<String> {
    let normalized_name = name.to_ascii_lowercase();
    output.lines().find_map(|line| {
        let trimmed = line.trim();
        if !trimmed.to_ascii_lowercase().starts_with(&normalized_name) {
            return None;
        }
        let parts = trimmed.split_whitespace().collect::<Vec<_>>();
        (parts.len() >= 3).then(|| parts[2..].join(" "))
    })
}

#[cfg(windows)]
fn parse_windows_proxy_enabled(raw: &str) -> bool {
    let value = raw.trim();
    if value.eq_ignore_ascii_case("true") {
        return true;
    }
    if let Some(hex) = value.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).is_ok_and(|parsed| parsed != 0);
    }
    value.parse::<u32>().is_ok_and(|parsed| parsed != 0)
}

fn parse_windows_proxy_server(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }
    if !value.contains('=') {
        return normalize_proxy_candidate(value);
    }

    let mut http_proxy = None;
    let mut socks_proxy = None;
    for part in value.split(';') {
        let Some((scheme, target)) = part.split_once('=') else {
            continue;
        };
        let scheme = scheme.trim().to_ascii_lowercase();
        let target = target.trim();
        if target.is_empty() {
            continue;
        }
        match scheme.as_str() {
            "https" => return normalize_proxy_candidate(target),
            "http" => http_proxy = normalize_proxy_candidate(target),
            "socks" | "socks5" => {
                socks_proxy = if target.contains("://") {
                    normalize_proxy_candidate(target)
                } else {
                    Some(format!("socks5h://{target}"))
                };
            }
            _ => {}
        }
    }
    http_proxy.or(socks_proxy)
}

fn proxy_bypass_matches_target(target_url: &str, raw_rules: &str) -> bool {
    let Some((host, port)) = target_host_and_port(target_url) else {
        return false;
    };
    raw_rules
        .split([',', ';'])
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .any(|rule| proxy_bypass_rule_matches(rule, &host, port))
}

fn target_host_and_port(target_url: &str) -> Option<(String, Option<u16>)> {
    let parsed = url::Url::parse(target_url)
        .or_else(|_| url::Url::parse(&format!("https://{}", target_url.trim_start_matches('/'))));
    let parsed = parsed.ok()?;
    Some((
        parsed
            .host_str()?
            .trim_matches(|ch| matches!(ch, '[' | ']'))
            .to_ascii_lowercase(),
        parsed.port_or_known_default(),
    ))
}

fn proxy_bypass_rule_matches(rule: &str, host: &str, target_port: Option<u16>) -> bool {
    let rule = rule.trim().trim_matches('"').trim_matches('\'').trim();
    if rule == "*" {
        return true;
    }
    if rule.eq_ignore_ascii_case("<local>") {
        return is_local_host(host);
    }

    let authority = rule
        .split_once("://")
        .map(|(_, value)| value)
        .unwrap_or(rule)
        .split('/')
        .next()
        .unwrap_or_default()
        .trim();
    let (host_pattern, rule_port) = split_proxy_bypass_host_port(authority);
    if rule_port.is_some() && rule_port != target_port {
        return false;
    }
    let host_pattern = host_pattern
        .trim_matches(|ch| matches!(ch, '[' | ']' | '.'))
        .to_ascii_lowercase();
    if host_pattern.is_empty() {
        return false;
    }
    if host_pattern.contains('*') {
        return wildcard_matches(&host_pattern, host);
    }
    host == host_pattern || host.ends_with(&format!(".{host_pattern}"))
}

fn split_proxy_bypass_host_port(authority: &str) -> (&str, Option<u16>) {
    if authority.starts_with('[') {
        if let Some(end) = authority.find(']') {
            let host = &authority[1..end];
            let port = authority[end + 1..]
                .strip_prefix(':')
                .and_then(|value| value.parse().ok());
            return (host, port);
        }
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => match port.parse::<u16>() {
            Ok(port) => (host, Some(port)),
            Err(_) => (authority, None),
        },
        _ => (authority, None),
    }
}

fn is_local_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || !host.contains('.')
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let (mut pattern_index, mut value_index) = (0usize, 0usize);
    let (mut star_index, mut star_value_index) = (None, 0usize);
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    while value_index < value.len() {
        if pattern_index < pattern.len() && pattern[pattern_index] == value[value_index] {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_value_index = value_index;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            star_value_index += 1;
            value_index = star_value_index;
        } else {
            return false;
        }
    }
    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

/// 函数 `resolve_path_with_base`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - raw: 参数 raw
/// - base_dir: 参数 base_dir
///
/// # 返回
/// 返回函数执行结果
fn resolve_path_with_base(raw: &str, base_dir: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw.is_empty() {
        return PathBuf::new();
    }
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        return path;
    }
    base_dir.join(path)
}

/// 函数 `ensure_default_db_path`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn ensure_default_db_path() -> PathBuf {
    let dir = exe_dir();
    let resolved = match std::env::var(ENV_DB_PATH) {
        Ok(raw) if !raw.trim().is_empty() => resolve_path_with_base(&raw, &dir),
        _ => dir.join(DEFAULT_DB_FILENAME),
    };
    std::env::set_var(ENV_DB_PATH, resolved.to_string_lossy().as_ref());
    resolved
}

/// 函数 `db_dir`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn db_dir() -> PathBuf {
    let db_path = ensure_default_db_path();
    db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(exe_dir)
}

/// 函数 `rpc_token_file_path`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn resolve_installation_id() -> std::io::Result<String> {
    let codex_home = db_dir();
    fs::create_dir_all(&codex_home)?;
    let path = codex_home.join(INSTALLATION_ID_FILENAME);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;

    let result = resolve_installation_id_from_locked_file(&mut file);
    let unlock_result = file.unlock();
    match (result, unlock_result) {
        (Ok(installation_id), Ok(())) => Ok(installation_id),
        (Err(err), _) => Err(err),
        (Ok(_), Err(err)) => Err(err),
    }
}

fn resolve_installation_id_from_locked_file(file: &mut fs::File) -> std::io::Result<String> {
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    if let Some(existing) = canonical_uuid(contents.trim()) {
        return Ok(existing);
    }

    let installation_id = random_uuid_v4();
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(installation_id.as_bytes())?;
    file.flush()?;
    file.sync_all()?;
    Ok(installation_id)
}

fn canonical_uuid(value: &str) -> Option<String> {
    if value.len() != 36 {
        return None;
    }
    let bytes = value.as_bytes();
    for index in [8, 13, 18, 23] {
        if bytes.get(index).copied() != Some(b'-') {
            return None;
        }
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| !matches!(index, 8 | 13 | 18 | 23) && !byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

fn random_uuid_v4() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

pub(crate) fn rpc_token_file_path() -> PathBuf {
    if let Ok(raw) = std::env::var(ENV_RPC_TOKEN_FILE) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return resolve_path_with_base(trimmed, &exe_dir());
        }
    }
    db_dir().join(DEFAULT_RPC_TOKEN_FILENAME)
}

/// 函数 `read_rpc_token_from_file`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn read_rpc_token_from_file(path: &Path) -> Option<String> {
    let Ok(mut f) = fs::File::open(path) else {
        return None;
    };
    let mut buf = String::new();
    if f.read_to_string(&mut buf).is_err() {
        return None;
    }
    let token = buf.trim();
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// 函数 `read_rpc_token_from_env_or_file`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn read_rpc_token_from_env_or_file() -> Option<String> {
    if let Ok(raw) = std::env::var(ENV_RPC_TOKEN) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    read_rpc_token_from_file(&rpc_token_file_path())
}

/// 尝试把 token 写入 token file（仅在文件不存在或为空时）。
///
/// - 成功写入返回 `None`
/// - 若检测到文件已存在且可读（可能是并发进程刚创建），返回 `Some(existing_token)`，
///   调用方应优先使用返回的 token 以避免多进程启动时 token 不一致。
pub(crate) fn persist_rpc_token_if_missing(token: &str) -> Option<String> {
    let path = rpc_token_file_path();

    // 快路径：文件已存在且非空
    if let Some(existing) = read_rpc_token_from_file(&path) {
        return Some(existing);
    }

    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            log::warn!(
                "persist rpc token failed: {} ({})",
                path.to_string_lossy(),
                err
            );
            return None;
        }
    }

    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            if let Err(err) = f.write_all(token.as_bytes()) {
                log::warn!(
                    "persist rpc token failed: {} ({})",
                    path.to_string_lossy(),
                    err
                );
            }
            None
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            read_rpc_token_from_file(&path)
        }
        Err(err) => {
            log::warn!(
                "persist rpc token failed: {} ({})",
                path.to_string_lossy(),
                err
            );
            None
        }
    }
}

/// 函数 `generate_rpc_token_hex_32bytes`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - crate: 参数 crate
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn generate_rpc_token_hex_32bytes() -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    token
}

#[cfg(test)]
#[path = "process_env_tests.rs"]
mod tests;
