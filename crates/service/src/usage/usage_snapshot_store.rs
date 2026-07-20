use crate::account_availability::{evaluate_snapshot, Availability};
use crate::account_status::{
    load_account_status_context, set_account_status_with_context, AccountStatusContext,
};
use codexmanager_core::storage::{now_ts, Storage, UsageSnapshotRecord};
use codexmanager_core::usage::parse_usage_snapshot;

const DEFAULT_USAGE_SNAPSHOTS_RETAIN_PER_ACCOUNT: usize = 1;
const USAGE_SNAPSHOTS_RETAIN_PER_ACCOUNT_ENV: &str =
    "CODEXMANAGER_USAGE_SNAPSHOTS_RETAIN_PER_ACCOUNT";
const PRIMARY_WINDOW_MINUTES: i64 = 300;
const SECONDARY_WINDOW_MINUTES: i64 = 10_080;
const WINDOW_MATCH_TOLERANCE_MINUTES: i64 = 5;
const RESET_CROSSING_CLOCK_SKEW_TOLERANCE_SECS: i64 = 300;

fn usage_status_updates_blocked(context: &AccountStatusContext) -> bool {
    context.status.trim().eq_ignore_ascii_case("disabled")
}

/// 函数 `usage_snapshots_retain_per_account`
///
/// 作者: gaohongshun
///
/// 时间: 2026-04-02
///
/// # 参数
/// 无
///
/// # 返回
/// 返回函数执行结果
fn usage_snapshots_retain_per_account() -> usize {
    std::env::var(USAGE_SNAPSHOTS_RETAIN_PER_ACCOUNT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(DEFAULT_USAGE_SNAPSHOTS_RETAIN_PER_ACCOUNT)
}

/// 函数 `apply_status_from_snapshot`
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
pub(crate) fn apply_status_from_snapshot(
    storage: &Storage,
    record: &UsageSnapshotRecord,
) -> Availability {
    let availability = evaluate_snapshot(record);
    let context = load_account_status_context(storage, &record.account_id);

    if usage_status_updates_blocked(&context) {
        return availability;
    }

    match availability {
        Availability::Available => {
            set_account_status_with_context(
                storage,
                &record.account_id,
                "active",
                "usage_ok",
                Some(&context),
            );
        }
        Availability::Unavailable("usage_exhausted_primary" | "usage_exhausted_secondary") => {
            set_account_status_with_context(
                storage,
                &record.account_id,
                "limited",
                "usage_limit_exhausted",
                Some(&context),
            );
        }
        Availability::Unavailable(_) => {}
    }
    availability
}

/// 函数 `store_usage_snapshot`
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
pub(crate) fn store_usage_snapshot(
    storage: &Storage,
    account_id: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    // 解析并写入用量快照
    let parsed = parse_usage_snapshot(&value);
    let previous = storage
        .latest_usage_snapshot_for_account(account_id)
        .ok()
        .flatten();
    let record = UsageSnapshotRecord {
        account_id: account_id.to_string(),
        used_percent: parsed.used_percent,
        window_minutes: parsed.window_minutes,
        resets_at: parsed.resets_at,
        secondary_used_percent: parsed.secondary_used_percent,
        secondary_window_minutes: parsed.secondary_window_minutes,
        secondary_resets_at: parsed.secondary_resets_at,
        credits_json: parsed.credits_json,
        captured_at: now_ts(),
    };
    storage
        .insert_usage_snapshot(&record)
        .map_err(|e| e.to_string())?;
    let retain = usage_snapshots_retain_per_account();
    if retain > 0 {
        let _ = storage.prune_usage_snapshots_for_account(account_id, retain);
    }
    let _ = apply_status_from_snapshot(storage, &record);
    maybe_enqueue_auto_warmup_after_quota_refresh(previous.as_ref(), &record);
    Ok(())
}

fn maybe_enqueue_auto_warmup_after_quota_refresh(
    previous: Option<&UsageSnapshotRecord>,
    current: &UsageSnapshotRecord,
) {
    if !crate::usage_refresh::auto_warmup_after_quota_refresh_enabled() {
        return;
    }
    let Some(reason) = auto_warmup_reason_after_quota_refresh(previous, current) else {
        return;
    };
    if crate::account_warmup::enqueue_auto_warmup_for_account(&current.account_id, reason) {
        log::info!(
            "queued auto account warmup after quota refresh: account_id={} reason={}",
            current.account_id,
            reason
        );
    }
}

fn auto_warmup_reason_after_quota_refresh(
    previous: Option<&UsageSnapshotRecord>,
    current: &UsageSnapshotRecord,
) -> Option<&'static str> {
    let previous = previous?;
    if quota_window_refreshed(
        previous.resets_at,
        current.resets_at,
        current.captured_at,
        previous.window_minutes.or(current.window_minutes),
    ) {
        return Some("primary_quota_window_reset");
    }
    if quota_window_refreshed(
        previous.secondary_resets_at,
        current.secondary_resets_at,
        current.captured_at,
        previous
            .secondary_window_minutes
            .or(current.secondary_window_minutes),
    ) {
        return Some("secondary_quota_window_reset");
    }
    None
}

