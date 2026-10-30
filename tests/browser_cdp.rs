//! 브라우저(CDP) 실행 백엔드 테스트.
//!
//! 실제 Chrome 대신 DevTools WebSocket을 흉내 내는 서버를 띄워, 실행기·journal·
//! 재시도 로직을 검증한다. 페이지에서 실행되는 JavaScript 자체는 브라우저에서만
//! 검증할 수 있으므로 여기서는 스크립트에 포함된 엔드포인트/본문만 확인한다.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::{TimeZone, Utc};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

use subscription_toolkit::browser::{BrowserSyncOptions, sync_actions};
use subscription_toolkit::error::AppError;
use subscription_toolkit::migrate::journal::{Journal, JournalStatus};
use subscription_toolkit::migrate::model::{Action, ActionKind};
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::Privacy;

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0)
        .single()
        .expect("시각 생성")
}

/// DevTools WebSocket을 흉내 내는 서버.
struct FakeCdp {
    url: String,
    expressions: Arc<Mutex<Vec<String>>>,
    navigations: Arc<Mutex<Vec<String>>>,
}

async fn start_fake(script: Vec<Value>) -> FakeCdp {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("바인드 실패");
    let address = listener.local_addr().expect("주소");
    let expressions = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&expressions);
    let mut queue: VecDeque<Value> = script.into();
    let navigated: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let navigated_writer = Arc::clone(&navigated);
    let navigations: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let navigations_writer = Arc::clone(&navigations);

    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut ws) = accept_async(stream).await else {
            return;
        };
        while let Some(Ok(message)) = ws.next().await {
            let text = match message {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Message::Close(_) => break,
                _ => continue,
            };
            let Ok(request) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let Some(id) = request["id"].as_u64() else {
                continue;
            };
            if request["method"] == "Page.navigate" {
                if let Some(url) = request["params"]["url"].as_str() {
                    *navigated_writer.lock().expect("잠금") = Some(url.to_string());
                    navigations_writer
                        .lock()
                        .expect("잠금")
                        .push(url.to_string());
                }
                let reply = json!({"id": id, "result": {"frameId": "F1"}});
                let _ = ws.send(Message::Text(reply.to_string())).await;
                continue;
            }
            if request["method"] != "Runtime.evaluate" {
                let reply = json!({"id": id, "result": {}});
                let _ = ws.send(Message::Text(reply.to_string())).await;
                continue;
            }
            let expression = request["params"]["expression"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            // 페이지 로딩 확인은 스크립트 대기열을 소비하지 않고 즉시 완료로 답한다.
            let value = if expression.contains("location.href") {
                json!({
                    "href": navigated.lock().expect("잠금").clone().unwrap_or_default(),
                    "ready": "complete"
                })
            } else if expression == "document.readyState" {
                json!("complete")
            } else {
                recorder.lock().expect("잠금").push(expression);
                queue
                    .pop_front()
                    .unwrap_or_else(|| json!({"ok": true, "status": 200, "body": "{}"}))
            };
            let reply = json!({
                "id": id,
                "result": {"result": {"type": "object", "value": value}}
            });
            if ws.send(Message::Text(reply.to_string())).await.is_err() {
                break;
            }
        }
    });

    FakeCdp {
        url: format!("ws://{address}"),
        expressions,
        navigations,
    }
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

fn fast_options() -> BrowserSyncOptions {
    BrowserSyncOptions {
        delay: std::time::Duration::ZERO,
        jitter: std::time::Duration::ZERO,
        retry_base: std::time::Duration::from_millis(1),
        limit: None,
    }
}

struct Harness {
    dir: TempDir,
    paths: Paths,
}

fn harness() -> Harness {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    paths.ensure_layout().expect("디렉터리 생성");
    Harness { dir, paths }
}

async fn run(
    harness: &Harness,
    fake: &FakeCdp,
    actions: &[Action],
    opts: BrowserSyncOptions,
) -> Result<subscription_toolkit::browser::BrowserSyncReport, AppError> {
    let mut cdp = subscription_toolkit::browser::CdpClient::connect(&fake.url)
        .await
        .expect("CDP 연결 실패");
    let mut journal = Journal::open(&harness.paths, now()).expect("journal 열기 실패");
    let mut targets = HashMap::new();
    let result = sync_actions(&mut cdp, actions, &mut journal, &mut targets, &opts, now()).await;
    let _ = harness.dir.path();
    result
}

