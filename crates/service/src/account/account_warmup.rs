use codexmanager_core::storage::{now_ts, Account, Event, RequestLog, Storage, Token};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;
use serde_json::json;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::account_status::mark_account_unavailable_for_auth_error;
use crate::storage_helpers::open_storage;
use crate::usage_account_meta::workspace_header_for_account;
use crate::usage_token_refresh::{refresh_and_persist_access_token, token_refresh_ahead_secs};

const DEFAULT_WARMUP_MESSAGE: &str = "hi";
const FALLBACK_WARMUP_MESSAGE: &str = "你好";
const WARMUP_UPSTREAM_URL: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_WARMUP_MODEL: &str = "gpt-5.3-codex";
const WARMUP_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const AUTO_WARMUP_QUEUE_CAPACITY: usize = 128;

static PENDING_AUTO_WARMUP_TASKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static WARMUP_ACCOUNTS_IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
static AUTO_WARMUP_QUEUE: OnceLock<SyncSender<AutoWarmupTask>> = OnceLock::new();

#[derive(Debug)]
struct AutoWarmupTask {
    account_id: String,
    reason: String,
}

struct WarmupAccountInFlightGuard {
    account_id: String,
}

impl WarmupAccountInFlightGuard {
    fn try_acquire(account_id: &str) -> Option<Self> {
        let account_id = account_id.trim();
        if account_id.is_empty() {
            return None;
        }
        let mutex = WARMUP_ACCOUNTS_IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()));
        let mut in_flight = crate::lock_utils::lock_recover(mutex, "warmup_accounts_in_flight");
        in_flight.insert(account_id.to_string()).then(|| Self {
            account_id: account_id.to_string(),
        })
    }
}

