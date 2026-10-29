//! 시청기록을 비공개 플레이리스트로 재현하는 계획.

use std::collections::{HashMap, HashSet};

use crate::error::AppError;
use crate::history::parse::HistoryFile;
use crate::migrate::model::{Action, ActionKind, TargetState};
use crate::migrate::{PLAYLIST_ITEM_LIMIT, is_system_summary};
use crate::youtube::Privacy;

/// 시청기록 플레이리스트의 액션 키/매핑에 쓰는 가상 소스 ID.
pub const HISTORY_PLAYLIST_ID: &str = "history";

/// 기본 플레이리스트 이름.
pub const DEFAULT_PLAYLIST_NAME: &str = "시청기록 (이전)";

/// 플레이리스트 설명.
pub const HISTORY_DESCRIPTION: &str = "Google Takeout 시청기록에서 생성";

/// 시청기록 플레이리스트 계획.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPlaylistPlan {
    /// 플레이리스트 이름.
    pub playlist_title: String,
    /// 재사용할 기존 플레이리스트 ID.
    pub existing_playlist_id: Option<String>,
    /// 계획한 영상 순서(최신순).
    pub video_ids: Vec<String>,
    /// 실행할 액션.
    pub actions: Vec<Action>,
    /// 이미 대상에 있어 건너뛴 항목 수.
    pub skipped_existing: usize,
    /// 상한(5,000/`--limit`)으로 잘라낸 항목 수.
    pub truncated: usize,
}

/// 시청기록에서 재현할 영상 목록을 만든다.
///
/// 광고와 영상 ID가 없는 항목을 제외하고, videoId 기준으로 가장 최근 시청만 남긴 뒤
/// 최신순으로 정렬한다.
pub fn history_video_ids(
    file: &HistoryFile,
    limit: Option<usize>,
) -> (Vec<(String, String)>, usize) {
    let mut newest: HashMap<&str, (Option<chrono::DateTime<chrono::Utc>>, &str)> = HashMap::new();
    for entry in &file.entries {
        if entry.is_ad {
            continue;
        }
        let Some(video_id) = entry.video_id.as_deref() else {
            continue;
        };
        let slot = newest
            .entry(video_id)
            .or_insert((None, entry.title.as_str()));
        if entry.watched_at > slot.0 {
            *slot = (entry.watched_at, entry.title.as_str());
        }
    }

    let mut videos: Vec<(String, String, Option<chrono::DateTime<chrono::Utc>>)> = newest
        .into_iter()
        .map(|(video_id, (watched_at, title))| {
            (video_id.to_string(), title.to_string(), watched_at)
        })
        .collect();
    videos.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));

    let cap = PLAYLIST_ITEM_LIMIT.min(limit.unwrap_or(PLAYLIST_ITEM_LIMIT));
    let truncated = videos.len().saturating_sub(cap);
    videos.truncate(cap);
    (
        videos
            .into_iter()
            .map(|(video_id, title, _)| (video_id, title))
            .collect(),
        truncated,
    )
}

/// 시청기록 플레이리스트 액션을 만든다.
///
/// 같은 이름의 대상 플레이리스트가 있으면 재사용하고 기존 영상은 건너뛴다.
pub fn plan_history_playlist(
    file: &HistoryFile,
    target: &TargetState,
    name: &str,
    limit: Option<usize>,
) -> Result<HistoryPlaylistPlan, AppError> {
    let playlist_title = if name.trim().is_empty() {
        DEFAULT_PLAYLIST_NAME.to_string()
    } else {
        name.trim().to_string()
    };

    let (videos, truncated) = history_video_ids(file, limit);
    if videos.is_empty() {
        return Err(AppError::Config(
            "재현할 시청기록이 없습니다. 광고와 삭제된 영상만 있거나 import 결과가 비어 있습니다."
                .to_string(),
        ));
    }

    let existing = target
        .playlist_summaries
        .iter()
        .find(|summary| summary.title == playlist_title && !is_system_summary(summary));
    let present: HashSet<&str> = existing
        .and_then(|summary| target.playlist_items.get(&summary.id))
        .map(|items| items.iter().map(|item| item.video_id.as_str()).collect())
        .unwrap_or_default();

    let mut actions = Vec::new();
    if existing.is_none() {
        actions.push(Action {
            key: format!("playlist:{HISTORY_PLAYLIST_ID}"),
            kind: ActionKind::CreatePlaylist {
                source_playlist_id: HISTORY_PLAYLIST_ID.to_string(),
                title: playlist_title.clone(),
                description: HISTORY_DESCRIPTION.to_string(),
                privacy: Privacy::Private,
            },
        });
    }

    let mut skipped_existing = 0usize;
    for (video_id, title) in &videos {
        if present.contains(video_id.as_str()) {
            skipped_existing += 1;
            continue;
        }
        actions.push(Action {
            key: format!("playlist-item:{HISTORY_PLAYLIST_ID}:{video_id}"),
            kind: ActionKind::AddPlaylistItem {
                source_playlist_id: HISTORY_PLAYLIST_ID.to_string(),
                video_id: video_id.clone(),
                title: title.clone(),
            },
        });
    }

    Ok(HistoryPlaylistPlan {
        playlist_title,
        existing_playlist_id: existing.map(|summary| summary.id.clone()),
        video_ids: videos.into_iter().map(|(video_id, _)| video_id).collect(),
        actions,
        skipped_existing,
        truncated,
    })
}
