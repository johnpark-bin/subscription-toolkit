//! 실행기 재개/쿼터/한도 동작 테스트.

mod support;

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use support::{FakeApi, fast_options, run_actions};
use tempfile::TempDir;

use subscription_toolkit::migrate::executor::{ExecOptions, Executor, ExecutorDeps};
use subscription_toolkit::migrate::journal::{Journal, JournalStatus};
use subscription_toolkit::migrate::model::{Action, ActionKind, DataType};
use subscription_toolkit::migrate::progress::ProgressReporter;
use subscription_toolkit::migrate::quota::QuotaLedger;
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::{ApiError, Privacy};

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-29T12:00:00Z")
        .expect("시각 파싱")
        .with_timezone(&Utc)
}

fn setup() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    paths.ensure_layout().expect("디렉터리 생성");
    (dir, paths)
}

fn subscribe(channel_id: &str) -> Action {
    Action {
        key: format!("subscription:{channel_id}"),
        kind: ActionKind::Subscribe {
            channel_id: channel_id.to_string(),
            title: format!("채널 {channel_id}"),
        },
    }
}

fn like(video_id: &str) -> Action {
    Action {
        key: format!("like:{video_id}"),
        kind: ActionKind::Like {
            video_id: video_id.to_string(),
            title: format!("영상 {video_id}"),
        },
    }
}

fn create_playlist(source_playlist_id: &str, title: &str) -> Action {
    Action {
        key: format!("playlist:{source_playlist_id}"),
        kind: ActionKind::CreatePlaylist {
            source_playlist_id: source_playlist_id.to_string(),
            title: title.to_string(),
            description: String::new(),
            privacy: Privacy::Private,
        },
    }
}

fn add_item(source_playlist_id: &str, video_id: &str) -> Action {
    Action {
        key: format!("playlist-item:{source_playlist_id}:{video_id}"),
        kind: ActionKind::AddPlaylistItem {
            source_playlist_id: source_playlist_id.to_string(),
            video_id: video_id.to_string(),
            title: format!("영상 {video_id}"),
        },
    }
}

#[tokio::test]
async fn limit_applies_per_type() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    let actions = vec![
        subscribe("UC_A"),
        subscribe("UC_B"),
        like("VID_1"),
        like("VID_2"),
    ];
    let opts = ExecOptions {
        limit: Some(1),
        ..fast_options()
    };

    let summary = run_actions(&paths, &api, &actions, opts, HashMap::new(), HashMap::new())
        .await
        .expect("실행 실패");

    assert_eq!(api.count_calls("subscribe:"), 1, "유형별 한도 1건");
    assert_eq!(api.count_calls("like:"), 1, "다른 유형도 각각 1건");
    assert_eq!(summary.types[&DataType::Subscriptions].success, 1);
    assert_eq!(summary.types[&DataType::Likes].success, 1);
    assert!(summary.types[&DataType::Subscriptions].limit_reached);
    assert!(summary.limit_reached());
}

#[tokio::test]
async fn failed_action_retried_next_run() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    api.queue_error(
        "like:VID_1",
        ApiError::Http {
            status: 500,
            body: "backend error".to_string(),
        },
    );
    let actions = vec![like("VID_1")];

    let error = run_actions(
        &paths,
        &api,
        &actions,
        fast_options(),
        HashMap::new(),
        HashMap::new(),
    )
    .await
    .expect_err("5xx는 실행을 중단해야 한다");
    assert_eq!(error.exit_code(), 1);

    let journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    assert!(
        journal
            .entries()
            .iter()
            .any(|entry| entry.key == "like:VID_1" && entry.status == JournalStatus::Failed),
        "실패한 액션은 Failed로 남아 재시도 대상이 된다"
    );

    let summary = run_actions(
        &paths,
        &api,
        &actions,
        fast_options(),
        HashMap::new(),
        HashMap::new(),
    )
    .await
    .expect("두 번째 실행은 성공해야 한다");
    assert_eq!(summary.success_total(), 1);
    assert_eq!(
        api.count_calls("like:VID_1"),
        2,
        "실패한 액션을 다시 호출한다"
    );
}

#[tokio::test]
async fn executor_restores_created_playlist_mapping() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();

    run_actions(
        &paths,
        &api,
        &[create_playlist("PL_SRC", "음악")],
        fast_options(),
        HashMap::new(),
        HashMap::new(),
    )
    .await
    .expect("플레이리스트 생성 실패");
    assert_eq!(api.playlist_titles(), vec!["음악".to_string()]);

    // 새 프로세스처럼 journal만 남은 상태에서 항목 추가.
    run_actions(
        &paths,
        &api,
        &[add_item("PL_SRC", "VID_1"), add_item("PL_SRC", "VID_2")],
        fast_options(),
        HashMap::new(),
        HashMap::new(),
    )
    .await
    .expect("항목 추가 실패");

    assert_eq!(
        api.count_calls("create_playlist:"),
        1,
        "journal에 기록된 매핑을 복원해 다시 만들지 않는다"
    );
    assert_eq!(api.playlist_items("PL_NEW_1").len(), 2);
}

#[tokio::test]
async fn journal_skips_completed_actions() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    let actions = vec![subscribe("UC_A"), subscribe("UC_B")];

    run_actions(
        &paths,
        &api,
        &actions,
        fast_options(),
        HashMap::new(),
        HashMap::new(),
    )
    .await
    .expect("첫 실행 실패");
    assert_eq!(api.count_calls("subscribe:"), 2);

    let summary = run_actions(
        &paths,
        &api,
        &actions,
        fast_options(),
        HashMap::new(),
        HashMap::new(),
    )
    .await
    .expect("두 번째 실행 실패");
    assert_eq!(
        api.count_calls("subscribe:"),
        2,
        "완료된 액션은 다시 쓰지 않는다"
    );
    assert_eq!(summary.success_total(), 0);
    assert_eq!(summary.skipped_total(), 2);
}

#[tokio::test]
async fn quota_exhaustion_aborts_with_quota_exit_code() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    let actions = vec![subscribe("UC_A"), subscribe("UC_B")];

    let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    // 예산 50 units = 쓰기 1회
    let mut quota = QuotaLedger::open(&paths, 50, now()).expect("쿼터 장부 열기 실패");
    let progress = ProgressReporter::with_tty(false);
    let mut executor = Executor::new(ExecutorDeps {
        api: &api,
        journal: &mut journal,
        quota: &mut quota,
        progress: &progress,
        opts: fast_options(),
        targets: HashMap::new(),
        expected_orders: HashMap::new(),
    });

    let error = executor
        .run(&actions, now())
        .await
        .expect_err("두 번째 액션에서 예산이 소진되어야 한다");
    assert_eq!(error.exit_code(), 4);
    assert_eq!(api.count_calls("subscribe:"), 1);

    let journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    assert_eq!(journal.complete_count(), 1, "첫 액션만 성공으로 남는다");
}
