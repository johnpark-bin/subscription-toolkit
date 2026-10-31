//! 브라우저 기준으로 두 계정의 집합을 비교해 빠진 항목만 반영한다.
//!
//! YouTube API의 대상 상태가 실제와 어긋날 때(예: 브라우저로 만든 좋아요/구독이
//! API 목록에 늦게 반영됨) 계획 기반 diff 대신 이 경로를 쓴다.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::browser::cdp::CdpClient;
use crate::browser::jitter::Jitter;
use crate::browser::page;
use crate::browser::sync::{BrowserSyncOptions, failure, send};
use crate::browser::watch_later::WatchLaterItem;
use crate::error::AppError;
use crate::migrate::journal::{Journal, JournalStatus};
use crate::migrate::quota::QuotaLedger;
use crate::youtube::{ApiError, YouTubeApi};

#[derive(Debug, Deserialize)]
struct ReadPage {
    ok: bool,
    status: u16,
    #[serde(default)]
    items: Vec<WatchLaterItem>,
    #[serde(default)]
    continuation: Option<String>,
    #[serde(default)]
    error: String,
}

async fn read_ids(
    cdp: &mut CdpClient,
    script: fn(Option<&str>) -> String,
    max_pages: usize,
    label: &str,
) -> Result<Vec<WatchLaterItem>, AppError> {
    // 페이지가 Studio 등 다른 오리진이면 요청이 교차 출처로 나가 400이 된다.
    cdp.navigate(page::YOUTUBE_URL)
        .await
        .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
    cdp.wait_for_ytcfg()
        .await
        .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;

    let mut items: Vec<WatchLaterItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut continuation: Option<String> = None;

    for _ in 0..max_pages.max(1) {
        let expression = script(continuation.as_deref());
        let value = cdp
            .evaluate(&expression)
            .await
            .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
        let page: ReadPage = serde_json::from_value(value).map_err(|error| {
            AppError::Config(format!("{label} 응답을 해석할 수 없습니다: {error}"))
        })?;
        if !page.ok {
            return Err(if matches!(page.status, 401 | 403) {
                AppError::Auth(format!(
                    "{label}을 읽으려면 브라우저에 로그인되어 있어야 합니다. Chrome에서 로그인한 뒤 다시 실행하세요."
                ))
            } else if page.error.is_empty() {
                AppError::Config(format!("{label} 조회 실패(HTTP {}).", page.status))
            } else {
                AppError::Config(format!("{label} 조회 실패: {}", page.error))
            });
        }
        for item in page.items {
            if seen.insert(item.video_id.clone()) {
                items.push(item);
            }
        }
        match page.continuation.filter(|token| !token.is_empty()) {
            Some(token) => continuation = Some(token),
            None => return Ok(items),
        }
    }
    Ok(items)
}

/// 좋아요 목록(영상 ID)을 읽는다.
pub async fn read_liked_videos(
    cdp: &mut CdpClient,
    max_pages: usize,
) -> Result<Vec<WatchLaterItem>, AppError> {
    read_ids(cdp, page::liked_videos_read_js, max_pages, "좋아요 목록").await
}

/// 구독 목록(채널 ID)을 읽는다.
pub async fn read_subscriptions(
    cdp: &mut CdpClient,
    max_pages: usize,
) -> Result<Vec<WatchLaterItem>, AppError> {
    read_ids(cdp, page::subscriptions_read_js, max_pages, "구독 목록").await
}

/// 원본에만 있는 ID를 원본 순서대로 돌려준다.
pub fn diff_missing(
    source: &[String],
    target: &HashSet<String>,
    limit: Option<usize>,
) -> Vec<String> {
    let mut missing: Vec<String> = source
        .iter()
        .filter(|id| !target.contains(*id))
        .cloned()
        .collect();
    if let Some(limit) = limit {
        missing.truncate(limit);
    }
    missing
}

