//! 제목 기준으로 누락된 재생목록만 새로 만들어 채운다.
//!
//! API 계획은 대상 채널 하나에 묶여 있어, 다른 대상으로 같은 원본을 복제할 때 쓸 수 없다.
//! 이 경로는 원본 재생목록(계획 스냅샷)과 대상의 재생목록 제목만 비교해 동작한다.
//! 같은 제목이 이미 있으면 내용도 같다고 보고 건너뛴다(원본 계정에서 복제된 목록이므로).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::browser::cdp::CdpClient;
use crate::browser::jitter::Jitter;
use crate::browser::page;
use crate::browser::sync::{BrowserSyncOptions, failure, send};
use crate::browser::watch_later::WatchLaterItem;
use crate::error::AppError;
use crate::migrate::plan::display_titles;
use crate::youtube::Playlist;

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

/// 대상 계정의 재생목록(제목·ID)을 읽는다.
pub async fn read_target_playlists(
    cdp: &mut CdpClient,
    max_pages: usize,
) -> Result<Vec<WatchLaterItem>, AppError> {
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
        let expression = page::playlists_read_js(continuation.as_deref());
        let value = cdp
            .evaluate(&expression)
            .await
            .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
        let page: ReadPage = serde_json::from_value(value).map_err(|error| {
            AppError::Config(format!("재생목록 응답을 해석할 수 없습니다: {error}"))
        })?;
        if !page.ok {
            return Err(if matches!(page.status, 401 | 403) {
                AppError::Auth(
                    "재생목록을 읽으려면 브라우저에 로그인되어 있어야 합니다.".to_string(),
                )
            } else if page.error.is_empty() {
                AppError::Config(format!("재생목록 조회 실패(HTTP {}).", page.status))
            } else {
                AppError::Config(format!("재생목록 조회 실패: {}", page.error))
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

/// 실행 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaylistCopyReport {
    /// 새로 만든 재생목록 수.
    pub created: u64,
    /// 추가한 항목 수.
    pub items_added: u64,
    /// 제목이 이미 있어 건너뛴 재생목록 수.
    pub skipped_playlists: u64,
    /// (원본 제목, 새 재생목록 ID) 목록.
    pub created_playlists: Vec<(String, String)>,
}

impl PlaylistCopyReport {
    /// 사람이 읽는 요약.
    pub fn render(&self) -> String {
        let mut out = format!(
            "재생목록 복제: 생성 {} · 항목 추가 {} · 제목 중복 건너뜀 {}\n",
            self.created, self.items_added, self.skipped_playlists
        );
        for (title, id) in &self.created_playlists {
            out.push_str(&format!("  {title} → {id}\n"));
        }
        out
    }
}

/// 시청기록 재현 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryPlaylistReport {
    /// 새로 만든 재생목록 ID.
    pub created: Option<String>,
    /// 이미 있어 재사용한 재생목록 ID.
    pub reused: Option<String>,
    /// 추가한 항목 수.
    pub items_added: u64,
    /// 상한으로 잘라낸 항목 수.
    pub truncated: usize,
}

impl HistoryPlaylistReport {
    /// 사람이 읽는 요약.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if let Some(id) = &self.created {
            out.push_str(&format!("시청기록 플레이리스트를 만들었습니다: {id}\n"));
        }
        if let Some(id) = &self.reused {
            out.push_str(&format!("같은 이름의 재생목록을 재사용합니다: {id}\n"));
        }
        out.push_str(&format!("항목 추가: {}건", self.items_added));
        if self.truncated > 0 {
            out.push_str(&format!(" · 잘림 {}", self.truncated));
        }
        out.push('\n');
        out
    }
}

