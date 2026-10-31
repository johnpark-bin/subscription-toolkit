//! API 쓰기 모드 조정 테스트.

mod support;

use std::collections::HashSet;

use chrono::{TimeZone, Utc};
use support::FakeApi;
use tempfile::TempDir;

use subscription_toolkit::browser::reconcile_via_api;
use subscription_toolkit::error::AppError;
use subscription_toolkit::migrate::journal::{Journal, JournalStatus};
use subscription_toolkit::migrate::quota::QuotaLedger;
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::ApiError;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 31, 12, 0, 0)
        .single()
        .expect("시각 생성")
}

fn setup() -> (TempDir, Paths) {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    paths.ensure_layout().expect("디렉터리 생성");
    (dir, paths)
}

#[tokio::test]
async fn api_mode_likes_missing_videos() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    let mut quota = QuotaLedger::open(&paths, 10_000, now()).expect("장부 열기 실패");
    let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    let source: Vec<String> = ["VID_1", "VID_2"].iter().map(|s| s.to_string()).collect();
    let target: HashSet<String> = ["VID_2".to_string()].into_iter().collect();

    let report = reconcile_via_api(
        &api,
        &mut quota,
        Some(&source),
        None,
        &target,
        &HashSet::new(),
        &mut journal,
        None,
        50,
        now(),
    )
    .await
    .expect("API 조정 실패");

    assert_eq!(report.likes_added, 1);
    assert_eq!(api.count_calls("like:VID_1"), 1);
    assert_eq!(
        api.count_calls("like:VID_2"),
        0,
        "이미 있으면 호출하지 않는다"
    );
    assert_eq!(quota.used(), 50);
    let journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    assert!(journal.is_complete("like:VID_1"));
}

#[tokio::test]
async fn api_mode_skips_unavailable_video() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    api.queue_error("like:VID_X", ApiError::NotFound("gone".to_string()));
    let mut quota = QuotaLedger::open(&paths, 10_000, now()).expect("장부 열기 실패");
    let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    let source = vec!["VID_X".to_string()];

    let report = reconcile_via_api(
        &api,
        &mut quota,
        Some(&source),
        None,
        &HashSet::new(),
        &HashSet::new(),
        &mut journal,
        None,
        50,
        now(),
    )
    .await
    .expect("건너뛰고 계속해야 한다");

    assert_eq!(report.likes_added, 0);
    assert_eq!(report.skipped, 1);
    let journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    let entry = journal
        .entries()
        .iter()
        .find(|entry| entry.key == "like:VID_X")
        .expect("기록");
    assert_eq!(entry.status, JournalStatus::Skipped);
}

#[tokio::test]
async fn api_mode_stops_when_budget_exhausted() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    let mut quota = QuotaLedger::open(&paths, 50, now()).expect("장부 열기 실패");
    let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    let source: Vec<String> = ["VID_1", "VID_2"].iter().map(|s| s.to_string()).collect();

    let error = reconcile_via_api(
        &api,
        &mut quota,
        Some(&source),
        None,
        &HashSet::new(),
        &HashSet::new(),
        &mut journal,
        None,
        50,
        now(),
    )
    .await
    .expect_err("예산 소진은 오류여야 한다");
    assert_eq!(error.exit_code(), 4);
    assert!(matches!(error, AppError::Quota(_)));
    assert_eq!(api.count_calls("like:"), 1, "예산만큼만 쓴다");
}

#[tokio::test]
async fn api_mode_subscribes_missing_channels() {
    let (_dir, paths) = setup();
    let api = FakeApi::new();
    let mut quota = QuotaLedger::open(&paths, 10_000, now()).expect("장부 열기 실패");
    let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
    let source = vec!["UC_A".to_string(), "UC_B".to_string()];
    let target: HashSet<String> = ["UC_B".to_string()].into_iter().collect();

    let report = reconcile_via_api(
        &api,
        &mut quota,
        None,
        Some(&source),
        &HashSet::new(),
        &target,
        &mut journal,
        None,
        50,
        now(),
    )
    .await
    .expect("API 조정 실패");

    assert_eq!(report.subscriptions_added, 1);
    assert_eq!(api.count_calls("subscribe:UC_A"), 1);
    assert_eq!(quota.used(), 50);
}
