//! 스냅샷 수집과 계획 계산(순수 로직).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::account::Account;
use crate::error::AppError;
use crate::migrate::model::{Action, ActionKind, DataType, Plan, Snapshot, TargetState};
use crate::youtube::{
    ApiError, LikedVideo, Playlist, PlaylistItemRef, PlaylistSummary, Subscription, YouTubeApi,
};

/// 스냅샷을 그대로 재사용하는 기간(시간).
pub const SNAPSHOT_TTL_HOURS: i64 = 24;

/// API 데이터 최대 보관 기간(일).
pub const DATA_RETENTION_DAYS: i64 = 30;

/// 플레이리스트 하나에 넣을 수 있는 항목 상한.
pub const PLAYLIST_ITEM_LIMIT: usize = 5_000;

/// 쓰기 요청 하나의 쿼터 비용.
pub const WRITE_QUOTA_UNIT: u64 = 50;

/// YouTube API가 돌려주는 좋아요 상한(최근 1,000개).
pub const LIKE_LIMIT: usize = 1_000;

/// 시스템 플레이리스트 ID 접두사.
const SYSTEM_PLAYLIST_PREFIXES: [&str; 4] = ["LL", "WL", "HL", "UU"];

/// 시스템 플레이리스트 제목(언어별).
const SYSTEM_PLAYLIST_TITLES: [&str; 4] = [
    "Liked videos",
    "좋아요 표시한 동영상",
    "Watch later",
    "나중에 볼 동영상",
];

/// ID가 시스템 플레이리스트 접두사로 시작하는지 확인한다.
pub fn is_system_playlist_id(id: &str) -> bool {
    SYSTEM_PLAYLIST_PREFIXES
        .iter()
        .any(|prefix| id.starts_with(prefix))
}

/// 시스템 플레이리스트 제목인지 확인한다.
pub fn is_system_playlist_title(title: &str) -> bool {
    SYSTEM_PLAYLIST_TITLES.contains(&title)
}

/// 원본 플레이리스트가 시스템 플레이리스트인지 확인한다.
pub fn is_system_playlist(playlist: &Playlist) -> bool {
    is_system_playlist_id(&playlist.id) || is_system_playlist_title(&playlist.title)
}

/// 대상 플레이리스트 요약이 시스템 플레이리스트인지 확인한다.
pub fn is_system_summary(summary: &PlaylistSummary) -> bool {
    is_system_playlist_id(&summary.id) || is_system_playlist_title(&summary.title)
}

/// 원본 플레이리스트의 표시 제목을 만든다.
///
/// 같은 제목이 여러 개면 두 번째부터 `" (2)"`, `" (3)"` 접미사를 붙인다.
pub fn display_titles(playlists: &[Playlist]) -> Vec<(String, String)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    playlists
        .iter()
        .map(|playlist| {
            let count = counts.entry(playlist.title.as_str()).or_insert(0);
            *count += 1;
            let title = if *count == 1 {
                playlist.title.clone()
            } else {
                format!("{} ({count})", playlist.title)
            };
            (playlist.id.clone(), title)
        })
        .collect()
}

/// 원본 계정의 전체 상태를 읽는다.
pub async fn fetch_snapshot(
    api: &dyn YouTubeApi,
    account: Account,
    now: DateTime<Utc>,
) -> Result<Snapshot, ApiError> {
    let channel = api.channel().await?;
    let subscriptions = api.list_subscriptions().await?;
    let summaries = api.list_playlists().await?;
    let mut playlists = Vec::new();
    for summary in summaries
        .into_iter()
        .filter(|item| !is_system_summary(item))
    {
        let items = api.list_playlist_items(&summary.id).await?;
        playlists.push(Playlist {
            id: summary.id,
            title: summary.title,
            description: summary.description,
            privacy: summary.privacy,
            items,
        });
    }
    let mut likes = api.list_liked_videos().await?;
    // YouTube API는 최근 1,000개까지만 돌려준다. 방어적으로 여기서도 자른다.
    likes.truncate(LIKE_LIMIT);
    Ok(Snapshot {
        account,
        channel,
        fetched_at: now,
        subscriptions,
        playlists,
        likes,
    })
}