impl Drop for WarmupAccountInFlightGuard {
    fn drop(&mut self) {
        let Some(mutex) = WARMUP_ACCOUNTS_IN_FLIGHT.get() else {
            return;
        };
        let mut in_flight = crate::lock_utils::lock_recover(mutex, "warmup_accounts_in_flight");
        in_flight.remove(&self.account_id);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountWarmupResult {
    pub(crate) requested: usize,
    pub(crate) succeeded: usize,
    pub(crate) failed: usize,
    pub(crate) results: Vec<AccountWarmupItemResult>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountWarmupItemResult {
    pub(crate) account_id: String,
    pub(crate) account_name: String,
    pub(crate) ok: bool,
    pub(crate) message: String,
}

struct AccountWarmupTarget {
    account: Account,
    token: Token,
}

/// 函数 `warmup_accounts`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-14
///
/// # 参数
/// - account_ids: 参数 account_ids
/// - message: 参数 message
///
/// # 返回
/// 返回函数执行结果
pub(crate) fn warmup_accounts(
    account_ids: Vec<String>,
    message: &str,
) -> Result<AccountWarmupResult, String> {
    let storage = open_storage().ok_or_else(|| "storage unavailable".to_string())?;
    let mut accounts = resolve_target_accounts(&storage, &account_ids)?;
    if accounts.is_empty() {
        return Err("no account available for warmup".to_string());
    }

    let warmup_message = normalize_warmup_message(message);
    let warmup_model = resolve_warmup_model_slug(&storage);
    let mut results = Vec::with_capacity(accounts.len());
    let mut succeeded = 0usize;

    for target in accounts.drain(..) {
        let Some(_in_flight_guard) = WarmupAccountInFlightGuard::try_acquire(&target.account.id)
        else {
            results.push(AccountWarmupItemResult {
                account_id: target.account.id,
                account_name: target.account.label,
                ok: false,
                message: "warmup already in progress for this account".to_string(),
            });
            continue;
        };
        let client = match build_warmup_client_for_account(&target.account.id) {
            Ok(client) => client,
            Err(err) => {
                results.push(AccountWarmupItemResult {
                    account_id: target.account.id,
                    account_name: target.account.label,
                    ok: false,
                    message: err,
                });
                continue;
            }
        };
        let item = warmup_single_account(
            &storage,
            &client,
            target,
            warmup_model.as_str(),
            warmup_message.as_str(),
        );
        if item.ok {
            succeeded += 1;
        }
        results.push(item);
    }

    Ok(AccountWarmupResult {
        requested: results.len(),
        succeeded,
        failed: results.len().saturating_sub(succeeded),
        results,
    })
}

pub(crate) fn enqueue_auto_warmup_for_account(account_id: &str, reason: &str) -> bool {
    let id = account_id.trim();
    if id.is_empty() || !mark_auto_warmup_task_pending(id) {
        return false;
    }

    try_enqueue_auto_warmup_task(
        auto_warmup_queue_sender(),
        AutoWarmupTask {
            account_id: id.to_string(),
            reason: reason.trim().to_string(),
        },
    )
}

fn auto_warmup_queue_sender() -> &'static SyncSender<AutoWarmupTask> {
    AUTO_WARMUP_QUEUE.get_or_init(|| {
        let (sender, receiver) = sync_channel(AUTO_WARMUP_QUEUE_CAPACITY);
        if let Err(err) = thread::Builder::new()
            .name("account-auto-warmup-worker".to_string())
            .spawn(move || auto_warmup_worker(receiver))
        {
            log::error!("spawn auto account warmup worker failed: {err}");
        }
        sender
    })
}

fn try_enqueue_auto_warmup_task(sender: &SyncSender<AutoWarmupTask>, task: AutoWarmupTask) -> bool {
    match sender.try_send(task) {
        Ok(()) => true,
        Err(TrySendError::Full(task)) => {
            clear_auto_warmup_task_pending(&task.account_id);
            log::warn!(
                "auto account warmup queue full: account_id={} reason={}",
                task.account_id,
                task.reason
            );
            false
        }
        Err(TrySendError::Disconnected(task)) => {
            clear_auto_warmup_task_pending(&task.account_id);
            log::warn!(
                "auto account warmup queue unavailable: account_id={} reason={}",
                task.account_id,
                task.reason
            );
            false
        }
    }
}

fn auto_warmup_worker(receiver: Receiver<AutoWarmupTask>) {
    while let Ok(task) = receiver.recv() {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            warmup_accounts(vec![task.account_id.clone()], DEFAULT_WARMUP_MESSAGE)
        }));
        match outcome {
            Ok(Ok(result)) => log::info!(
                "auto account warmup completed: account_id={} reason={} succeeded={} failed={}",
                task.account_id,
                task.reason,
                result.succeeded,
                result.failed
            ),
            Ok(Err(err)) => log::warn!(
                "auto account warmup failed: account_id={} reason={} err={}",
                task.account_id,
                task.reason,
                err
            ),
            Err(_) => log::error!(
                "auto account warmup panicked: account_id={} reason={}",
                task.account_id,
                task.reason
            ),
        }
        clear_auto_warmup_task_pending(&task.account_id);
    }
}

fn mark_auto_warmup_task_pending(account_id: &str) -> bool {
    let mutex = PENDING_AUTO_WARMUP_TASKS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut pending = crate::lock_utils::lock_recover(mutex, "pending_auto_warmup_tasks");
    pending.insert(account_id.to_string())
}

fn clear_auto_warmup_task_pending(account_id: &str) {
    let Some(mutex) = PENDING_AUTO_WARMUP_TASKS.get() else {
        return;
    };
    let mut pending = crate::lock_utils::lock_recover(mutex, "pending_auto_warmup_tasks");
    pending.remove(account_id);
}

fn resolve_target_accounts(
    storage: &Storage,
    account_ids: &[String],
) -> Result<Vec<AccountWarmupTarget>, String> {
    if account_ids.is_empty() {
        return storage
            .list_gateway_candidates()
            .map_err(|err| err.to_string())
            .map(gateway_candidate_warmup_targets);
    }

    storage
        .list_gateway_candidates_for_accounts(account_ids)
        .map_err(|err| err.to_string())
        .map(gateway_candidate_warmup_targets)
}