#[tokio::test]
async fn verify_account_accepts_matching_channel() {
    let fake = start_fake(vec![json!({
        "ok": true,
        "logged_in": true,
        "channel_id": "UC_DST",
        "session_index": 0
    })])
    .await;
    let mut cdp = subscription_toolkit::browser::CdpClient::connect(&fake.url)
        .await
        .expect("CDP 연결 실패");

    let account = subscription_toolkit::browser::verify_account(&mut cdp, "UC_DST")
        .await
        .expect("채널이 같으면 통과해야 한다");
    assert_eq!(account["channel_id"], "UC_DST");

    let navigations = fake.navigations.lock().expect("잠금").clone();
    assert_eq!(
        navigations,
        vec![
            "https://studio.youtube.com/".to_string(),
            "https://www.youtube.com/".to_string()
        ],
        "Studio에서 확인한 뒤 요청 오리진(www.youtube.com)으로 돌아온다"
    );
    assert!(
        fake.expressions
            .lock()
            .expect("잠금")
            .iter()
            .any(|expression| expression.contains("ytcfg")),
        "계정 확인 스크립트를 실행해야 한다"
    );
}

#[tokio::test]
async fn verify_account_rejects_other_channel() {
    let fake = start_fake(vec![json!({
        "ok": true,
        "logged_in": true,
        "channel_id": "UC_OTHER"
    })])
    .await;
    let mut cdp = subscription_toolkit::browser::CdpClient::connect(&fake.url)
        .await
        .expect("CDP 연결 실패");

    let error = subscription_toolkit::browser::verify_account(&mut cdp, "UC_DST")
        .await
        .expect_err("다른 채널은 거부해야 한다");
    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("다릅니다"));
}

#[tokio::test]
async fn verify_account_requires_login() {
    let fake = start_fake(vec![
        json!({"ok": true, "logged_in": false, "channel_id": null}),
    ])
    .await;
    let mut cdp = subscription_toolkit::browser::CdpClient::connect(&fake.url)
        .await
        .expect("CDP 연결 실패");

    let error = subscription_toolkit::browser::verify_account(&mut cdp, "UC_DST")
        .await
        .expect_err("로그아웃 상태는 실패해야 한다");
    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("로그인"));
}

