//! CLI 종단간 테스트.

mod support;

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use support::{MockYouTube, write_nested_takeout};
use tempfile::TempDir;
use wiremock::MockServer;

use subscription_toolkit::account::Account;
use subscription_toolkit::auth::StoredToken;

struct Harness {
    dir: TempDir,
    server: MockServer,
    mock: std::sync::Arc<MockYouTube>,
}

impl Harness {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let server = MockServer::start().await;
        let mock = MockYouTube::new();
        mock.mount(&server).await;
        Self { dir, server, mock }
    }

    fn home(&self) -> &Path {
        self.dir.path()
    }

    fn command(&self) -> Command {
        let mut command = Command::cargo_bin("stk").expect("바이너리를 찾을 수 없습니다");
        command
            .env("STK_HOME", self.home())
            .env("STK_API_BASE", format!("{}/youtube/v3", self.server.uri()))
            .env("STK_SOURCE_ACCESS_TOKEN", "source-token")
            .env("STK_TARGET_ACCESS_TOKEN", "target-token")
            .env("STK_RETRY_BASE_MS", "1");
        command
    }

    fn write_token(&self) {
        let tokens = self.home().join("tokens");
        std::fs::create_dir_all(&tokens).expect("토큰 디렉터리 생성");
        let token = StoredToken {
            client_id: "cid".to_string(),
            client_secret: "csecret".to_string(),
            refresh_token: "refresh".to_string(),
            access_token: Some("target-token".to_string()),
            expires_at: None,
            channel_id: "UC_DST".to_string(),
            channel_title: "대상 채널".to_string(),
        };
        std::fs::write(
            tokens.join("target.json"),
            serde_json::to_vec_pretty(&token).expect("직렬화"),
        )
        .expect("토큰 파일 쓰기");
    }

    fn write_data_files(&self) {
        let data = self.home().join("data");
        std::fs::create_dir_all(data.join("source")).expect("디렉터리 생성");
        for name in ["plan.json", "journal.jsonl", "quota.json", "history.json"] {
            std::fs::write(data.join(name), "{}").expect("데이터 파일 쓰기");
        }
        std::fs::write(data.join("source").join("snapshot.json"), "{}").expect("스냅샷 쓰기");
    }
}

#[tokio::test]
async fn cli_help_is_korean() {
    let harness = Harness::start().await;
    harness
        .command()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "구독·플레이리스트·좋아요를 다른 계정으로 복사",
        ))
        .stdout(predicate::str::contains("사용법"))
        .stdout(predicate::str::contains("Takeout 시청기록을 다룹니다"));

    harness
        .command()
        .args(["push", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("계획을 실행합니다"))
        .stdout(predicate::str::contains("쿼터 예산"));
}

#[tokio::test]
async fn plan_json_reports_counts() {
    let harness = Harness::start().await;
    for channel in ["UC_A", "UC_B", "UC_C"] {
        harness.mock.add_subscription(Account::Source, channel);
    }
    harness.mock.add_subscription(Account::Target, "UC_A");

    let output = harness
        .command()
        .args(["plan", "--json", "--types", "subscriptions"])
        .output()
        .expect("명령 실행 실패");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let summary: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON 파싱 실패");
    assert_eq!(summary["subscriptions"]["source"], 3);
    assert_eq!(summary["subscriptions"]["target_existing"], 1);
    assert_eq!(summary["subscriptions"]["to_add"], 2);
    assert_eq!(summary["source"]["channel_id"], "UC_SRC");
    assert_eq!(summary["target"]["channel_id"], "UC_DST");

    // 데이터 계약: plan.json의 필드 이름과 중첩 구조.
    let plan_path = harness.home().join("data").join("plan.json");
    assert!(plan_path.exists());
    let plan: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&plan_path).expect("계획 파일 읽기"))
            .expect("계획 JSON 파싱");
    assert!(plan["generated_at"].is_string());
    assert_eq!(plan["source"]["account"], "source");
    assert_eq!(plan["source"]["channel"]["id"], "UC_SRC");
    assert!(plan["source"]["fetched_at"].is_string());
    assert_eq!(plan["source"]["subscriptions"][0]["channel_id"], "UC_A");
    assert!(plan["source"]["playlists"].is_array());
    assert!(plan["source"]["likes"].is_array());
    assert_eq!(plan["target"]["channel"]["id"], "UC_DST");
    assert!(plan["target"]["subscription_ids"].is_array());
    assert!(plan["target"]["playlist_items"].is_object());
    assert!(plan["playlist_targets"].is_object());
    let action = &plan["actions"][0];
    assert_eq!(action["key"], "subscription:UC_B");
    assert_eq!(action["kind"]["kind"], "subscribe");
    assert_eq!(action["kind"]["channel_id"], "UC_B");
}

