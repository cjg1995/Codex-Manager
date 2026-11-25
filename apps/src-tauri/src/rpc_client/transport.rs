use codexmanager_core::rpc::types::JsonRpcRequest;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use super::address::{resolve_service_addr, resolve_socket_addrs};
use super::http::parse_http_body;

const RPC_CONNECT_TIMEOUT: Duration = Duration::from_millis(400);
const RPC_DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(10);
const RPC_BULK_USAGE_REFRESH_IO_TIMEOUT: Duration = Duration::from_secs(600);
const RPC_ACCOUNT_IMPORT_IO_TIMEOUT: Duration = Duration::from_secs(600);
const RPC_CODEX_SWITCH_IO_TIMEOUT: Duration = Duration::from_secs(120);

fn is_codex_desktop_switch(method: &str, params: Option<&serde_json::Value>) -> bool {
    method == "codexProfile/applyDirectAccount"
        && params
            .and_then(|value| value.get("launchApp"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

/// 函数 `rpc_io_timeout`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - method: 参数 method
/// - params: 参数 params
///
/// # 返回
/// 返回函数执行结果
fn rpc_io_timeout(method: &str, params: Option<&serde_json::Value>) -> Duration {
    if is_codex_desktop_switch(method, params) {
        return RPC_CODEX_SWITCH_IO_TIMEOUT;
    }

    if method == "account/import" {
        return RPC_ACCOUNT_IMPORT_IO_TIMEOUT;
    }

    if method == "account/usage/refresh"
        && params
            .and_then(|value| value.get("accountId"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return RPC_BULK_USAGE_REFRESH_IO_TIMEOUT;
    }

    RPC_DEFAULT_IO_TIMEOUT
}

/// 函数 `rpc_call_on_stream`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// - method: 参数 method
/// - addr: 参数 addr
/// - sock: 参数 sock
/// - stream: 已连接的服务套接字
/// - params: 参数 params
///
/// # 返回
/// 返回函数执行结果
fn rpc_call_on_stream(
    method: &str,
    addr: &str,
    sock: SocketAddr,
    mut stream: TcpStream,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let io_timeout = rpc_io_timeout(method, params.as_ref());
    let _ = stream.set_read_timeout(Some(io_timeout));
    let _ = stream.set_write_timeout(Some(io_timeout));

    let req = JsonRpcRequest {
        id: 1.into(),
        method: method.to_string(),
        params,
        trace: None,
    };
    let json = serde_json::to_string(&req).map_err(|e| e.to_string())?;
    let rpc_token = codexmanager_service::rpc_auth_token();
    let http = format!(
        "POST /rpc HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nX-CodexManager-Rpc-Token: {rpc_token}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        json.len(),
        json
    );
    stream.write_all(http.as_bytes()).map_err(|e| {
        let msg = e.to_string();
        log::warn!(
            "rpc write failed ({} -> {} via {}): {}",
            method,
            addr,
            sock,
            msg
        );
        msg
    })?;

    let mut buf = String::new();
    stream.read_to_string(&mut buf).map_err(|e| {
        let msg = e.to_string();
        log::warn!(
            "rpc read failed ({} -> {} via {}): {}",
            method,
            addr,
            sock,
            msg
        );
        msg
    })?;
    let body = parse_http_body(&buf).map_err(|msg| {
        log::warn!(
            "rpc parse failed ({} -> {} via {}): {}",
            method,
            addr,
            sock,
            msg
        );
        msg
    })?;
    if body.trim().is_empty() {
        log::warn!("rpc empty response ({} -> {} via {})", method, addr, sock);
        return Err(
            "Empty response from service (service not ready, exited, or port occupied)".to_string(),
        );
    }

    let v: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        let msg = format!("Unexpected RPC response (non-JSON body): {e}");
        log::warn!(
            "rpc json parse failed ({} -> {} via {}): {}",
            method,
            addr,
            sock,
            msg
        );
        msg
    })?;
    if let Some(err) = v.get("error") {
        log::warn!("rpc error ({} -> {} via {}): {}", method, addr, sock, err);
    }
    Ok(v)
}

/// 函数 `rpc_call_with_sockets`
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
pub(crate) fn rpc_call_with_sockets(
    method: &str,
    addr: &str,
    socket_addrs: &[SocketAddr],
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    if socket_addrs.is_empty() {
        return Err(format!(
            "Invalid service address {addr}: no address resolved"
        ));
    }
    let mut last_err =
        "Empty response from service (service not ready, exited, or port occupied)".to_string();
    let is_desktop_switch = is_codex_desktop_switch(method, params.as_ref());
    for attempt in 0..=1 {
        for sock in socket_addrs {
            let stream = match TcpStream::connect_timeout(sock, RPC_CONNECT_TIMEOUT) {
                Ok(stream) => stream,
                Err(err) => {
                    log::warn!(
                        "rpc connect failed ({} -> {} via {}): {}",
                        method,
                        addr,
                        sock,
                        err
                    );
                    last_err = format!("Failed to connect to service at {addr}: {err}");
                    continue;
                }
            };
            match rpc_call_on_stream(method, addr, *sock, stream, params.clone()) {
                Ok(v) => return Ok(v),
                // A lost response does not cancel the service's close/write/open sequence.
                Err(err) if is_desktop_switch => {
                    return Err(format!("未收到切换结果，原切换可能仍在执行。{err}"));
                }
                Err(err) => last_err = err,
            }
        }
        if attempt == 0 {
            std::thread::sleep(Duration::from_millis(120));
        }
    }
    Err(last_err)
}

/// 函数 `rpc_call`
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
pub(crate) fn rpc_call(
    method: &str,
    addr: Option<String>,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let addr = resolve_service_addr(addr)?;
    let socket_addrs = resolve_socket_addrs(&addr)?;
    rpc_call_with_sockets(method, &addr, &socket_addrs, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn codex_desktop_switch_waits_longer_than_a_regular_rpc() {
        assert_eq!(
            rpc_io_timeout(
                "codexProfile/applyDirectAccount",
                Some(&serde_json::json!({ "launchApp": true })),
            ),
            RPC_CODEX_SWITCH_IO_TIMEOUT
        );
        assert!(RPC_CODEX_SWITCH_IO_TIMEOUT > RPC_DEFAULT_IO_TIMEOUT);
        assert_eq!(
            rpc_io_timeout("codexProfile/applyDirectAccount", None),
            RPC_DEFAULT_IO_TIMEOUT
        );
    }

    #[test]
    fn codex_desktop_switch_does_not_resend_after_a_lost_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind primary listener");
        let fallback = TcpListener::bind("127.0.0.1:0").expect("bind fallback listener");
        let address = listener.local_addr().expect("primary address");
        let fallback_address = fallback.local_addr().expect("fallback address");
        let primary_server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept switch request");
            assert!(stream.read(&mut [0; 1024]).expect("read switch request") > 0);
            // The service received the request but closes without returning a result.
        });
        let fallback_server = std::thread::spawn(move || {
            let (mut stream, _) = fallback.accept().expect("accept fallback connection");
            stream
                .read(&mut [0; 1024])
                .expect("read fallback connection")
                > 0
        });

        let result = rpc_call_with_sockets(
            "codexProfile/applyDirectAccount",
            &address.to_string(),
            &[address, fallback_address],
            Some(serde_json::json!({ "accountId": "account", "launchApp": true })),
        );

        // Wake the fallback listener with an empty connection if it got no request.
        drop(TcpStream::connect(fallback_address));
        primary_server.join().expect("primary server");
        assert!(!fallback_server.join().expect("fallback server"));
        assert!(result
            .expect_err("missing RPC response")
            .contains("原切换可能仍在执行"));
    }

    /// 函数 `bulk_usage_refresh_uses_extended_timeout`
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
    fn bulk_usage_refresh_uses_extended_timeout() {
        let timeout = rpc_io_timeout("account/usage/refresh", None);
        assert_eq!(timeout, RPC_BULK_USAGE_REFRESH_IO_TIMEOUT);
    }

    /// 函数 `single_usage_refresh_keeps_default_timeout`
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
    fn single_usage_refresh_keeps_default_timeout() {
        let timeout = rpc_io_timeout(
            "account/usage/refresh",
            Some(&serde_json::json!({ "accountId": "acc-1" })),
        );
        assert_eq!(timeout, RPC_DEFAULT_IO_TIMEOUT);
    }

    /// 函数 `unrelated_rpc_keeps_default_timeout`
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
    fn unrelated_rpc_keeps_default_timeout() {
        let timeout = rpc_io_timeout("account/list", None);
        assert_eq!(timeout, RPC_DEFAULT_IO_TIMEOUT);
    }
}