/// 대상 계정의 현재 상태를 읽는다.
///
/// 재사용할 수 있는 플레이리스트(제목 일치)의 항목만 읽어 쿼터를 아낀다.
pub async fn fetch_target_state(
    api: &dyn YouTubeApi,
    source_playlists: &[Playlist],
) -> Result<TargetState, ApiError> {
    let channel = api.channel().await?;
    let subscription_ids = api
        .list_subscriptions()
        .await?
        .into_iter()
        .map(|item| item.channel_id)
        .collect();
    let playlist_summaries: Vec<PlaylistSummary> = api
        .list_playlists()
        .await?
        .into_iter()
        .filter(|item| !is_system_summary(item))
        .collect();

    let wanted: HashSet<String> = display_titles(source_playlists)
        .into_iter()
        .map(|(_, title)| title)
        .collect();
    let mut playlist_items = HashMap::new();
    for summary in &playlist_summaries {
        if wanted.contains(&summary.title) {
            let items = api.list_playlist_items(&summary.id).await?;
            playlist_items.insert(summary.id.clone(), items);
        }
    }

    let liked_ids = api
        .list_liked_videos()
        .await?
        .into_iter()
        .map(|item| item.video_id)
        .collect();

    Ok(TargetState {
        channel,
        subscription_ids,
        playlist_summaries,
        playlist_items,
        liked_ids,
    })
}

fn add_item_action(source_playlist_id: &str, item: &PlaylistItemRef) -> Action {
    Action {
        key: format!("playlist-item:{source_playlist_id}:{}", item.video_id),
        kind: ActionKind::AddPlaylistItem {
            source_playlist_id: source_playlist_id.to_string(),
            video_id: item.video_id.clone(),
            title: item.title.clone(),
        },
    }
}

/// 원본과 대상의 차이를 계산해 계획을 만든다. 쓰기를 하지 않는 순수 함수다.
pub fn build_plan(
    source: &Snapshot,
    target: &TargetState,
    types: &[DataType],
    now: DateTime<Utc>,
) -> Result<Plan, AppError> {
    if source.channel.id == target.channel.id {
        return Err(AppError::Auth(
            "원본과 대상이 같은 채널입니다. 다른 계정으로 로그인하세요.".to_string(),
        ));
    }

    let wants = |data_type: DataType| types.contains(&data_type);
    let mut actions = Vec::new();

    if wants(DataType::Subscriptions) {
        let mut seen: HashSet<&str> = HashSet::new();
        for subscription in &source.subscriptions {
            if target.subscription_ids.contains(&subscription.channel_id)
                || !seen.insert(subscription.channel_id.as_str())
            {
                continue;
            }
            actions.push(Action {
                key: format!("subscription:{}", subscription.channel_id),
                kind: ActionKind::Subscribe {
                    channel_id: subscription.channel_id.clone(),
                    title: subscription.title.clone(),
                },
            });
        }
    }

    let mut playlist_targets = HashMap::new();
    if wants(DataType::Playlists) {
        let titles = display_titles(&source.playlists);
        for (playlist, (_, display_title)) in source.playlists.iter().zip(titles) {
            let existing = target
                .playlist_summaries
                .iter()
                .find(|summary| summary.title == display_title && !is_system_summary(summary));
            let present: HashSet<&str> = existing
                .and_then(|summary| target.playlist_items.get(&summary.id))
                .map(|items| items.iter().map(|item| item.video_id.as_str()).collect())
                .unwrap_or_default();

            if let Some(existing) = existing {
                playlist_targets.insert(playlist.id.clone(), existing.id.clone());
            } else {
                actions.push(Action {
                    key: format!("playlist:{}", playlist.id),
                    kind: ActionKind::CreatePlaylist {
                        source_playlist_id: playlist.id.clone(),
                        title: display_title,
                        description: playlist.description.clone(),
                        privacy: playlist.privacy,
                    },
                });
            }

            let mut seen: HashSet<&str> = HashSet::new();
            for item in playlist.items.iter().take(PLAYLIST_ITEM_LIMIT) {
                if present.contains(item.video_id.as_str()) || !seen.insert(item.video_id.as_str())
                {
                    continue;
                }
                actions.push(add_item_action(&playlist.id, item));
            }
        }
    }

    if wants(DataType::Likes) {
        let mut seen: HashSet<&str> = HashSet::new();
        for like in &source.likes {
            if target.liked_ids.contains(&like.video_id) || !seen.insert(like.video_id.as_str()) {
                continue;
            }
            actions.push(Action {
                key: format!("like:{}", like.video_id),
                kind: ActionKind::Like {
                    video_id: like.video_id.clone(),
                    title: like.title.clone(),
                },
            });
        }
    }

    Ok(Plan {
        generated_at: now,
        source: source.clone(),
        target: target.clone(),
        playlist_targets,
        actions,
    })
}