#[tokio::test]
async fn subscribes_through_page_and_records_journal() {
    let harness = harness();
    let fake = start_fake(vec![
        json!({"ok": true, "status": 200, "body": "{}"}),
        json!({"ok": true, "status": 200, "body": "{}"}),
    ])
    .await;

    let report = run(
        &harness,
        &fake,
        &[subscribe("UC_A"), subscribe("UC_B")],
        fast_options(),
    )
    .await
    .expect("실행 실패");

    assert_eq!(report.success, 2);
    let journal = Journal::open(&harness.paths, now()).expect("journal 열기 실패");
    assert!(journal.is_complete("subscription:UC_A"));
    assert!(journal.is_complete("subscription:UC_B"));

    let expressions = fake.expressions.lock().expect("잠금").clone();
    assert_eq!(expressions.len(), 2);
    assert!(expressions[0].contains("/youtubei/v1/subscription/subscribe"));
    assert!(expressions[0].contains(r#""channelIds":["UC_A"]"#));
    assert!(expressions[0].contains("SAPISIDHASH"));
}

#[tokio::test]
async fn creates_playlist_and_uses_returned_id_for_items() {
    let harness = harness();
    let fake = start_fake(vec![
        json!({"ok": true, "status": 200, "body": "{\"playlistId\":\"PL_NEW\"}"}),
        json!({"ok": true, "status": 200, "body": "{\"status\":\"STATUS_SUCCEEDED\"}"}),
        json!({"ok": true, "status": 200, "body": "{\"status\":\"STATUS_SUCCEEDED\"}"}),
    ])
    .await;

    let report = run(
        &harness,
        &fake,
        &[
            create_playlist("PL_SRC", "음악"),
            add_item("PL_SRC", "VID_1"),
            add_item("PL_SRC", "VID_2"),
        ],
        fast_options(),
    )
    .await
    .expect("실행 실패");

    assert_eq!(report.success, 3);
    assert_eq!(
        report.target_playlists,
        vec![("PL_SRC".to_string(), "PL_NEW".to_string())]
    );

    let expressions = fake.expressions.lock().expect("잠금").clone();
    assert!(expressions[0].contains("/youtubei/v1/playlist/create"));
    assert!(expressions[1].contains(r#""playlistId":"PL_NEW""#));
    assert!(expressions[1].contains(r#""addedVideoId":"VID_1""#));
    assert!(expressions[2].contains(r#""addedVideoId":"VID_2""#));

    let journal = Journal::open(&harness.paths, now()).expect("journal 열기 실패");
    let entry = journal
        .entries()
        .iter()
        .find(|entry| entry.key == "playlist:PL_SRC")
        .expect("생성 기록이 있어야 한다");
    assert_eq!(entry.detail, Some(json!({"target_playlist_id": "PL_NEW"})));
}

#[tokio::test]
async fn skips_actions_already_in_journal() {
    let harness = harness();
    let fake = start_fake(vec![json!({"ok": true, "status": 200, "body": "{}"})]).await;

    let mut journal = Journal::open(&harness.paths, now()).expect("journal 열기 실패");
    journal
        .record("subscription:UC_A", JournalStatus::Success, None, now())
        .expect("기록 실패");
    drop(journal);

    let report = run(
        &harness,
        &fake,
        &[subscribe("UC_A"), subscribe("UC_B")],
        fast_options(),
    )
    .await
    .expect("실행 실패");

    assert_eq!(report.already_done, 1);
    assert_eq!(report.success, 1);
    let expressions = fake.expressions.lock().expect("잠금").clone();
    assert_eq!(expressions.len(), 1, "완료된 항목은 요청하지 않는다");
}

#[tokio::test]
async fn retries_when_rate_limited() {
    let harness = harness();
    let fake = start_fake(vec![
        json!({"ok": false, "status": 429, "body": "rate limit"}),
        json!({"ok": false, "status": 503, "body": "backend"}),
        json!({"ok": true, "status": 200, "body": "{}"}),
    ])
    .await;

    let report = run(&harness, &fake, &[like("VID_1")], fast_options())
        .await
        .expect("재시도 후 성공해야 한다");
    assert_eq!(report.success, 1);
    assert_eq!(fake.expressions.lock().expect("잠금").len(), 3);
}

#[tokio::test]
async fn aborts_on_auth_problem() {
    let harness = harness();
    let fake = start_fake(vec![
        json!({"ok": false, "status": 401, "body": "login required"}),
    ])
    .await;

    let error = run(&harness, &fake, &[subscribe("UC_A")], fast_options())
        .await
        .expect_err("인증 오류는 중단해야 한다");
    assert_eq!(error.exit_code(), 3);
    let journal = Journal::open(&harness.paths, now()).expect("journal 열기 실패");
    assert_eq!(
        journal.complete_count(),
        0,
        "실패한 액션은 완료로 기록하지 않는다"
    );
}

#[tokio::test]
async fn gives_up_after_retry_limit() {
    let harness = harness();
    let fake = start_fake(vec![
        json!({"ok": false, "status": 429, "body": "rate limit"}),
        json!({"ok": false, "status": 429, "body": "rate limit"}),
        json!({"ok": false, "status": 429, "body": "rate limit"}),
        json!({"ok": false, "status": 429, "body": "rate limit"}),
    ])
    .await;

    let error = run(&harness, &fake, &[like("VID_1")], fast_options())
        .await
        .expect_err("재시도 한도를 넘으면 실패해야 한다");
    assert_eq!(error.exit_code(), 5);
    assert_eq!(fake.expressions.lock().expect("잠금").len(), 4);
}

#[tokio::test]
async fn applies_limit_per_type() {
    let harness = harness();
    let fake = start_fake(vec![]).await;
    let opts = BrowserSyncOptions {
        limit: Some(1),
        ..fast_options()
    };

    let report = run(
        &harness,
        &fake,
        &[subscribe("UC_A"), subscribe("UC_B"), like("VID_1")],
        opts,
    )
    .await
    .expect("실행 실패");

    assert_eq!(report.success, 2, "유형별 1건씩");
    assert_eq!(fake.expressions.lock().expect("잠금").len(), 2);
}

#[tokio::test]
async fn duplicate_subscription_is_skipped() {
    let harness = harness();
    let fake = start_fake(vec![json!({
        "ok": false,
        "status": 400,
        "body": "{\"error\":{\"message\":\"Subscription already exists\"}}"
    })])
    .await;

    let report = run(&harness, &fake, &[subscribe("UC_A")], fast_options())
        .await
        .expect("중복은 건너뛴다");
    assert_eq!(report.skipped, 1);
    assert_eq!(report.success, 0);

    let journal = Journal::open(&harness.paths, now()).expect("journal 열기 실패");
    assert!(
        journal.is_complete("subscription:UC_A"),
        "건너뜀도 완료로 본다"
    );
}

#[tokio::test]
async fn add_item_without_mapping_fails_fast() {
    let harness = harness();
    let fake = start_fake(vec![]).await;

    let error = run(
        &harness,
        &fake,
        &[add_item("PL_MISSING", "VID_1")],
        fast_options(),
    )
    .await
    .expect_err("매핑이 없으면 실패해야 한다");
    assert_eq!(error.exit_code(), 1);
    assert!(error.to_string().contains("대상 플레이리스트 ID"));
    assert!(fake.expressions.lock().expect("잠금").is_empty());
}
