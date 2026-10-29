//! 시청기록 import/리포트/플레이리스트 테스트.

mod support;

use std::collections::HashSet;

use chrono::{TimeZone, Utc};
use support::{
    empty_target, history_fixture, run_actions, write_nested_takeout, write_takeout_zip,
};
use tempfile::TempDir;

use subscription_toolkit::error::AppError;
use subscription_toolkit::history::{
    DEFAULT_PLAYLIST_NAME, HistoryEntry, HistoryFile, build_report, import, parse_history,
    plan_history_playlist,
};
use subscription_toolkit::migrate::PLAYLIST_ITEM_LIMIT;
use subscription_toolkit::migrate::executor::ExecOptions;
use subscription_toolkit::migrate::journal::Journal;
use subscription_toolkit::migrate::model::ActionKind;
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::PlaylistSummary;
use subscription_toolkit::youtube::Privacy;

fn setup() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    paths.ensure_layout().expect("디렉터리 생성");
    (dir, paths)
}

fn fixture_file() -> HistoryFile {
    let entries = parse_history(&history_fixture()).expect("픽스처 파싱 실패");
    HistoryFile {
        imported_at: Utc::now(),
        source_file: "fixture".to_string(),
        entries,
    }
}

#[test]
fn history_parse_extracts_ids() {
    let entries = parse_history(&history_fixture()).expect("픽스처 파싱 실패");
    assert_eq!(entries.len(), 8);

    let first = &entries[0];
    assert_eq!(first.video_id.as_deref(), Some("aaaaaaaaaaa"));
    assert_eq!(first.title, "영상 A");
    assert!(
        first
            .channel_id
            .as_deref()
            .is_some_and(|id| id.starts_with("UC"))
    );
    assert_eq!(first.channel_name.as_deref(), Some("Channel A"));
    assert_eq!(
        first.watched_at,
        Some(
            Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0)
                .single()
                .expect("시각")
        )
    );
    assert_eq!(first.header, "YouTube");
    assert!(!first.is_ad);
}

#[test]
fn history_parse_skips_ads_and_removed() {
    let entries = parse_history(&history_fixture()).expect("픽스처 파싱 실패");
    let removed = &entries[4];
    assert_eq!(removed.video_id, None, "삭제된 영상은 videoId가 없다");
    assert_eq!(removed.title, "a video that has been removed");

    let ad = &entries[5];
    assert!(ad.is_ad, "Google Ads 항목은 광고로 표시한다");
    assert_eq!(ad.video_id.as_deref(), Some("ddddddddddd"));

    let invalid = &entries[7];
    assert_eq!(invalid.video_id, None, "잘못된 URL은 videoId가 없다");
}

#[test]
fn history_report_top_channels() {
    let file = fixture_file();
    let report = build_report(&file.entries, 5);

    assert_eq!(report.total_events, 8);
    assert_eq!(report.unique_videos, 5, "광고 영상도 고유 영상으로 센다");
    assert_eq!(report.unique_channels, 3, "광고 채널은 채널 집계에서 뺀다");
    assert_eq!(report.unparsed_events, 2);
    assert_eq!(report.ad_events, 1);
    assert_eq!(
        report.first_watched_at,
        Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).single()
    );
    assert_eq!(
        report.last_watched_at,
        Utc.with_ymd_and_hms(2026, 2, 5, 0, 0, 0).single()
    );
    assert_eq!(report.top_channels[0].name, "Channel A");
    assert_eq!(report.top_channels[0].count, 3);
    assert_eq!(report.top_videos[0].video_id, "aaaaaaaaaaa");
    assert_eq!(report.top_videos[0].count, 2);
    assert_eq!(
        report
            .monthly
            .iter()
            .map(|month| (month.month.as_str(), month.count))
            .collect::<Vec<_>>(),
        vec![("2026-01", 3), ("2026-02", 5)]
    );

    let markdown = report.to_markdown();
    assert!(markdown.contains("# YouTube 시청기록 분석"));
    assert!(markdown.contains("## 상위 채널"));
    assert!(markdown.contains("Channel A"));
    assert!(markdown.contains("전체 이벤트: 8"));

    let json = report.to_json();
    assert_eq!(json["unique_channels"], 3);
    assert_eq!(json["monthly"][0]["month"], "2026-01");
}