/// 시청기록을 비공개 재생목록으로 재현한다.
///
/// `videos`는 최신순이다. 재생목록은 나중에 추가한 항목이 위로 오므로, 최신 항목이
/// 먼저 보이도록 오래된 항목부터 추가한다. 같은 이름의 재생목록이 있으면 재사용한다.
pub async fn copy_history_playlist(
    cdp: &mut CdpClient,
    videos: &[(String, String)],
    name: &str,
    description: &str,
    opts: &BrowserSyncOptions,
) -> Result<HistoryPlaylistReport, AppError> {
    let target = read_target_playlists(cdp, 50).await?;
    if let Some(existing) = target.iter().find(|item| item.title == name) {
        return Ok(HistoryPlaylistReport {
            reused: Some(existing.video_id.clone()),
            ..HistoryPlaylistReport::default()
        });
    }

    let response = send(
        cdp,
        page::CREATE_PLAYLIST_ENDPOINT,
        &page::create_playlist_body(name, description, "private"),
        opts,
    )
    .await?;
    if !response.ok {
        return Err(failure(&format!("history:{name}"), &response));
    }
    let playlist_id = response
        .json()
        .and_then(|value| {
            value["playlistId"]
                .as_str()
                .map(str::to_string)
                .or_else(|| value["playlistId"].as_u64().map(|id| id.to_string()))
        })
        .ok_or_else(|| {
            AppError::Config(format!(
                "시청기록 재생목록 생성 응답에 playlistId가 없습니다: {}",
                response.body
            ))
        })?;

    let mut report = HistoryPlaylistReport {
        created: Some(playlist_id.clone()),
        ..HistoryPlaylistReport::default()
    };
    let mut pacing = Jitter::from_clock();
    let mut added: HashSet<&str> = HashSet::new();
    for (video_id, _) in videos.iter().rev() {
        if !added.insert(video_id.as_str()) {
            continue;
        }
        let delay = opts.delay + pacing.duration(opts.jitter);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let response = send(
            cdp,
            page::EDIT_PLAYLIST_ENDPOINT,
            &page::add_item_body(&playlist_id, video_id),
            opts,
        )
        .await?;
        if response.ok && !response.body.contains("STATUS_FAILED") {
            report.items_added += 1;
        } else if response.body.contains("already") {
            continue;
        } else {
            return Err(failure(&format!("history:{video_id}"), &response));
        }
    }
    Ok(report)
}

/// 원본 재생목록 중 대상에 없는 제목만 생성하고 항목을 채운다.
pub async fn copy_missing_playlists(
    cdp: &mut CdpClient,
    source: &[Playlist],
    opts: &BrowserSyncOptions,
    now: DateTime<Utc>,
) -> Result<PlaylistCopyReport, AppError> {
    let _ = now;
    let target = read_target_playlists(cdp, 50).await?;
    let existing: HashSet<String> = target.into_iter().map(|item| item.title).collect();

    let mut report = PlaylistCopyReport::default();
    let mut pacing = Jitter::from_clock();
    let titles = display_titles(source);

    for (playlist, (_, display_title)) in source.iter().zip(titles) {
        if existing.contains(&display_title) {
            report.skipped_playlists += 1;
            continue;
        }

        let response = send(
            cdp,
            page::CREATE_PLAYLIST_ENDPOINT,
            &page::create_playlist_body(
                &display_title,
                &playlist.description,
                playlist.privacy.api_status(),
            ),
            opts,
        )
        .await?;
        if !response.ok {
            return Err(failure(&format!("playlist:{display_title}"), &response));
        }
        let playlist_id = response
            .json()
            .and_then(|value| {
                value["playlistId"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| value["playlistId"].as_u64().map(|id| id.to_string()))
            })
            .ok_or_else(|| {
                AppError::Config(format!(
                    "재생목록 '{display_title}' 생성 응답에 playlistId가 없습니다: {}",
                    response.body
                ))
            })?;
        report.created += 1;
        report
            .created_playlists
            .push((display_title.clone(), playlist_id.clone()));

        let mut added: HashSet<&str> = HashSet::new();
        for item in &playlist.items {
            if !added.insert(item.video_id.as_str()) {
                continue;
            }
            let delay = opts.delay + pacing.duration(opts.jitter);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let response = send(
                cdp,
                page::EDIT_PLAYLIST_ENDPOINT,
                &page::add_item_body(&playlist_id, &item.video_id),
                opts,
            )
            .await?;
            if response.ok && !response.body.contains("STATUS_FAILED") {
                report.items_added += 1;
            } else if response.body.contains("already") {
                continue;
            } else {
                return Err(failure(
                    &format!("playlist-item:{display_title}:{}", item.video_id),
                    &response,
                ));
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_renders_korean_counts() {
        let report = PlaylistCopyReport {
            created: 2,
            items_added: 10,
            skipped_playlists: 3,
            created_playlists: vec![("여행".to_string(), "PL_1".to_string())],
        };
        let text = report.render();
        assert!(text.contains("생성 2"));
        assert!(text.contains("항목 추가 10"));
        assert!(text.contains("건너뜀 3"));
        assert!(text.contains("여행 → PL_1"));
    }
}
