//! 구독 복사 동작 테스트.

mod support;

use support::{FakeApi, fast_options, run_actions, test_now};
use tempfile::TempDir;

use subscription_toolkit::account::Account;
use subscription_toolkit::error::AppError;
use subscription_toolkit::migrate::executor::RunSummary;
use subscription_toolkit::migrate::journal::Journal;
use subscription_toolkit::migrate::model::DataType;
use subscription_toolkit::migrate::{build_plan, fetch_snapshot, fetch_target_state};
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::ApiError;

fn setup() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    paths.ensure_layout().expect("디렉터리 생성");
    (dir, paths)
}

fn source_api(channels: &[&str]) -> FakeApi {
    let api = FakeApi::new();
    api.set_channel("UC_SRC", "원본 채널");
    for channel in channels {
        api.add_subscription(channel);
    }
    api
}

async fn push_subscriptions(
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
    let plan = build_plan(
        &snapshot,
        &target_state,
        &[DataType::Subscriptions],
        test_now(),
    )
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
async fn push_subscriptions_is_idempotent() {
    let (_dir, paths) = setup();
    let source = source_api(&["UC_A", "UC_B"]);
    let target = FakeApi::new();

    let first = push_subscriptions(&paths, &source, &target)
        .await
        .expect("첫 실행 실패");
    assert_eq!(first.success_total(), 2);
    assert_eq!(target.count_calls("subscribe:"), 2);

    let journal = Journal::open(&paths, test_now()).expect("journal 열기 실패");
    assert_eq!(journal.entries().len(), 2, "액션마다 journal 한 줄");

    let second = push_subscriptions(&paths, &source, &target)
        .await
        .expect("두 번째 실행 실패");
    assert_eq!(
        target.count_calls("subscribe:"),
        2,
        "이미 구독한 채널은 다시 insert하지 않는다"
    );
    assert_eq!(second.success_total(), 0);
    assert_eq!(
        Journal::open(&paths, test_now())
            .expect("journal 열기 실패")
            .entries()
            .len(),
        2
    );
}

#[tokio::test]
async fn subscription_limit_aborts() {
    let (_dir, paths) = setup();
    let source = source_api(&["UC_A", "UC_B"]);
    let target = FakeApi::new();
    target.queue_error(
        "subscribe:UC_A",
        ApiError::Forbidden(
            "The channel has reached the maximum number of subscriptions.".to_string(),
        ),
    );

    let error = push_subscriptions(&paths, &source, &target)
        .await
        .expect_err("구독 한도는 실행을 중단해야 한다");
    assert_eq!(error.exit_code(), 5);
    assert!(error.to_string().contains("구독 한도"), "{error}");

    let journal = Journal::open(&paths, test_now()).expect("journal 열기 실패");
    assert_eq!(journal.complete_count(), 0);
}

#[tokio::test]
async fn too_many_recent_subscriptions_requests_retry() {
    let (_dir, paths) = setup();
    let source = source_api(&["UC_A"]);
    let target = FakeApi::new();
    target.queue_error(
        "subscribe:UC_A",
        ApiError::RetryLater("Too many recent subscriptions.".to_string()),
    );

    let error = push_subscriptions(&paths, &source, &target)
        .await
        .expect_err("최근 구독 과다는 실행을 중단해야 한다");
    assert_eq!(error.exit_code(), 5);
    assert!(error.to_string().contains("몇 시간 후"), "{error}");
}

#[tokio::test]
async fn duplicate_subscription_is_skipped() {
    let (_dir, paths) = setup();
    let source = source_api(&["UC_A", "UC_B"]);
    let target = FakeApi::new();
    target.queue_error(
        "subscribe:UC_A",
        ApiError::AlreadyExists("The subscription already exists.".to_string()),
    );

    let summary = push_subscriptions(&paths, &source, &target)
        .await
        .expect("중복 구독은 건너뛰고 계속해야 한다");
    assert_eq!(summary.success_total(), 1);
    assert_eq!(summary.skipped_total(), 1);
    assert_eq!(target.count_calls("subscribe:UC_B"), 1);
}

#[tokio::test]
async fn own_channel_subscription_is_skipped() {
    let (_dir, paths) = setup();
    let source = source_api(&["UC_A"]);
    let target = FakeApi::new();
    target.queue_error(
        "subscribe:UC_A",
        ApiError::Forbidden("The subscription is not allowed.".to_string()),
    );

    let summary = push_subscriptions(&paths, &source, &target)
        .await
        .expect("구독 불가 채널은 건너뛴다");
    assert_eq!(summary.skipped_total(), 1);
    assert_eq!(summary.success_total(), 0);

    let journal = Journal::open(&paths, test_now()).expect("journal 열기 실패");
    let entry = &journal.entries()[0];
    assert_eq!(
        entry.detail,
        Some(serde_json::json!("subscriptionForbidden"))
    );
}

#[tokio::test]
async fn target_state_reflects_existing_subscriptions() {
    let source = source_api(&["UC_A", "UC_B"]);
    let target = FakeApi::new();
    target.add_subscription("UC_B");

    let snapshot = fetch_snapshot(&source, Account::Source, test_now())
        .await
        .expect("스냅샷 수집 실패");
    let target_state = fetch_target_state(&target, &snapshot.playlists)
        .await
        .expect("대상 상태 수집 실패");

    assert!(target_state.subscription_ids.contains("UC_B"));
    let plan = build_plan(
        &snapshot,
        &target_state,
        &[DataType::Subscriptions],
        test_now(),
    )
    .expect("계획 계산 실패");
    assert_eq!(plan.actions.len(), 1);
    assert_eq!(plan.actions[0].key, "subscription:UC_A");
}