/// 쿼터 견적.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QuotaEstimate {
    /// 쓰기 액션 수.
    pub write_actions: u64,
    /// 예상 쿼터(units).
    pub estimated_units: u64,
    /// 예산 기준 예상 일수.
    pub estimated_days: u64,
}

/// 계획의 예상 쿼터를 계산한다.
pub fn estimate_quota(plan: &Plan, budget: u64) -> QuotaEstimate {
    let write_actions = plan.actions.len() as u64;
    let estimated_units = write_actions * WRITE_QUOTA_UNIT;
    let budget = budget.max(1);
    let estimated_days = if estimated_units == 0 {
        0
    } else {
        estimated_units.div_ceil(budget)
    };
    QuotaEstimate {
        write_actions,
        estimated_units,
        estimated_days,
    }
}

/// `stk plan --json` 출력.
pub fn summary_json(plan: &Plan, budget: u64) -> Value {
    let count = |predicate: fn(&ActionKind) -> bool| {
        plan.actions
            .iter()
            .filter(|action| predicate(&action.kind))
            .count()
    };

    let subscriptions_add = count(|kind| matches!(kind, ActionKind::Subscribe { .. }));
    let subscriptions_existing = plan
        .source
        .subscriptions
        .iter()
        .filter(|item| plan.target.subscription_ids.contains(&item.channel_id))
        .count();

    let playlists_create = count(|kind| matches!(kind, ActionKind::CreatePlaylist { .. }));
    let items_add = count(|kind| matches!(kind, ActionKind::AddPlaylistItem { .. }));
    let mut items_skipped = 0usize;
    let mut items_truncated = 0usize;
    for playlist in &plan.source.playlists {
        items_truncated += playlist.items.len().saturating_sub(PLAYLIST_ITEM_LIMIT);
        if let Some(target_id) = plan.playlist_targets.get(&playlist.id)
            && let Some(existing) = plan.target.playlist_items.get(target_id)
        {
            let present: HashSet<&str> =
                existing.iter().map(|item| item.video_id.as_str()).collect();
            items_skipped += playlist
                .items
                .iter()
                .filter(|item| present.contains(item.video_id.as_str()))
                .count();
        }
    }

    let likes_add = count(|kind| matches!(kind, ActionKind::Like { .. }));
    let likes_existing = plan
        .source
        .likes
        .iter()
        .filter(|item| plan.target.liked_ids.contains(&item.video_id))
        .count();

    let quota = estimate_quota(plan, budget);
    json!({
        "source": {
            "channel_id": plan.source.channel.id,
            "channel_title": plan.source.channel.title,
        },
        "target": {
            "channel_id": plan.target.channel.id,
            "channel_title": plan.target.channel.title,
        },
        "subscriptions": {
            "source": plan.source.subscriptions.len(),
            "target_existing": subscriptions_existing,
            "to_add": subscriptions_add,
        },
        "playlists": {
            "source": plan.source.playlists.len(),
            "to_create": playlists_create,
            "items_to_add": items_add,
            "items_skipped": items_skipped,
            "items_truncated": items_truncated,
        },
        "likes": {
            "source": plan.source.likes.len(),
            "target_existing": likes_existing,
            "to_add": likes_add,
        },
        "quota": {
            "write_actions": quota.write_actions,
            "estimated_units": quota.estimated_units,
            "estimated_days": quota.estimated_days,
        },
    })
}