fn gateway_candidate_warmup_targets(candidates: Vec<(Account, Token)>) -> Vec<AccountWarmupTarget> {
    candidates
        .into_iter()
        .map(|(account, token)| AccountWarmupTarget { account, token })
        .collect()
}

fn normalize_warmup_message(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        DEFAULT_WARMUP_MESSAGE.to_string()
    } else {
        trimmed.to_string()
    }
}

fn build_warmup_client_for_account(account_id: &str) -> Result<Client, String> {
    let normalized = account_id.trim();
    if normalized.is_empty() {
        return Err("build warmup client failed: missing account id".to_string());
    }
    crate::gateway::fresh_upstream_client_for_account(normalized)
        .map_err(|err| format!("build warmup client failed: {err}"))
}

fn warmup_single_account(
    storage: &Storage,
    client: &Client,
    target: AccountWarmupTarget,
    model_slug: &str,
    message: &str,
) -> AccountWarmupItemResult {
    let AccountWarmupTarget { account, mut token } = target;
    let account_name = account.label.clone();
    let started_at = Instant::now();
    let mut outcome =
        send_warmup_request_with_fallback(client, &account, &token, model_slug, message);

    if let Err(err) = outcome.as_ref() {
        if should_retry_warmup_with_refresh(&token, err) {
            let issuer = std::env::var("CODEXMANAGER_ISSUER")
                .unwrap_or_else(|_| codexmanager_core::auth::DEFAULT_ISSUER.to_string());
            let client_id = std::env::var("CODEXMANAGER_CLIENT_ID")
                .unwrap_or_else(|_| codexmanager_core::auth::DEFAULT_CLIENT_ID.to_string());
            outcome = refresh_and_persist_access_token(
                storage,
                &mut token,
                &issuer,
                &client_id,
                token_refresh_ahead_secs(),
            )
            .and_then(|_| {
                send_warmup_request_with_fallback(client, &account, &token, model_slug, message)
            });
        }
    }

    match outcome {
        Ok(ok_message) => {
            persist_warmup_observability(
                storage,
                &account,
                200,
                None,
                model_slug,
                started_at.elapsed().as_millis() as i64,
                ok_message.as_str(),
            );
            let _ = crate::usage_refresh::enqueue_usage_refresh_for_account(&account.id);
            AccountWarmupItemResult {
                account_id: account.id,
                account_name,
                ok: true,
                message: ok_message,
            }
        }
        Err(err) => {
            let _ = maybe_mark_account_auth_error(storage, &account.id, &err);
            let status_code = extract_status_code_from_message(&err);
            persist_warmup_observability(
                storage,
                &account,
                status_code,
                Some(err.as_str()),
                model_slug,
                started_at.elapsed().as_millis() as i64,
                "预热失败",
            );
            AccountWarmupItemResult {
                account_id: account.id,
                account_name,
                ok: false,
                message: err,
            }
        }
    }
}

