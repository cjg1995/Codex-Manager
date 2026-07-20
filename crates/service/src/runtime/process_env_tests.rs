use super::*;

#[test]
fn normalize_proxy_candidate_adds_http_scheme_for_host_port() {
    assert_eq!(
        normalize_proxy_candidate("127.0.0.1:7892").as_deref(),
        Some("http://127.0.0.1:7892")
    );
    assert_eq!(
        normalize_proxy_candidate("socks5h://127.0.0.1:7890").as_deref(),
        Some("socks5h://127.0.0.1:7890")
    );
}

#[test]
fn parse_windows_proxy_server_prefers_https_proxy() {
    assert_eq!(
        parse_windows_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7892").as_deref(),
        Some("http://127.0.0.1:7892")
    );
}

#[test]
fn parse_windows_proxy_server_supports_socks_proxy() {
    assert_eq!(
        parse_windows_proxy_server("socks=127.0.0.1:7890").as_deref(),
        Some("socks5h://127.0.0.1:7890")
    );
}

#[test]
fn proxy_bypass_supports_loopback_local_and_wildcard_rules() {
    assert!(proxy_bypass_matches_target(
        "http://localhost:3000/path",
        "<local>"
    ));
    assert!(proxy_bypass_matches_target(
        "http://127.0.0.1:8080/path",
        "127.0.0.1"
    ));
    assert!(proxy_bypass_matches_target(
        "https://api.example.com/v1",
        "*.example.com"
    ));
    assert!(!proxy_bypass_matches_target(
        "https://chatgpt.com/backend-api",
        "localhost;*.example.com;<local>"
    ));
}

#[test]
fn proxy_bypass_respects_rule_ports() {
    assert!(proxy_bypass_matches_target(
        "https://api.example.com:8443/v1",
        "api.example.com:8443"
    ));
    assert!(!proxy_bypass_matches_target(
        "https://api.example.com:9443/v1",
        "api.example.com:8443"
    ));
}

#[test]
fn normalize_proxy_candidate_rejects_unsupported_urls() {
    assert_eq!(normalize_proxy_candidate("ftp://127.0.0.1:21"), None);
}

#[cfg(windows)]
#[test]
fn parse_reg_query_value_reads_value_column() {
    let output = r#"
HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Internet Settings
    ProxyServer    REG_SZ    127.0.0.1:7892
"#;

    assert_eq!(
        parse_reg_query_value(output, "ProxyServer").as_deref(),
        Some("127.0.0.1:7892")
    );
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    /// 函数 `set`
    ///
    /// 作者: gaohongshun
    ///
    /// 时间: 2026-04-02
    ///
    /// # 参数
    /// - key: 参数 key
    /// - value: 参数 value
    ///
    /// # 返回
    /// 返回函数执行结果
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var_os(key);
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    /// 函数 `drop`
    ///
    /// 作者: gaohongshun
    ///
    /// 时间: 2026-04-02
    ///
    /// # 参数
    /// - self: 参数 self
    ///
    /// # 返回
    /// 无
    fn drop(&mut self) {
        if let Some(value) = self.previous.as_ref() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

/// 函数 `ensure_default_db_path_resolves_relative_env_against_exe_dir`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// 无
///
/// # 返回
/// 无
#[test]
fn ensure_default_db_path_resolves_relative_env_against_exe_dir() {
    let _db_guard = EnvGuard::set(ENV_DB_PATH, Some("./data/codexmanager.db"));

    let resolved = ensure_default_db_path();

    assert_eq!(resolved, exe_dir().join("data").join("codexmanager.db"));
    assert_eq!(
        std::env::var(ENV_DB_PATH).ok().as_deref(),
        Some(resolved.to_string_lossy().as_ref())
    );
}

/// 函数 `rpc_token_file_path_resolves_relative_env_against_exe_dir`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// 无
///
/// # 返回
/// 无
#[test]
fn rpc_token_file_path_resolves_relative_env_against_exe_dir() {
    let _db_guard = EnvGuard::set(ENV_DB_PATH, Some("./data/codexmanager.db"));
    let _token_guard = EnvGuard::set(ENV_RPC_TOKEN_FILE, Some("./data/codexmanager.rpc-token"));

    let resolved = rpc_token_file_path();

    assert_eq!(
        resolved,
        exe_dir().join("data").join("codexmanager.rpc-token")
    );
}