/// 편의용 생성 함수. 테스트와 다른 모듈에서 스냅샷/대상 상태를 만들 때 쓴다.
pub fn sample_snapshot(
    account: Account,
    channel_id: &str,
    fetched_at: DateTime<Utc>,
    subscriptions: Vec<Subscription>,
    playlists: Vec<Playlist>,
    likes: Vec<LikedVideo>,
) -> Snapshot {
    Snapshot {
        account,
        channel: crate::youtube::ChannelInfo {
            id: channel_id.to_string(),
            title: format!("채널 {channel_id}"),
        },
        fetched_at,
        subscriptions,
        playlists,
        likes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::youtube::{ChannelInfo, Privacy};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T12:00:00Z")
            .expect("시각 파싱")
            .with_timezone(&Utc)
    }

    fn subscription(channel_id: &str) -> Subscription {
        Subscription {
            channel_id: channel_id.to_string(),
            title: format!("채널 {channel_id}"),
        }
    }

    fn item(video_id: &str, position: u64) -> PlaylistItemRef {
        PlaylistItemRef {
            item_id: format!("PI_{video_id}"),
            video_id: video_id.to_string(),
            title: format!("영상 {video_id}"),
            position,
        }
    }

    fn playlist(id: &str, title: &str, video_ids: &[&str]) -> Playlist {
        Playlist {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            privacy: Privacy::Private,
            items: video_ids
                .iter()
                .enumerate()
                .map(|(index, video_id)| item(video_id, index as u64))
                .collect(),
        }
    }

    fn summary(id: &str, title: &str) -> PlaylistSummary {
        PlaylistSummary {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            privacy: Privacy::Private,
            item_count: 0,
        }
    }

    fn target(channel_id: &str) -> TargetState {
        TargetState {
            channel: ChannelInfo {
                id: channel_id.to_string(),
                title: format!("채널 {channel_id}"),
            },
            subscription_ids: HashSet::new(),
            playlist_summaries: Vec::new(),
            playlist_items: HashMap::new(),
            liked_ids: HashSet::new(),
        }
    }

    fn all_types() -> Vec<DataType> {
        vec![
            DataType::Subscriptions,
            DataType::Playlists,
            DataType::Likes,
        ]
    }

    #[test]
    fn subscriptions_diff_dedupes() {
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            vec![
                subscription("UC_A"),
                subscription("UC_A"),
                subscription("UC_B"),
                subscription("UC_C"),
            ],
            Vec::new(),
            Vec::new(),
        );
        let mut target = target("UC_DST");
        target.subscription_ids.insert("UC_C".to_string());

        let plan = build_plan(&source, &target, &all_types(), now()).expect("계획 실패");
        let keys: Vec<&str> = plan
            .actions
            .iter()
            .map(|action| action.key.as_str())
            .collect();
        assert_eq!(keys, vec!["subscription:UC_A", "subscription:UC_B"]);
    }

    #[test]
    fn playlists_merge_by_title() {
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            Vec::new(),
            vec![
                playlist("PL_SRC_A", "음악", &["VID_1", "VID_2"]),
                playlist("PL_SRC_B", "여행", &["VID_3"]),
            ],
            Vec::new(),
        );
        let mut target = target("UC_DST");
        target
            .playlist_summaries
            .push(summary("PL_DST_MUSIC", "음악"));
        target
            .playlist_items
            .insert("PL_DST_MUSIC".to_string(), vec![item("VID_1", 0)]);

        let plan = build_plan(&source, &target, &all_types(), now()).expect("계획 실패");

        assert_eq!(
            plan.playlist_targets.get("PL_SRC_A").map(String::as_str),
            Some("PL_DST_MUSIC")
        );
        let create: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|action| match &action.kind {
                ActionKind::CreatePlaylist {
                    source_playlist_id, ..
                } => Some(source_playlist_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(create, vec!["PL_SRC_B"]);
        let added: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|action| match &action.kind {
                ActionKind::AddPlaylistItem { video_id, .. } => Some(video_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(added, vec!["VID_2", "VID_3"]);
    }

    #[test]
    fn system_playlists_excluded() {
        assert!(is_system_playlist(&playlist("LL123", "아무 제목", &[])));
        assert!(is_system_playlist(&playlist("WL123", "아무 제목", &[])));
        assert!(is_system_playlist(&playlist("HL123", "아무 제목", &[])));
        assert!(is_system_playlist(&playlist("UU123", "아무 제목", &[])));
        assert!(is_system_playlist(&playlist("PL123", "Liked videos", &[])));
        assert!(is_system_playlist(&playlist(
            "PL123",
            "좋아요 표시한 동영상",
            &[]
        )));
        assert!(is_system_playlist(&playlist("PL123", "Watch later", &[])));
        assert!(is_system_playlist(&playlist(
            "PL123",
            "나중에 볼 동영상",
            &[]
        )));
        assert!(!is_system_playlist(&playlist("PL123", "음악", &[])));

        // 대상의 시스템 플레이리스트와 제목이 같아도 재사용하지 않는다.
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            Vec::new(),
            vec![playlist("PL_SRC", "기록", &["VID_1"])],
            Vec::new(),
        );
        let mut target = target("UC_DST");
        target.playlist_summaries.push(summary("WL_DST", "기록"));
        let plan = build_plan(&source, &target, &all_types(), now()).expect("계획 실패");
        assert!(plan.playlist_targets.is_empty());
        assert!(
            plan.actions
                .iter()
                .any(|action| matches!(action.kind, ActionKind::CreatePlaylist { .. }))
        );
    }

    #[test]
    fn duplicate_source_titles_get_suffix() {
        let playlists = vec![
            playlist("PL_A", "음악", &[]),
            playlist("PL_B", "음악", &[]),
            playlist("PL_C", "음악", &[]),
            playlist("PL_D", "여행", &[]),
        ];
        let titles = display_titles(&playlists);
        assert_eq!(
            titles,
            vec![
                ("PL_A".to_string(), "음악".to_string()),
                ("PL_B".to_string(), "음악 (2)".to_string()),
                ("PL_C".to_string(), "음악 (3)".to_string()),
                ("PL_D".to_string(), "여행".to_string()),
            ]
        );
    }

    #[test]
    fn playlist_items_dedupe_and_cap_5000() {
        let video_ids: Vec<String> = (0..PLAYLIST_ITEM_LIMIT + 5)
            .map(|index| format!("VID_{index:05}"))
            .collect();
        let mut refs: Vec<&str> = video_ids.iter().map(String::as_str).collect();
        refs[1] = refs[0]; // 중복 항목
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            Vec::new(),
            vec![playlist("PL_SRC", "큰 목록", &refs)],
            Vec::new(),
        );

        let plan = build_plan(&source, &target("UC_DST"), &all_types(), now()).expect("계획 실패");
        let added = plan
            .actions
            .iter()
            .filter(|action| matches!(action.kind, ActionKind::AddPlaylistItem { .. }))
            .count();
        assert_eq!(
            added,
            PLAYLIST_ITEM_LIMIT - 1,
            "중복 1건과 상한 초과분을 잘라낸다"
        );

        let summary = summary_json(&plan, 9_500);
        assert_eq!(summary["playlists"]["items_truncated"], json!(5));
    }

    #[test]
    fn likes_diff_dedupes() {
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            Vec::new(),
            Vec::new(),
            vec![
                LikedVideo {
                    video_id: "VID_1".to_string(),
                    title: "영상 1".to_string(),
                },
                LikedVideo {
                    video_id: "VID_1".to_string(),
                    title: "영상 1".to_string(),
                },
                LikedVideo {
                    video_id: "VID_2".to_string(),
                    title: "영상 2".to_string(),
                },
            ],
        );
        let mut target = target("UC_DST");
        target.liked_ids.insert("VID_2".to_string());

        let plan = build_plan(&source, &target, &all_types(), now()).expect("계획 실패");
        let keys: Vec<&str> = plan
            .actions
            .iter()
            .map(|action| action.key.as_str())
            .collect();
        assert_eq!(keys, vec!["like:VID_1"]);
    }

    #[test]
    fn quota_estimate_matches_action_count() {
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            vec![subscription("UC_A"), subscription("UC_B")],
            Vec::new(),
            Vec::new(),
        );
        let plan = build_plan(&source, &target("UC_DST"), &all_types(), now()).expect("계획 실패");

        let estimate = estimate_quota(&plan, 9_500);
        assert_eq!(estimate.write_actions, 2);
        assert_eq!(estimate.estimated_units, 100);
        assert_eq!(estimate.estimated_days, 1);

        let summary = summary_json(&plan, 100);
        assert_eq!(summary["quota"]["estimated_days"], json!(1));
        assert_eq!(summary["subscriptions"]["to_add"], json!(2));
        assert_eq!(summary["subscriptions"]["source"], json!(2));
    }

    #[test]
    fn plan_rejects_same_channel() {
        let source = sample_snapshot(
            Account::Source,
            "UC_SAME",
            now(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let error = build_plan(&source, &target("UC_SAME"), &all_types(), now())
            .expect_err("같은 채널은 실패해야 한다");
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("같은 채널"));
    }

    #[test]
    fn action_order_is_subscriptions_then_playlists_then_likes() {
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            vec![subscription("UC_A")],
            vec![playlist("PL_A", "음악", &["VID_1"])],
            vec![LikedVideo {
                video_id: "VID_9".to_string(),
                title: "영상 9".to_string(),
            }],
        );
        let plan = build_plan(&source, &target("UC_DST"), &all_types(), now()).expect("계획 실패");
        let order: Vec<DataType> = plan
            .actions
            .iter()
            .map(|action| action.kind.data_type())
            .collect();
        assert_eq!(
            order,
            vec![
                DataType::Subscriptions,
                DataType::Playlists,
                DataType::Playlists,
                DataType::Likes,
            ]
        );
        assert!(matches!(
            plan.actions[1].kind,
            ActionKind::CreatePlaylist { .. }
        ));
        assert!(matches!(
            plan.actions[2].kind,
            ActionKind::AddPlaylistItem { .. }
        ));
    }

    #[test]
    fn plan_respects_selected_types() {
        let source = sample_snapshot(
            Account::Source,
            "UC_SRC",
            now(),
            vec![subscription("UC_A")],
            vec![playlist("PL_A", "음악", &["VID_1"])],
            vec![LikedVideo {
                video_id: "VID_9".to_string(),
                title: "영상 9".to_string(),
            }],
        );
        let plan = build_plan(&source, &target("UC_DST"), &[DataType::Playlists], now())
            .expect("계획 실패");
        assert_eq!(plan.actions.len(), 2);
        assert!(
            plan.actions
                .iter()
                .all(|action| action.kind.data_type() == DataType::Playlists)
        );
    }
}