#[tokio::test]
async fn plan_without_token_exits_with_auth_code() {
    let harness = Harness::start().await;
    let output = harness
        .command()
        .env_remove("STK_SOURCE_ACCESS_TOKEN")
        .env_remove("STK_TARGET_ACCESS_TOKEN")
        .env_remove("STK_ACCESS_TOKEN")
        .args(["plan"])
        .output()
        .expect("명령 실행 실패");
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("stk auth login"), "{stderr}");
}

#[tokio::test]
async fn push_stops_when_budget_exhausted() {
    let harness = Harness::start().await;
    harness.mock.add_subscription(Account::Source, "UC_A");
    harness.mock.add_subscription(Account::Source, "UC_B");

    let output = harness
        .command()
        .args(["push", "subscriptions", "--budget", "50", "--yes"])
        .output()
        .expect("명령 실행 실패");
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(
        harness.mock.subscriptions(Account::Target).len(),
        1,
        "예산 50 units로는 쓰기 1회만 허용한다"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("쿼터"), "{stderr}");
}

#[tokio::test]
async fn push_is_noop_when_nothing_pending() {
    let harness = Harness::start().await;
    harness
        .command()
        .args(["push", "all", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("이미 완료"));
    assert_eq!(harness.mock.subscription_inserts(Account::Target), 0);
}

#[tokio::test]
async fn push_requires_confirmation_without_tty() {
    let harness = Harness::start().await;
    harness.mock.add_subscription(Account::Source, "UC_A");

    let output = harness
        .command()
        .args(["push", "subscriptions"])
        .output()
        .expect("명령 실행 실패");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
    assert_eq!(harness.mock.subscription_inserts(Account::Target), 0);
}

#[tokio::test]
async fn push_subscriptions_creates_and_resumes() {
    let harness = Harness::start().await;
    harness.mock.add_subscription(Account::Source, "UC_A");

    harness
        .command()
        .args(["push", "subscriptions", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("구독: 성공 1"));
    assert_eq!(harness.mock.subscription_inserts(Account::Target), 1);

    // 데이터 계약: journal.jsonl 한 줄 형식.
    let journal = std::fs::read_to_string(harness.home().join("data").join("journal.jsonl"))
        .expect("journal 읽기");
    let first: serde_json::Value =
        serde_json::from_str(journal.lines().next().expect("journal 첫 줄")).expect("journal 파싱");
    assert_eq!(first["key"], "subscription:UC_A");
    assert_eq!(first["status"], "success");
    assert!(first["at"].is_string());
    assert_eq!(first["detail"], serde_json::Value::Null);

    harness
        .command()
        .args(["push", "subscriptions", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("이미 완료"));
    assert_eq!(harness.mock.subscription_inserts(Account::Target), 1);
}

#[tokio::test]
async fn push_playlists_creates_private_playlist() {
    let harness = Harness::start().await;
    harness
        .mock
        .add_playlist(Account::Source, "PL_SRC", "음악", &["VID_1", "VID_2"]);

    harness
        .command()
        .args(["push", "playlists", "--yes"])
        .assert()
        .success();

    assert_eq!(harness.mock.created_playlists(Account::Target), 1);
    let playlists = harness.mock.playlist_ids(Account::Target);
    assert_eq!(playlists.len(), 1);
    assert_eq!(
        harness.mock.playlist_items(Account::Target, &playlists[0]),
        vec!["VID_1".to_string(), "VID_2".to_string()]
    );
}

#[tokio::test]
async fn auth_status_without_token_exits_zero() {
    let harness = Harness::start().await;
    harness
        .command()
        .args(["auth", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("로그인 안 됨"));
}

#[tokio::test]
async fn auth_status_json_reads_token_file() {
    let harness = Harness::start().await;
    harness.write_token();

    let output = harness
        .command()
        .args(["auth", "status", "--json"])
        .output()
        .expect("명령 실행 실패");
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON 파싱 실패");
    assert_eq!(status["target"]["logged_in"], true);
    assert_eq!(status["target"]["channel_id"], "UC_DST");
    assert_eq!(status["source"]["logged_in"], false);
}

#[test]
fn purge_removes_api_data_only() {
    let runtime = tokio::runtime::Runtime::new().expect("런타임");
    let harness = runtime.block_on(Harness::start());
    harness.write_token();
    harness.write_data_files();

    harness
        .command()
        .args(["purge", "--yes"])
        .assert()
        .success();

    let data = harness.home().join("data");
    for name in ["plan.json", "journal.jsonl", "quota.json", "history.json"] {
        assert!(!data.join(name).exists(), "{name}는 삭제되어야 한다");
    }
    assert!(!data.join("source").join("snapshot.json").exists());
    assert!(
        harness.home().join("tokens").join("target.json").exists(),
        "기본 purge는 토큰을 남긴다"
    );
}

#[test]
fn purge_tokens_removes_tokens() {
    let runtime = tokio::runtime::Runtime::new().expect("런타임");
    let harness = runtime.block_on(Harness::start());
    harness.write_token();
    harness.write_data_files();

    harness
        .command()
        .args(["purge", "--tokens", "--yes"])
        .assert()
        .success();

    assert!(!harness.home().join("tokens").join("target.json").exists());
    assert!(!harness.home().join("tokens").exists());
}

#[tokio::test]
async fn quota_sync_and_show_reflect_console_values() {
    let harness = Harness::start().await;
    harness
        .command()
        .args([
            "quota",
            "sync",
            "--used",
            "4929",
            "--budget",
            "10000",
            "--write-cost",
            "25",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("쿼터 장부를 맞췄습니다"));

    let output = harness
        .command()
        .args(["quota", "show", "--json"])
        .output()
        .expect("명령 실행 실패");
    assert!(output.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON 파싱 실패");
    assert_eq!(shown["today"]["used"], 4929);
    assert_eq!(shown["today"]["limit"], 10000);
    assert_eq!(shown["today"]["write_cost"], 25);

    harness
        .command()
        .args(["quota", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("남은 쓰기 202건"))
        .stdout(predicate::str::contains("10000 units"));

    // plan과 push는 장부의 단가·예산을 그대로 이어받는다.
    harness.mock.add_subscription(Account::Source, "UC_A");
    let output = harness
        .command()
        .args(["plan", "--types", "subscriptions", "--json"])
        .output()
        .expect("명령 실행 실패");
    let summary: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("JSON 파싱 실패");
    assert_eq!(
        summary["quota"]["estimated_units"], 25,
        "단가 25로 계산한다"
    );
    assert_eq!(summary["quota"]["write_actions"], 1);
}

#[tokio::test]
async fn status_reports_progress_and_requires_plan() {
    let harness = Harness::start().await;

    let output = harness
        .command()
        .args(["status"])
        .output()
        .expect("명령 실행 실패");
    assert_eq!(output.status.code(), Some(2), "계획이 없으면 사용법 오류");
    assert!(String::from_utf8_lossy(&output.stderr).contains("stk plan"));

    harness.mock.add_subscription(Account::Source, "UC_A");
    harness.mock.add_subscription(Account::Source, "UC_B");
    harness
        .command()
        .args(["plan", "--types", "subscriptions"])
        .assert()
        .success();

    let output = harness
        .command()
        .args(["status", "--json"])
        .output()
        .expect("명령 실행 실패");
    let before: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON 파싱 실패");
    assert_eq!(before["total_actions"], 2);
    assert_eq!(before["done_actions"], 0);
    assert_eq!(before["pending_actions"], 2);

    harness
        .command()
        .args(["push", "subscriptions", "--limit", "1", "--yes"])
        .assert()
        .success();

    let output = harness
        .command()
        .args(["status", "--json"])
        .output()
        .expect("명령 실행 실패");
    let after: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON 파싱 실패");
    assert_eq!(after["done_actions"], 1);
    assert_eq!(after["pending_actions"], 1);
    assert_eq!(after["types"][0]["data_type"], "subscriptions");
}

#[tokio::test]
async fn status_keeps_completed_count_after_replan() {
    let harness = Harness::start().await;
    harness.mock.add_subscription(Account::Source, "UC_A");
    harness.mock.add_subscription(Account::Source, "UC_B");

    harness
        .command()
        .args(["plan", "--types", "subscriptions"])
        .assert()
        .success();
    harness
        .command()
        .args(["push", "subscriptions", "--limit", "1", "--yes"])
        .assert()
        .success();

    // 대상 상태가 바뀌었으므로 계획을 다시 계산하면 완료분이 plan에서 빠진다.
    harness
        .command()
        .args(["plan", "--types", "subscriptions"])
        .assert()
        .success();

    let output = harness
        .command()
        .args(["status", "--json"])
        .output()
        .expect("명령 실행 실패");
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON 파싱");
    assert_eq!(status["done_actions"], 1, "journal 누적 완료를 센다");
    assert_eq!(status["pending_actions"], 1);
    assert_eq!(status["total_actions"], 2);
}

#[tokio::test]
async fn migrate_plans_and_runs_in_one_command() {
    let harness = Harness::start().await;
    harness.mock.add_subscription(Account::Source, "UC_A");

    harness
        .command()
        .args(["migrate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("구독: 성공 1"));
    assert_eq!(harness.mock.subscription_inserts(Account::Target), 1);
    assert!(harness.home().join("data").join("plan.json").exists());

    harness
        .command()
        .args(["migrate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("이미 완료"));
    assert_eq!(harness.mock.subscription_inserts(Account::Target), 1);
}

#[tokio::test]
async fn history_import_and_report_from_cli() {
    let harness = Harness::start().await;
    write_nested_takeout(harness.home());

    harness
        .command()
        .args(["history", "import", "--takeout"])
        .arg(harness.home())
        .assert()
        .success()
        .stdout(predicate::str::contains("시청기록 8건"));

    let output = harness
        .command()
        .args(["history", "report", "--format", "json"])
        .output()
        .expect("명령 실행 실패");
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON 파싱 실패");
    assert_eq!(report["total_events"], 8);
    assert_eq!(report["unique_channels"], 3);

    let out_path = harness.home().join("report.md");
    harness
        .command()
        .args(["history", "report", "--out"])
        .arg(&out_path)
        .assert()
        .success();
    let markdown = std::fs::read_to_string(&out_path).expect("리포트 읽기");
    assert!(markdown.contains("## 상위 채널"));
}

#[tokio::test]
async fn history_playlist_creates_private_playlist_from_cli() {
    let harness = Harness::start().await;
    write_nested_takeout(harness.home());
    harness
        .command()
        .args(["history", "import", "--takeout"])
        .arg(harness.home())
        .assert()
        .success();

    harness
        .command()
        .args(["history", "playlist", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("플레이리스트 '시청기록 (이전)'"));

    assert_eq!(harness.mock.created_playlists(Account::Target), 1);
    let playlists = harness.mock.playlist_ids(Account::Target);
    assert_eq!(playlists.len(), 1);
    assert_eq!(
        harness.mock.playlist_items(Account::Target, &playlists[0]),
        vec![
            "eeeeeeeeeee".to_string(),
            "ccccccccccc".to_string(),
            "bbbbbbbbbbb".to_string(),
            "aaaaaaaaaaa".to_string()
        ],
        "광고 제외·최신순·중복 제거 순서로 들어가야 한다"
    );
}

#[tokio::test]
async fn history_playlist_dry_run_reports_plan() {
    let harness = Harness::start().await;
    write_nested_takeout(harness.home());
    harness
        .command()
        .args(["history", "import", "--takeout"])
        .arg(harness.home())
        .assert()
        .success();

    harness
        .command()
        .args(["history", "playlist", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("항목 4건"))
        .stdout(predicate::str::contains("예상 쿼터"));

    assert_eq!(harness.mock.created_playlists(Account::Target), 0);
}