fn persist_warmup_observability(
    storage: &Storage,
    account: &Account,
    status_code: i64,
    error: Option<&str>,
    model_slug: &str,
    duration_ms: i64,
    event_message: &str,
) {
    let created_at = now_ts();
    let trace_id = format!("warmup-{}-{created_at}", account.id);
    let _ = storage.insert_request_log(&RequestLog {
        trace_id: Some(trace_id),
        account_id: Some(account.id.clone()),
        initial_account_id: Some(account.id.clone()),
        attempted_account_ids_json: Some(format!(r#"["{}"]"#, account.id)),
        request_path: "/internal/account/warmup".to_string(),
        original_path: Some("/internal/account/warmup".to_string()),
        adapted_path: Some("/internal/account/warmup".to_string()),
        method: "POST".to_string(),
        request_type: Some("account_warmup".to_string()),
        gateway_mode: None,
        transparent_mode: None,
        enhanced_mode: None,
        model: Some(model_slug.to_string()),
        upstream_url: Some(WARMUP_UPSTREAM_URL.to_string()),
        status_code: Some(status_code),
        duration_ms: Some(duration_ms.max(0)),
        first_response_ms: None,
        error: error.map(str::to_string),
        created_at,
        ..RequestLog::default()
    });
    let _ = storage.insert_event(&Event {
        account_id: Some(account.id.clone()),
        event_type: "account_warmup".to_string(),
        message: match error {
            Some(err) => {
                format!("{event_message}; model={model_slug}; status={status_code}; error={err}")
            }
            None => format!("{event_message}; model={model_slug}; status={status_code}"),
        },
        created_at,
    });
}

fn extract_status_code_from_message(message: &str) -> i64 {
    let marker = "status=";
    let Some(index) = message.find(marker) else {
        return 500;
    };
    let digits: String = message[index + marker.len()..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    digits.parse::<i64>().unwrap_or(500)
}

fn resolve_warmup_model_slug(storage: &Storage) -> String {
    storage
        .list_api_models_v2()
        .ok()
        .and_then(|models| models.into_iter().next().map(|model| model.slug))
        .filter(|slug| !slug.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_WARMUP_MODEL.to_string())
}

fn send_warmup_request_with_fallback(
    client: &Client,
    account: &Account,
    token: &Token,
    model_slug: &str,
    message: &str,
) -> Result<String, String> {
    let primary = send_warmup_request(client, account, token, model_slug, message);
    match primary {
        Ok(()) => Ok("已发送预热消息".to_string()),
        Err(primary_err) if message == DEFAULT_WARMUP_MESSAGE => {
            send_warmup_request(client, account, token, model_slug, FALLBACK_WARMUP_MESSAGE)
                .map(|_| "已发送预热消息".to_string())
                .map_err(|fallback_err| format!("{primary_err}; fallback={fallback_err}"))
        }
        Err(err) => Err(err),
    }
}

fn should_retry_warmup_with_refresh(token: &Token, err: &str) -> bool {
    if token.refresh_token.trim().is_empty() {
        return false;
    }
    let normalized = err.to_ascii_lowercase();
    normalized.contains("status=401")
        || normalized.contains("status=403")
        || normalized.contains("auth error")
        || normalized.contains("unauthorized")
        || normalized.contains("forbidden")
}

fn send_warmup_request(
    client: &Client,
    account: &Account,
    token: &Token,
    model_slug: &str,
    message: &str,
) -> Result<(), String> {
    let body = json!({
        "model": model_slug,
        "instructions": "",
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": message
            }]
        }],
        "stream": true,
        "store": false
    });

    let headers = build_warmup_headers(account, token.access_token.as_str())?;
    let response = client
        .post(WARMUP_UPSTREAM_URL)
        .headers(headers)
        .json(&body)
        .timeout(WARMUP_REQUEST_TIMEOUT)
        .send()
        .map_err(|err| format!("warmup request failed: {err}"))?;

    let status = response.status();
    let headers = response.headers().clone();
    if status.is_success() {
        return consume_warmup_stream(response);
    }

    let body_text = response.text().unwrap_or_default();
    Err(summarize_warmup_error(
        status.as_u16(),
        &headers,
        &body_text,
    ))
}