#[test]
fn history_import_finds_nested_file() {
    let (dir, paths) = setup();
    write_nested_takeout(dir.path());

    let file = import(&paths, dir.path(), Utc::now()).expect("import 실패");
    assert_eq!(file.entries.len(), 8);
    assert!(file.source_file.contains("watch-history.json"));
    assert!(paths.history().exists(), "data/history.json에 저장한다");

    let reloaded = subscription_toolkit::history::load(&paths).expect("저장된 시청기록 로드 실패");
    assert_eq!(reloaded.entries.len(), 8);
}

#[test]
fn history_import_zip() {
    let (dir, paths) = setup();
    let zip_path = dir.path().join("takeout.zip");
    write_takeout_zip(&zip_path);

    let file = import(&paths, &zip_path, Utc::now()).expect("zip import 실패");
    assert_eq!(file.entries.len(), 8);
    assert!(file.source_file.contains("watch-history.json"));
}

#[test]
fn history_import_without_file_reports_path() {
    let (dir, paths) = setup();
    let error =
        import(&paths, dir.path(), Utc::now()).expect_err("시청기록이 없으면 실패해야 한다");
    assert!(error.to_string().contains("watch-history.json"));
    assert_eq!(error.exit_code(), 1);
}

#[test]
fn history_playlist_dedupes_newest() {
    let file = fixture_file();
    let target = empty_target("UC_DST");
    let plan =
        plan_history_playlist(&file, &target, DEFAULT_PLAYLIST_NAME, None).expect("계획 실패");

    assert_eq!(
        plan.video_ids,
        vec![
            "eeeeeeeeeee".to_string(),
            "ccccccccccc".to_string(),
            "bbbbbbbbbbb".to_string(),
            "aaaaaaaaaaa".to_string(),
        ],
        "광고를 제외하고 최신순으로 중복을 제거한다"
    );
    assert_eq!(plan.existing_playlist_id, None);
    assert_eq!(plan.actions.len(), 5, "생성 1건 + 항목 4건");

    let create = plan.actions.first().expect("생성 액션이 먼저 와야 한다");
    assert_eq!(create.key, "playlist:history");
    match &create.kind {
        ActionKind::CreatePlaylist { privacy, title, .. } => {
            assert_eq!(*privacy, Privacy::Private);
            assert_eq!(title, DEFAULT_PLAYLIST_NAME);
        }
        other => panic!("생성 액션이 아니다: {other:?}"),
    }
}

#[test]
fn history_playlist_caps_5000() {
    let entries: Vec<HistoryEntry> = (0..PLAYLIST_ITEM_LIMIT + 5)
        .map(|index| HistoryEntry {
            video_id: Some(format!("VID_{index:07}")),
            title: format!("영상 {index}"),
            channel_name: Some("채널".to_string()),
            channel_id: Some("UC_TEST".to_string()),
            watched_at: Some(
                Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
                    .single()
                    .expect("시각"),
            ),
            header: "YouTube".to_string(),
            is_ad: false,
        })
        .collect();
    let file = HistoryFile {
        imported_at: Utc::now(),
        source_file: "generated".to_string(),
        entries,
    };

    let plan = plan_history_playlist(&file, &empty_target("UC_DST"), DEFAULT_PLAYLIST_NAME, None)
        .expect("계획 실패");
    assert_eq!(plan.video_ids.len(), PLAYLIST_ITEM_LIMIT);
    assert_eq!(plan.truncated, 5);

    let limited = plan_history_playlist(
        &file,
        &empty_target("UC_DST"),
        DEFAULT_PLAYLIST_NAME,
        Some(2),
    )
    .expect("계획 실패");
    assert_eq!(limited.video_ids.len(), 2);
    assert_eq!(limited.truncated, PLAYLIST_ITEM_LIMIT + 3);
}

