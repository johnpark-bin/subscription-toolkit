//! 좋아요 복사 동작 테스트.

mod support;

use support::{FakeApi, fast_options, run_actions, test_now};
use tempfile::TempDir;

use subscription_toolkit::account::Account;
use subscription_toolkit::error::AppError;
use subscription_toolkit::migrate::executor::RunSummary;
use subscription_toolkit::migrate::model::DataType;
use subscription_toolkit::migrate::{LIKE_LIMIT, build_plan, fetch_snapshot, fetch_target_state};
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::ApiError;

fn setup() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    paths.ensure_layout().expect("디렉터리 생성");
    (dir, paths)
}

fn source_api(likes: &[String]) -> FakeApi {
    let api = FakeApi::new();
    api.set_channel("UC_SRC", "원본 채널");
    for video_id in likes {
        api.add_like(video_id);
    }
    api
}

async fn push_likes(
    paths: &Paths,
    source: &FakeApi,
    target: &FakeApi,
) -> Result<RunSummary, AppError> {
    let snapshot = fetch_snapshot(source, Account::Source, test_now())
        .await
        .expect("스냅샷 수집 실패");
    let target_state = fetch_target_state(target, &snapshot.playlists)
        .await
        .expect("대상 상태 수집 실패");
    let plan = build_plan(&snapshot, &target_state, &[DataType::Likes], test_now())
        .expect("계획 계산 실패");
    run_actions(
        paths,
        target,
        &plan.actions,
        fast_options(),
        plan.playlist_targets.clone(),
        Default::default(),
    )
    .await
}

#[tokio::test]
async fn likes_capped_at_1000() {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    let likes: Vec<String> = (0..LIKE_LIMIT + 500)
        .map(|index| format!("VID_{index:05}"))
        .collect();
    let source = source_api(&likes);
    let target = FakeApi::new();

    let snapshot = fetch_snapshot(&source, Account::Source, test_now())
        .await
        .expect("스냅샷 수집 실패");
    assert_eq!(
        snapshot.likes.len(),
        LIKE_LIMIT,
        "YouTube API 상한(최근 1,000개)만 계획한다"
    );

    let target_state = fetch_target_state(&target, &snapshot.playlists)
        .await
        .expect("대상 상태 수집 실패");
    let plan = build_plan(&snapshot, &target_state, &[DataType::Likes], test_now())
        .expect("계획 계산 실패");
    assert_eq!(plan.actions.len(), LIKE_LIMIT);

    let summary = run_actions(
        &paths,
        &target,
        &plan.actions[..2],
        fast_options(),
        plan.playlist_targets.clone(),
        Default::default(),
    )
    .await
    .expect("좋아요 복사 실패");
    assert_eq!(summary.success_total(), 2);
}

#[tokio::test]
async fn like_disabled_video_skipped() {
    let (_dir, paths) = setup();
    let source = source_api(&["VID_1".to_string(), "VID_2".to_string()]);
    let target = FakeApi::new();
    target.queue_error(
        "like:VID_1",
        ApiError::Forbidden("Video rating is disabled. (동영상 평가가 비활성화됨)".to_string()),
    );

    let summary = push_likes(&paths, &source, &target)
        .await
        .expect("평가 불가 영상은 건너뛰고 계속해야 한다");
    assert_eq!(summary.skipped_total(), 1);
    assert_eq!(summary.success_total(), 1);
    assert_eq!(target.count_calls("like:VID_2"), 1);
}

#[tokio::test]
async fn like_email_unverified_aborts() {
    let (_dir, paths) = setup();
    let source = source_api(&["VID_1".to_string()]);
    let target = FakeApi::new();
    target.queue_error(
        "like:VID_1",
        ApiError::Auth("emailNotVerified (Google 계정 이메일 인증이 필요합니다)".to_string()),
    );

    let error = push_likes(&paths, &source, &target)
        .await
        .expect_err("이메일 미인증은 중단해야 한다");
    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("이메일"));
}

#[tokio::test]
async fn likes_copy_skips_already_liked() {
    let (_dir, paths) = setup();
    let source = source_api(&["VID_1".to_string(), "VID_2".to_string()]);
    let target = FakeApi::new();
    target.add_like("VID_2");

    let summary = push_likes(&paths, &source, &target)
        .await
        .expect("좋아요 복사 실패");
    assert_eq!(summary.success_total(), 1);
    assert_eq!(target.count_calls("like:VID_2"), 0);
    assert_eq!(target.count_calls("like:VID_1"), 1);
}
