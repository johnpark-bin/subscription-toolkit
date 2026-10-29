//! 플레이리스트 복사와 순서 검증 테스트.

mod support;

use std::collections::HashMap;

use support::{FakeApi, fast_options, run_actions, test_now};
use tempfile::TempDir;

use subscription_toolkit::account::Account;
use subscription_toolkit::error::AppError;
use subscription_toolkit::migrate::executor::{ExecOptions, RunSummary};
use subscription_toolkit::migrate::journal::Journal;
use subscription_toolkit::migrate::model::{DataType, Plan};
use subscription_toolkit::migrate::{build_plan, fetch_snapshot, fetch_target_state};
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::{ApiError, Privacy};

fn setup() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    paths.ensure_layout().expect("디렉터리 생성");
    (dir, paths)
}

fn source_api(playlists: &[(&str, &str, &[&str])]) -> FakeApi {
    let api = FakeApi::new();
    api.set_channel("UC_SRC", "원본 채널");
    for (id, title, video_ids) in playlists {
        api.add_playlist(id, title, video_ids);
    }
    api
}

fn expected_orders(plan: &Plan) -> HashMap<String, Vec<String>> {
    plan.source
        .playlists
        .iter()
        .map(|playlist| {
            (
                playlist.id.clone(),
                playlist
                    .items
                    .iter()
                    .map(|item| item.video_id.clone())
                    .collect(),
            )
        })
        .collect()
}

async fn push_playlists(
    paths: &Paths,
    source: &FakeApi,
    target: &FakeApi,
    opts: ExecOptions,
) -> Result<(RunSummary, Plan), AppError> {
    let snapshot = fetch_snapshot(source, Account::Source, test_now())
        .await
        .expect("스냅샷 수집 실패");
    let target_state = fetch_target_state(target, &snapshot.playlists)
        .await
        .expect("대상 상태 수집 실패");
    let plan = build_plan(&snapshot, &target_state, &[DataType::Playlists], test_now())
        .expect("계획 계산 실패");
    let orders = expected_orders(&plan);
    let summary = run_actions(
        paths,
        target,
        &plan.actions,
        opts,
        plan.playlist_targets.clone(),
        orders,
    )
    .await?;
    Ok((summary, plan))
}

fn verify_options() -> ExecOptions {
    ExecOptions {
        verify_order: true,
        ..fast_options()
    }
}

#[tokio::test]
async fn playlist_copy_creates_private_playlist() {
    let (_dir, paths) = setup();
    let source = source_api(&[("PL_SRC", "음악", &["VID_1", "VID_2"])]);
    let target = FakeApi::new();

    let (summary, _plan) = push_playlists(&paths, &source, &target, fast_options())
        .await
        .expect("플레이리스트 복사 실패");

    assert_eq!(summary.success_total(), 3, "생성 1건 + 항목 2건");
    let playlists = target.playlists();
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].title, "음악");
    assert_eq!(playlists[0].privacy, Privacy::Private);
    assert_eq!(target.playlist_items(&playlists[0].id).len(), 2);
    assert_eq!(target.count_calls("create_playlist:"), 1);
}

#[tokio::test]
async fn playlist_copy_skips_existing_videos() {
    let (_dir, paths) = setup();
    let source = source_api(&[("PL_SRC", "음악", &["VID_1", "VID_2"])]);
    let target = FakeApi::new();
    target.add_playlist("PL_DST", "음악", &["VID_1"]);

    let (summary, plan) = push_playlists(&paths, &source, &target, fast_options())
        .await
        .expect("플레이리스트 복사 실패");

    assert_eq!(
        plan.playlist_targets.get("PL_SRC").map(String::as_str),
        Some("PL_DST")
    );
    assert_eq!(summary.success_total(), 1, "새 영상만 추가한다");
    assert_eq!(
        target.count_calls("create_playlist:"),
        0,
        "기존 목록을 재사용한다"
    );
    assert_eq!(target.playlist_items("PL_DST").len(), 2);
    assert_eq!(target.count_calls("add_playlist_item:PL_DST:VID_2"), 1);
}

#[tokio::test]
async fn playlist_order_repair_updates_positions() {
    let (_dir, paths) = setup();
    let source = source_api(&[("PL_SRC", "음악", &["VID_1", "VID_2"])]);
    let target = FakeApi::new().with_reverse_append(true);

    let (summary, _plan) = push_playlists(&paths, &source, &target, verify_options())
        .await
        .expect("순서 보정 실패");

    let playlist_id = target
        .playlists()
        .first()
        .map(|playlist| playlist.id.clone())
        .expect("플레이리스트가 있어야 한다");
    let order: Vec<String> = target
        .playlist_items(&playlist_id)
        .into_iter()
        .map(|item| item.video_id)
        .collect();
    assert_eq!(order, vec!["VID_1".to_string(), "VID_2".to_string()]);
    assert_eq!(summary.orders.len(), 1);
    assert!(summary.orders[0].repaired > 0, "위치를 보정해야 한다");
    assert!(target.count_calls("update_position:") > 0);
}

#[tokio::test]
async fn playlist_full_marks_remaining_skipped() {
    let (_dir, paths) = setup();
    let source = source_api(&[
        ("PL_A", "가", &["VID_1", "VID_2"]),
        ("PL_B", "나", &["VID_3"]),
    ]);
    let target = FakeApi::new();
    target.queue_error("add_playlist_item:PL_NEW_1:VID_1", ApiError::PlaylistFull);

    let (summary, _plan) = push_playlists(&paths, &source, &target, verify_options())
        .await
        .expect("가득 찬 목록은 건너뛰고 계속해야 한다");

    assert_eq!(
        target.count_calls("add_playlist_item:PL_NEW_1"),
        1,
        "가득 찬 뒤에는 같은 목록에 API를 호출하지 않는다"
    );
    assert_eq!(target.playlist_items("PL_NEW_1").len(), 0);
    assert_eq!(
        target.playlist_items("PL_NEW_2").len(),
        1,
        "다음 목록은 계속한다"
    );
    assert_eq!(summary.skipped_total(), 2, "VID_1, VID_2는 건너뛴다");

    let journal = Journal::open(&paths, test_now()).expect("journal 열기 실패");
    assert_eq!(
        journal
            .entries()
            .iter()
            .filter(|entry| entry.detail == Some(serde_json::json!("playlistFull")))
            .count(),
        2
    );
}

#[tokio::test]
async fn playlist_manual_sort_warning() {
    let (_dir, paths) = setup();
    let source = source_api(&[("PL_SRC", "음악", &["VID_1", "VID_2"])]);
    let target = FakeApi::new().with_reverse_append(true);
    target.queue_error(
        "update_position:PL_NEW_1:VID_1:0",
        ApiError::ManualSortRequired,
    );

    let (summary, _plan) = push_playlists(&paths, &source, &target, verify_options())
        .await
        .expect("수동 정렬 필요는 경고로 처리해야 한다");

    assert_eq!(summary.orders.len(), 1);
    let warning = summary.orders[0]
        .warning
        .as_deref()
        .expect("경고가 있어야 한다");
    assert!(warning.contains("수동"), "{warning}");
    assert!(summary.render().contains("경고:"));
}