fn quota_window_refreshed(
    previous_resets_at: Option<i64>,
    current_resets_at: Option<i64>,
    current_captured_at: i64,
    window_minutes: Option<i64>,
) -> bool {
    let Some(previous_resets_at) = previous_resets_at.filter(|value| *value > 0) else {
        return false;
    };
    let Some(current_resets_at) = current_resets_at.filter(|value| *value > 0) else {
        return false;
    };
    is_supported_quota_window_minutes(window_minutes)
        && current_captured_at.saturating_add(RESET_CROSSING_CLOCK_SKEW_TOLERANCE_SECS)
            >= previous_resets_at
        && current_resets_at > previous_resets_at
}

fn is_supported_quota_window_minutes(window_minutes: Option<i64>) -> bool {
    window_minutes.is_some_and(|value| {
        [PRIMARY_WINDOW_MINUTES, SECONDARY_WINDOW_MINUTES]
            .into_iter()
            .any(|expected| (value - expected).abs() <= WINDOW_MATCH_TOLERANCE_MINUTES)
    })
}

#[cfg(test)]
mod tests {
    use super::auto_warmup_reason_after_quota_refresh;
    use codexmanager_core::storage::UsageSnapshotRecord;

    fn snapshot(
        captured_at: i64,
        primary_reset: Option<i64>,
        secondary_reset: Option<i64>,
    ) -> UsageSnapshotRecord {
        UsageSnapshotRecord {
            account_id: "acc-1".to_string(),
            used_percent: Some(10.0),
            window_minutes: Some(300),
            resets_at: primary_reset,
            secondary_used_percent: Some(10.0),
            secondary_window_minutes: Some(10_080),
            secondary_resets_at: secondary_reset,
            credits_json: None,
            captured_at,
        }
    }

    #[test]
    fn detects_primary_window_reset_crossing() {
        let previous = snapshot(1_000, Some(1_200), Some(8_000));
        let current = snapshot(1_210, Some(2_000), Some(8_000));

        assert_eq!(
            auto_warmup_reason_after_quota_refresh(Some(&previous), &current),
            Some("primary_quota_window_reset")
        );
    }

    #[test]
    fn detects_secondary_window_reset_crossing() {
        let previous = snapshot(1_000, Some(1_500), Some(1_200));
        let current = snapshot(1_210, Some(1_500), Some(20_000));

        assert_eq!(
            auto_warmup_reason_after_quota_refresh(Some(&previous), &current),
            Some("secondary_quota_window_reset")
        );
    }

    #[test]
    fn detects_long_window_when_reported_as_primary() {
        let mut previous = snapshot(1_000, Some(1_200), None);
        previous.window_minutes = Some(10_080);
        previous.secondary_used_percent = None;
        previous.secondary_window_minutes = None;
        let mut current = snapshot(1_210, Some(20_000), None);
        current.window_minutes = Some(10_080);
        current.secondary_used_percent = None;
        current.secondary_window_minutes = None;

        assert_eq!(
            auto_warmup_reason_after_quota_refresh(Some(&previous), &current),
            Some("primary_quota_window_reset")
        );
    }

    #[test]
    fn ignores_non_reset_snapshot_updates() {
        let previous = snapshot(1_000, Some(1_200), Some(8_000));
        let current = snapshot(1_100, Some(1_200), Some(8_000));

        assert_eq!(
            auto_warmup_reason_after_quota_refresh(Some(&previous), &current),
            None
        );
    }

    #[test]
    fn tolerates_small_clock_skew_at_reset_boundary() {
        let previous = snapshot(1_000, Some(1_200), Some(8_000));
        let current = snapshot(1_110, Some(2_000), Some(8_000));

        assert_eq!(
            auto_warmup_reason_after_quota_refresh(Some(&previous), &current),
            Some("primary_quota_window_reset")
        );
    }

    #[test]
    fn does_not_treat_far_early_reset_advance_as_crossed() {
        let previous = snapshot(1_000, Some(2_000), Some(8_000));
        let current = snapshot(1_500, Some(3_000), Some(8_000));

        assert_eq!(
            auto_warmup_reason_after_quota_refresh(Some(&previous), &current),
            None
        );
    }
}