fn consume_warmup_stream<R: Read>(reader: R) -> Result<(), String> {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();

    loop {
        line.clear();
        let bytes = reader
            .read_line(&mut line)
            .map_err(|err| format!("warmup stream read failed: {err}"))?;
        if bytes == 0 {
            if process_warmup_sse_event(event_name.as_deref(), &data_lines)? {
                return Ok(());
            }
            return Err("warmup stream ended before response.completed".to_string());
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if process_warmup_sse_event(event_name.as_deref(), &data_lines)? {
                return Ok(());
            }
            event_name = None;
            data_lines.clear();
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("data:") {
            data_lines.push(value.trim().to_string());
        }
    }
}

fn process_warmup_sse_event(
    event_name: Option<&str>,
    data_lines: &[String],
) -> Result<bool, String> {
    let event_name = event_name.map(str::trim).filter(|value| !value.is_empty());
    if let Some(event) = event_name {
        if is_warmup_terminal_event(event) {
            return Ok(true);
        }
    }

    if data_lines.is_empty() {
        if let Some(event) = event_name {
            if is_warmup_error_event(event) {
                return Err(format!("warmup stream error event: {event}"));
            }
        }
        return Ok(false);
    }
    let data = data_lines.join("\n");
    let trimmed = data.trim();
    if trimmed == "[DONE]" {
        return Ok(true);
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return Ok(false);
    };
    let event_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .or(event_name);
    if let Some(event_type) = event_type {
        if is_warmup_terminal_event(event_type) {
            return Ok(true);
        }
        if is_warmup_error_event(event_type) {
            return Err(format!(
                "warmup stream error event: {}; {}",
                event_type,
                summarize_warmup_stream_error(&value)
            ));
        }
    }
    if let Some(event) = event_name {
        if is_warmup_error_event(event) {
            return Err(format!("warmup stream error event: {event}"));
        }
    }
    Ok(false)
}

fn is_warmup_terminal_event(value: &str) -> bool {
    matches!(value.trim(), "response.completed" | "response.done")
}

fn is_warmup_error_event(value: &str) -> bool {
    matches!(
        value.trim(),
        "error" | "response.failed" | "response.incomplete"
    )
}

fn summarize_warmup_stream_error(value: &serde_json::Value) -> String {
    value
        .get("error")
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
        })
        .and_then(|error| {
            error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .or_else(|| error.as_str())
        })
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("unknown stream error")
        .to_string()
}

#[cfg(test)]
#[path = "account_warmup_tests.rs"]
mod tests;

fn build_warmup_headers(account: &Account, bearer: &str) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        header_value(&format!("Bearer {bearer}"))?,
    );
    headers.insert(
        reqwest::header::ACCEPT,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        reqwest::header::USER_AGENT,
        header_value(&crate::gateway::current_codex_user_agent())?,
    );
    headers.insert(
        HeaderName::from_static("originator"),
        header_value(&crate::gateway::current_wire_originator())?,
    );

    if let Some(residency_requirement) = crate::gateway::current_residency_requirement() {
        headers.insert(
            HeaderName::from_static("x-openai-internal-codex-residency"),
            header_value(&residency_requirement)?,
        );
    }
    if let Some(account_header) = workspace_header_for_account(account) {
        headers.insert(
            HeaderName::from_static("chatgpt-account-id"),
            header_value(&account_header)?,
        );
    }

    Ok(headers)
}

fn header_value(value: &str) -> Result<HeaderValue, String> {
    HeaderValue::from_str(value).map_err(|err| format!("invalid header value: {err}"))
}

fn summarize_warmup_error(status: u16, headers: &HeaderMap, body: &str) -> String {
    let body_hint =
        crate::gateway::summarize_upstream_error_hint_from_body(status, body.as_bytes())
            .or_else(|| {
                let trimmed = body.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
            .unwrap_or_else(|| "unknown error".to_string());

    let request_id = first_header(headers, &["x-request-id", "x-oai-request-id"]);
    let auth_error = first_header(headers, &["x-openai-authorization-error"]);
    let cf_ray = first_header(headers, &["cf-ray"]);

    let mut details = Vec::new();
    if let Some(value) = request_id {
        details.push(format!("request id: {value}"));
    }
    if let Some(value) = auth_error {
        details.push(format!("auth error: {value}"));
    }
    if let Some(value) = cf_ray {
        details.push(format!("cf-ray: {value}"));
    }

    if details.is_empty() {
        format!("status={status} body={body_hint}")
    } else {
        format!("status={status} body={body_hint}, {}", details.join(", "))
    }
}

fn first_header(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        headers
            .get(*name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn maybe_mark_account_auth_error(
    storage: &Storage,
    account_id: &str,
    err: &str,
) -> Result<(), String> {
    if err.to_ascii_lowercase().contains("auth error")
        || err.to_ascii_lowercase().contains("status=401")
        || err.to_ascii_lowercase().contains("status=403")
    {
        let _ = mark_account_unavailable_for_auth_error(storage, account_id, err);
    }
    Ok(())
}
