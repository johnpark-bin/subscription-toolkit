//! Watch Later(나중에 볼 동영상) 동기화.
//!
//! YouTube Data API는 Watch Later(`WL`)에 접근하지 못하고 Takeout에서도 빠진다.
//! 브라우저 세션에서는 `browse(VLWL)`로 읽고 `browse/edit_playlist`로 추가·삭제할 수 있다.

use std::collections::HashSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::browser::cdp::CdpClient;
use crate::browser::jitter::Jitter;
use crate::browser::page;
use crate::browser::sync::{BrowserSyncOptions, failure, send};
use crate::error::AppError;
use crate::migrate::journal::{Journal, JournalStatus};

/// Watch Later 항목.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchLaterItem {
    /// 영상 ID.
    pub video_id: String,
    /// 영상 제목(있으면).
    #[serde(default)]
    pub title: String,
}

/// 내보낸 Watch Later 파일.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchLaterFile {
    /// 내보낸 시각.
    pub exported_at: DateTime<Utc>,
    /// 내보낸 계정의 채널 ID.
    pub channel_id: String,
    /// 최신순 항목.
    pub items: Vec<WatchLaterItem>,
}

/// journal 키 접두사.
pub const JOURNAL_PREFIX: &str = "watch-later:";
/// 삭제(prune) journal 키 접두사.
pub const JOURNAL_PRUNE_PREFIX: &str = "watch-later-remove:";

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