/// 조정 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// 추가한 좋아요 수.
    pub likes_added: u64,
    /// 추가한 구독 수.
    pub subscriptions_added: u64,
    /// 이미 대상에 있어 건너뛴 수.
    pub skipped: u64,
    /// journal 기준 이미 완료된 수.
    pub already_done: u64,
}

impl ReconcileReport {
    /// 사람이 읽는 요약.
    pub fn render(&self) -> String {
        format!(
            "브라우저 조정: 좋아요 추가 {} · 구독 추가 {} · 건너뜀 {} · 이미 완료 {}\n",
            self.likes_added, self.subscriptions_added, self.skipped, self.already_done
        )
    }
}

struct Apply {
    endpoint: &'static str,
    body: fn(&str) -> Value,
    prefix: &'static str,
}

const LIKE_APPLY: Apply = Apply {
    endpoint: page::LIKE_ENDPOINT,
    body: page::like_body,
    prefix: "like:",
};

const SUBSCRIBE_APPLY: Apply = Apply {
    endpoint: page::SUBSCRIBE_ENDPOINT,
    body: page::subscribe_body,
    prefix: "subscription:",
};

#[allow(clippy::too_many_arguments)]
async fn apply_missing(
    cdp: &mut CdpClient,
    ids: &[String],
    apply: &Apply,
    journal: &mut Journal,
    opts: &BrowserSyncOptions,
    now: DateTime<Utc>,
    already_done: &mut u64,
) -> Result<u64, AppError> {
    let mut added = 0u64;
    let mut pacing = Jitter::from_clock();
    let mut first = true;
    for id in ids {
        // 대상에서 실제로 빠져 있으므로 journal 기록과 무관하게 다시 시도한다.
        // (이전 실행이 성공으로 남았어도 플랫폼이 반영하지 않았을 수 있다.)
        let key = format!("{}{id}", apply.prefix);
        if journal.is_complete(&key) {
            *already_done += 1;
        }
        if !first {
            let delay = opts.delay + pacing.duration(opts.jitter);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        first = false;

        let response = send(cdp, apply.endpoint, &(apply.body)(id), opts).await?;
        if response.ok && !response.body.contains("STATUS_FAILED") {
            journal.record(&key, JournalStatus::Success, None, now)?;
            added += 1;
        } else if response.body.contains("already") {
            journal.record(
                &key,
                JournalStatus::Skipped,
                Some(Value::String("alreadyPresent".to_string())),
                now,
            )?;
        } else {
            return Err(failure(&key, &response));
        }
    }
    Ok(added)
}

/// 좋아요와 구독을 원본 집합에 맞춘다. 대상에만 있는 항목은 건드리지 않는다.
pub async fn reconcile(
    cdp: &mut CdpClient,
    source_likes: Option<&[String]>,
    source_subscriptions: Option<&[String]>,
    journal: &mut Journal,
    opts: &BrowserSyncOptions,
    now: DateTime<Utc>,
) -> Result<ReconcileReport, AppError> {
    let mut report = ReconcileReport::default();

    if let Some(source) = source_likes {
        let target = read_liked_videos(cdp, 50).await?;
        let target_ids: HashSet<String> = target.iter().map(|item| item.video_id.clone()).collect();
        let missing = diff_missing(source, &target_ids, opts.limit);
        report.skipped += (source.len().saturating_sub(missing.len())) as u64;
        let added = apply_missing(
            cdp,
            &missing,
            &LIKE_APPLY,
            journal,
            opts,
            now,
            &mut report.already_done,
        )
        .await?;
        report.likes_added = added;
    }

    if let Some(source) = source_subscriptions {
        let target = read_subscriptions(cdp, 50).await?;
        let target_ids: HashSet<String> = target.iter().map(|item| item.video_id.clone()).collect();
        let missing = diff_missing(source, &target_ids, opts.limit);
        report.skipped += (source.len().saturating_sub(missing.len())) as u64;
        let added = apply_missing(
            cdp,
            &missing,
            &SUBSCRIBE_APPLY,
            journal,
            opts,
            now,
            &mut report.already_done,
        )
        .await?;
        report.subscriptions_added = added;
    }

    Ok(report)
}

/// API로 좋아요·구독 부족분을 채운다.
///
/// 브라우저 세션이 응답만 성공으로 돌려주고 목록에 게시하지 않는 경우(플랫폼 스로틀)의
/// 대체 경로다. 대상 집합은 호출자가 브라우저에서 읽어 전달한다.
#[allow(clippy::too_many_arguments)]
pub async fn reconcile_via_api(
    api: &dyn YouTubeApi,
    quota: &mut QuotaLedger,
    source_likes: Option<&[String]>,
    source_subscriptions: Option<&[String]>,
    target_likes: &HashSet<String>,
    target_subscriptions: &HashSet<String>,
    journal: &mut Journal,
    limit: Option<usize>,
    write_cost: u64,
    now: DateTime<Utc>,
) -> Result<ReconcileReport, AppError> {
    let mut report = ReconcileReport::default();

    async fn apply(
        api: &dyn YouTubeApi,
        quota: &mut QuotaLedger,
        ids: &[String],
        is_like: bool,
        prefix: &str,
        journal: &mut Journal,
        write_cost: u64,
        now: DateTime<Utc>,
        report: &mut ReconcileReport,
    ) -> Result<(), AppError> {
        for id in ids {
            quota.reserve(write_cost)?;
            let result = if is_like {
                api.like(id).await
            } else {
                api.subscribe(id).await
            };
            let key = format!("{prefix}{id}");
            match result {
                Ok(()) => {
                    journal.record(&key, JournalStatus::Success, None, now)?;
                    if is_like {
                        report.likes_added += 1;
                    } else {
                        report.subscriptions_added += 1;
                    }
                }
                Err(ApiError::AlreadyExists(_)) => {
                    journal.record(
                        &key,
                        JournalStatus::Skipped,
                        Some(Value::String("alreadyPresent".to_string())),
                        now,
                    )?;
                    report.skipped += 1;
                }
                Err(ApiError::NotFound(_)) | Err(ApiError::Forbidden(_)) => {
                    journal.record(
                        &key,
                        JournalStatus::Skipped,
                        Some(Value::String("unavailable".to_string())),
                        now,
                    )?;
                    report.skipped += 1;
                }
                Err(error) => return Err(AppError::Api(error)),
            }
        }
        Ok(())
    }

    if let Some(source) = source_likes {
        let missing = diff_missing(source, target_likes, limit);
        report.skipped += (source.len().saturating_sub(missing.len())) as u64;
        apply(
            api,
            quota,
            &missing,
            true,
            "like:",
            journal,
            write_cost,
            now,
            &mut report,
        )
        .await?;
    }

    if let Some(source) = source_subscriptions {
        let missing = diff_missing(source, target_subscriptions, limit);
        report.skipped += (source.len().saturating_sub(missing.len())) as u64;
        apply(
            api,
            quota,
            &missing,
            false,
            "subscription:",
            journal,
            write_cost,
            now,
            &mut report,
        )
        .await?;
    }

    Ok(report)
}

/// 기본 실행 옵션.
pub fn default_options() -> BrowserSyncOptions {
    BrowserSyncOptions {
        delay: Duration::from_millis(3_000),
        jitter: Duration::from_millis(1_500),
        ..BrowserSyncOptions::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_missing_preserves_source_order_and_limit() {
        let source: Vec<String> = ["A", "B", "C", "D"].iter().map(|s| s.to_string()).collect();
        let target: HashSet<String> = ["B".to_string()].into_iter().collect();
        assert_eq!(diff_missing(&source, &target, None), vec!["A", "C", "D"]);
        assert_eq!(diff_missing(&source, &target, Some(2)), vec!["A", "C"]);
    }

    #[test]
    fn diff_missing_is_empty_when_identical() {
        let source: Vec<String> = ["A", "B"].iter().map(|s| s.to_string()).collect();
        let target: HashSet<String> = ["A".to_string(), "B".to_string()].into_iter().collect();
        assert!(diff_missing(&source, &target, None).is_empty());
    }
}