#[test]
fn history_playlist_reuses_existing() {
    let file = fixture_file();
    let mut target = empty_target("UC_DST");
    target.playlist_summaries.push(PlaylistSummary {
        id: "PL_HISTORY".to_string(),
        title: DEFAULT_PLAYLIST_NAME.to_string(),
        description: String::new(),
        privacy: Privacy::Private,
        item_count: 1,
    });
    target.playlist_items.insert(
        "PL_HISTORY".to_string(),
        vec![subscription_toolkit::youtube::PlaylistItemRef {
            item_id: "PI_OLD".to_string(),
            video_id: "aaaaaaaaaaa".to_string(),
            title: "영상 A".to_string(),
            position: 0,
        }],
    );

    let plan =
        plan_history_playlist(&file, &target, DEFAULT_PLAYLIST_NAME, None).expect("계획 실패");
    assert_eq!(plan.existing_playlist_id.as_deref(), Some("PL_HISTORY"));
    assert_eq!(plan.skipped_existing, 1);
    assert!(
        plan.actions
            .iter()
            .all(|action| matches!(action.kind, ActionKind::AddPlaylistItem { .. })),
        "기존 목록을 재사용하면 생성하지 않는다"
    );
    assert_eq!(plan.actions.len(), 3);
}

#[tokio::test]
async fn history_playlist_creates_private_playlist_with_ordered_items() {
    let (_dir, paths) = setup();
    let file = fixture_file();
    let target = support::FakeApi::new().with_reverse_append(true);
    let target_state = empty_target("UC_DST");

    let plan = plan_history_playlist(&file, &target_state, DEFAULT_PLAYLIST_NAME, None)
        .expect("계획 실패");
    let mut targets = std::collections::HashMap::new();
    if let Some(existing) = &plan.existing_playlist_id {
        targets.insert(
            subscription_toolkit::history::HISTORY_PLAYLIST_ID.to_string(),
            existing.clone(),
        );
    }
    let mut orders = std::collections::HashMap::new();
    orders.insert(
        subscription_toolkit::history::HISTORY_PLAYLIST_ID.to_string(),
        plan.video_ids.clone(),
    );

    let summary = run_actions(
        &paths,
        &target,
        &plan.actions,
        ExecOptions {
            limit: None,
            delay: std::time::Duration::ZERO,
            verify_order: true,
        },
        targets,
        orders,
    )
    .await
    .expect("시청기록 플레이리스트 생성 실패");

    let playlist = target
        .playlists()
        .into_iter()
        .next()
        .expect("플레이리스트가 있어야 한다");
    assert_eq!(playlist.privacy, Privacy::Private);
    let order: Vec<String> = target
        .playlist_items(&playlist.id)
        .into_iter()
        .map(|item| item.video_id)
        .collect();
    assert_eq!(order, plan.video_ids, "순서 보정까지 끝나야 한다");
    assert_eq!(summary.orders.len(), 1);

    let journal = Journal::open(&paths, support::test_now()).expect("journal 열기 실패");
    assert!(
        journal.is_complete("playlist:history"),
        "시청기록 생성도 journal에 남는다"
    );
}

#[test]
fn history_playlist_without_valid_entries_errors() {
    let file = HistoryFile {
        imported_at: Utc::now(),
        source_file: "empty".to_string(),
        entries: vec![HistoryEntry {
            video_id: None,
            title: "삭제된 영상".to_string(),
            channel_name: None,
            channel_id: None,
            watched_at: None,
            header: "YouTube".to_string(),
            is_ad: false,
        }],
    };
    let error = plan_history_playlist(&file, &empty_target("UC_DST"), DEFAULT_PLAYLIST_NAME, None)
        .expect_err("유효 항목이 없으면 실패해야 한다");
    assert!(matches!(error, AppError::Config(_)));
    assert!(error.to_string().contains("재현할 시청기록이 없습니다"));

    // 광고만 있는 경우도 같다.
    let ads_only = HistoryFile {
        imported_at: Utc::now(),
        source_file: "ads".to_string(),
        entries: vec![HistoryEntry {
            video_id: Some("ddddddddddd".to_string()),
            title: "광고".to_string(),
            channel_name: None,
            channel_id: None,
            watched_at: None,
            header: "YouTube".to_string(),
            is_ad: true,
        }],
    };
    assert!(
        plan_history_playlist(
            &ads_only,
            &empty_target("UC_DST"),
            DEFAULT_PLAYLIST_NAME,
            None
        )
        .is_err()
    );
}

#[test]
fn history_report_uses_ad_filter_for_channels() {
    let file = fixture_file();
    let report = build_report(&file.entries, 10);
    let channels: HashSet<&str> = report
        .top_channels
        .iter()
        .map(|channel| channel.name.as_str())
        .collect();
    assert!(
        !channels.contains("Channel D"),
        "광고 채널은 상위 채널에서 제외한다"
    );
    assert_eq!(channels.len(), 3);
}