/// Watch Later 목록을 읽는다. continuation 페이지를 따라가며 중복을 제거한다.
pub async fn read_watch_later(
    cdp: &mut CdpClient,
    max_pages: usize,
) -> Result<Vec<WatchLaterItem>, AppError> {
    // 요청은 www.youtube.com 오리진에서 보내야 한다(Studio 탭이면 400).
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
        let expression = page::watch_later_read_js(continuation.as_deref());
        let value = cdp
            .evaluate(&expression)
            .await
            .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
        let page: ReadPage = serde_json::from_value(value).map_err(|error| {
            AppError::Config(format!("Watch Later 응답을 해석할 수 없습니다: {error}"))
        })?;

        if !page.ok {
            return Err(if matches!(page.status, 401 | 403) {
                AppError::Auth(
                    "Watch Later를 읽으려면 브라우저에 로그인되어 있어야 합니다. Chrome에서 로그인한 뒤 다시 실행하세요."
                        .to_string(),
                )
            } else if page.error.is_empty() {
                AppError::Config(format!(
                    "Watch Later 조회에 실패했습니다(HTTP {}).",
                    page.status
                ))
            } else {
                AppError::Config(format!("Watch Later 조회 실패: {}", page.error))
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

/// 원본에서 대상에 없는 항목을 계산한다.
///
/// Watch Later는 추가 시각 역순으로 보이므로, 원본 순서(최신순)를 유지하려면 오래된 항목부터
/// 추가해야 한다. 따라서 결과는 원본의 역순이며 `limit`은 추가할 개수 상한이다.
pub fn plan_watch_later(
    source: &[WatchLaterItem],
    target_ids: &HashSet<String>,
    limit: Option<usize>,
) -> Vec<WatchLaterItem> {
    let mut missing: Vec<WatchLaterItem> = source
        .iter()
        .filter(|item| !target_ids.contains(&item.video_id))
        .cloned()
        .collect();
    missing.reverse();
    if let Some(limit) = limit {
        missing.truncate(limit);
    }
    missing
}

/// 실행 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchLaterReport {
    /// 추가 성공 수.
    pub added: u64,
    /// 삭제(prune) 수.
    pub removed: u64,
    /// 이미 대상에 있어 건너뛴 수.
    pub skipped: u64,
    /// journal 기준 이미 완료된 수.
    pub already_done: u64,
}

impl WatchLaterReport {
    /// 사람이 읽는 요약.
    pub fn render(&self) -> String {
        format!(
            "Watch Later: 추가 {} · 건너뜀 {} · 삭제 {} · 이미 완료 {}\n",
            self.added, self.skipped, self.removed, self.already_done
        )
    }
}

fn edit_key(video_id: &str) -> String {
    format!("{JOURNAL_PREFIX}{video_id}")
}

fn prune_key(video_id: &str) -> String {
    format!("{JOURNAL_PRUNE_PREFIX}{video_id}")
}

/// Watch Later를 원본 목록에 맞춘다.
///
/// `prune`이 참이면 대상에만 있는 항목을 삭제한다(기본 거짓).
pub async fn sync_watch_later(
    cdp: &mut CdpClient,
    source: &[WatchLaterItem],
    journal: &mut Journal,
    opts: &BrowserSyncOptions,
    prune: bool,
    now: DateTime<Utc>,
) -> Result<WatchLaterReport, AppError> {
    if prune && source.is_empty() {
        return Err(AppError::Usage(
            "원본 Watch Later 목록이 비어 있어 삭제를 중단했습니다. `stk browser watch-later export`로 원본을 먼저 내보내세요."
                .to_string(),
        ));
    }

    let target = read_watch_later(cdp, 50).await?;
    let target_ids: HashSet<String> = target.iter().map(|item| item.video_id.clone()).collect();
    let source_ids: HashSet<String> = source.iter().map(|item| item.video_id.clone()).collect();
    let to_add = plan_watch_later(source, &target_ids, opts.limit);

    let mut report = WatchLaterReport {
        skipped: (source.len().saturating_sub(to_add.len())) as u64,
        ..WatchLaterReport::default()
    };
    let mut pacing = Jitter::from_clock();
    let mut first = true;

    for item in &to_add {
        let key = edit_key(&item.video_id);
        if journal.is_complete(&key) {
            report.already_done += 1;
            continue;
        }
        if !first {
            let delay = opts.delay + pacing.duration(opts.jitter);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        first = false;

        let response = send(
            cdp,
            page::EDIT_PLAYLIST_ENDPOINT,
            &page::watch_later_edit_body(&item.video_id, true),
            opts,
        )
        .await?;
        if response.ok && !response.body.contains("STATUS_FAILED") {
            journal.record(&key, JournalStatus::Success, None, now)?;
            report.added += 1;
        } else if response.body.contains("already") {
            journal.record(
                &key,
                JournalStatus::Skipped,
                Some(Value::String("alreadyInWatchLater".to_string())),
                now,
            )?;
            report.skipped += 1;
        } else {
            return Err(failure(&key, &response));
        }
    }

    if prune {
        for item in target
            .iter()
            .filter(|item| !source_ids.contains(&item.video_id))
        {
            let key = prune_key(&item.video_id);
            if journal.is_complete(&key) {
                continue;
            }
            if !opts.delay.is_zero() {
                tokio::time::sleep(opts.delay + pacing.duration(opts.jitter)).await;
            }
            let response = send(
                cdp,
                page::EDIT_PLAYLIST_ENDPOINT,
                &page::watch_later_edit_body(&item.video_id, false),
                opts,
            )
            .await?;
            if response.ok && !response.body.contains("STATUS_FAILED") {
                journal.record(&key, JournalStatus::Success, None, now)?;
                report.removed += 1;
            } else {
                return Err(failure(&key, &response));
            }
        }
    }

    Ok(report)
}

/// 대기 시간 기본값과 함께 쓰는 옵션.
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

    fn item(id: &str) -> WatchLaterItem {
        WatchLaterItem {
            video_id: id.to_string(),
            title: format!("영상 {id}"),
        }
    }

    #[test]
    fn plan_returns_missing_items_in_reverse_source_order() {
        let source = vec![item("A"), item("B"), item("C")];
        let target: HashSet<String> = ["B".to_string()].into_iter().collect();
        let planned = plan_watch_later(&source, &target, None);
        assert_eq!(
            planned
                .iter()
                .map(|item| item.video_id.as_str())
                .collect::<Vec<_>>(),
            vec!["C", "A"],
            "오래된 항목부터 추가해 최신순 표시를 맞춘다"
        );
    }

    #[test]
    fn plan_respects_limit() {
        let source = vec![item("A"), item("B"), item("C"), item("D")];
        let planned = plan_watch_later(&source, &HashSet::new(), Some(2));
        assert_eq!(
            planned
                .iter()
                .map(|item| item.video_id.as_str())
                .collect::<Vec<_>>(),
            vec!["D", "C"]
        );
    }

    #[test]
    fn plan_is_empty_when_all_present() {
        let source = vec![item("A")];
        let target: HashSet<String> = ["A".to_string()].into_iter().collect();
        assert!(plan_watch_later(&source, &target, None).is_empty());
    }

    #[test]
    fn journal_keys_are_distinct() {
        assert_eq!(edit_key("A"), "watch-later:A");
        assert_eq!(prune_key("A"), "watch-later-remove:A");
    }
}
