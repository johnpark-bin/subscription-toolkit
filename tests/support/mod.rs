//! 통합 테스트 공용 도우미.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use subscription_toolkit::account::Account;
use subscription_toolkit::auth::{StaticTokenSource, TokenSource};
use subscription_toolkit::error::AppError;
use subscription_toolkit::migrate::executor::{ExecOptions, Executor, ExecutorDeps, RunSummary};
use subscription_toolkit::migrate::journal::Journal;
use subscription_toolkit::migrate::model::{Action, TargetState};
use subscription_toolkit::migrate::progress::ProgressReporter;
use subscription_toolkit::migrate::quota::QuotaLedger;
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::{
    ApiError, ChannelInfo, HttpYouTube, LikedVideo, PlaylistItemRef, PlaylistSummary, Privacy,
    Subscription, YouTubeApi,
};

/// 호출 순서대로 다른 응답을 돌려주는 wiremock 응답기.
pub struct Sequence {
    responses: Vec<ResponseTemplate>,
    index: AtomicUsize,
}

impl Sequence {
    /// 응답 목록으로 만든다.
    pub fn new(responses: Vec<ResponseTemplate>) -> Self {
        assert!(!responses.is_empty(), "응답 목록이 비어 있습니다");
        Self {
            responses,
            index: AtomicUsize::new(0),
        }
    }

    /// 지금까지 처리한 요청 수.
    pub fn hits(&self) -> usize {
        self.index.load(Ordering::SeqCst)
    }
}

impl Respond for Sequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let index = self.index.fetch_add(1, Ordering::SeqCst);
        self.responses
            .get(index)
            .or_else(|| self.responses.last())
            .cloned()
            .unwrap_or_else(|| ResponseTemplate::new(500))
    }
}

/// JSON 본문 응답.
pub fn json_response(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

/// Google API 형식 오류 응답.
pub fn api_error_response(status: u16, reason: &str, message: &str) -> ResponseTemplate {
    json_response(
        status,
        serde_json::json!({
            "error": {
                "code": status,
                "message": message,
                "errors": [{"reason": reason, "message": message}],
            }
        }),
    )
}

/// 고정 토큰을 쓰는 테스트 클라이언트.
pub fn static_client(server: &MockServer) -> HttpYouTube {
    client(server, Arc::new(StaticTokenSource::new("test-token")))
}

/// 토큰 소스를 지정한 테스트 클라이언트. 재시도 지연은 1ms로 줄인다.
pub fn client(server: &MockServer, tokens: Arc<dyn TokenSource>) -> HttpYouTube {
    HttpYouTube::new(format!("{}/youtube/v3", server.uri()), tokens)
        .expect("클라이언트 생성 실패")
        .with_retry_base(Duration::from_millis(1))
}

/// 강제 갱신 요청에 다른 토큰을 돌려주는 토큰 소스.
pub struct RefreshingToken;

#[async_trait]
impl TokenSource for RefreshingToken {
    async fn access_token(&self, force_refresh: bool) -> Result<String, ApiError> {
        Ok(if force_refresh {
            "refreshed-token".to_string()
        } else {
            "initial-token".to_string()
        })
    }
}

/// 요청의 Authorization 헤더 값.
pub fn authorization(request: &Request) -> Option<String> {
    request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 요청 URL의 쿼리 값.
pub fn query_param(request: &Request, key: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

/// 메모리에서 동작하는 가짜 YouTube API.
#[derive(Default)]
pub struct FakeApi {
    state: std::sync::Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    channel: Option<ChannelInfo>,
    subscriptions: Vec<Subscription>,
    playlists: Vec<PlaylistSummary>,
    items: std::collections::HashMap<String, Vec<PlaylistItemRef>>,
    likes: Vec<LikedVideo>,
    calls: Vec<String>,
    errors: Vec<(String, ApiError)>,
    created: u32,
    next_item: u32,
    reverse_append: bool,
}

impl FakeApi {
    /// 기본 채널(`UC_DST`)을 가진 가짜 API.
    pub fn new() -> Self {
        let api = Self::default();
        {
            let mut state = api.state.lock().expect("잠금");
            state.channel = Some(ChannelInfo {
                id: "UC_DST".to_string(),
                title: "대상 채널".to_string(),
            });
        }
        api
    }

    /// YouTube가 항목을 역순으로 덧붙이는 상황을 흉내 낸다(순서 보정 테스트용).
    pub fn with_reverse_append(self, reverse: bool) -> Self {
        {
            let mut state = self.state.lock().expect("잠금");
            state.reverse_append = reverse;
        }
        self
    }

    /// 채널을 바꾼다.
    pub fn set_channel(&self, id: &str, title: &str) {
        let mut state = self.state.lock().expect("잠금");
        state.channel = Some(ChannelInfo {
            id: id.to_string(),
            title: title.to_string(),
        });
    }

    /// 구독을 미리 넣는다.
    pub fn add_subscription(&self, channel_id: &str) {
        let mut state = self.state.lock().expect("잠금");
        state.subscriptions.push(Subscription {
            channel_id: channel_id.to_string(),
            title: format!("채널 {channel_id}"),
        });
    }

    /// 플레이리스트와 항목을 미리 넣는다.
    pub fn add_playlist(&self, id: &str, title: &str, video_ids: &[&str]) {
        let mut state = self.state.lock().expect("잠금");
        let items = video_ids
            .iter()
            .enumerate()
            .map(|(index, video_id)| PlaylistItemRef {
                item_id: format!("PI_{video_id}"),
                video_id: (*video_id).to_string(),
                title: format!("영상 {video_id}"),
                position: index as u64,
            })
            .collect();
        state.playlists.push(PlaylistSummary {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            privacy: Privacy::Private,
            item_count: video_ids.len() as u64,
        });
        state.items.insert(id.to_string(), items);
    }

    /// 좋아요를 미리 넣는다.
    pub fn add_like(&self, video_id: &str) {
        let mut state = self.state.lock().expect("잠금");
        state.likes.push(LikedVideo {
            video_id: video_id.to_string(),
            title: format!("영상 {video_id}"),
        });
    }

    /// 특정 작업 호출에 오류를 한 번 주입한다.
    pub fn queue_error(&self, call: &str, error: ApiError) {
        let mut state = self.state.lock().expect("잠금");
        state.errors.push((call.to_string(), error));
    }

    /// 호출 기록.
    pub fn calls(&self) -> Vec<String> {
        self.state.lock().expect("잠금").calls.clone()
    }

    /// 접두사로 시작하는 호출 수.
    pub fn count_calls(&self, prefix: &str) -> usize {
        let state = self.state.lock().expect("잠금");
        state
            .calls
            .iter()
            .filter(|call| call.starts_with(prefix))
            .count()
    }

    /// 플레이리스트 항목(위치순).
    pub fn playlist_items(&self, playlist_id: &str) -> Vec<PlaylistItemRef> {
        let state = self.state.lock().expect("잠금");
        let mut items = state.items.get(playlist_id).cloned().unwrap_or_default();
        items.sort_by_key(|item| item.position);
        items
    }

    /// 플레이리스트 요약 목록.
    pub fn playlists(&self) -> Vec<PlaylistSummary> {
        self.state.lock().expect("잠금").playlists.clone()
    }

    /// 플레이리스트 제목 목록.
    pub fn playlist_titles(&self) -> Vec<String> {
        let state = self.state.lock().expect("잠금");
        state
            .playlists
            .iter()
            .map(|playlist| playlist.title.clone())
            .collect()
    }

    fn take_error(state: &mut FakeState, call: &str) -> Option<ApiError> {
        state
            .errors
            .iter()
            .position(|(queued, _)| queued == call)
            .map(|index| state.errors.remove(index).1)
    }
}

#[async_trait]
impl YouTubeApi for FakeApi {
    async fn channel(&self) -> Result<ChannelInfo, ApiError> {
        let mut state = self.state.lock().expect("잠금");
        state.calls.push("channel".to_string());
        if let Some(error) = FakeApi::take_error(&mut state, "channel") {
            return Err(error);
        }
        state
            .channel
            .clone()
            .ok_or_else(|| ApiError::NotFound("채널이 없습니다".to_string()))
    }

    async fn list_subscriptions(&self) -> Result<Vec<Subscription>, ApiError> {
        let mut state = self.state.lock().expect("잠금");
        state.calls.push("list_subscriptions".to_string());
        if let Some(error) = FakeApi::take_error(&mut state, "list_subscriptions") {
            return Err(error);
        }
        Ok(state.subscriptions.clone())
    }

    async fn list_playlists(&self) -> Result<Vec<PlaylistSummary>, ApiError> {
        let mut state = self.state.lock().expect("잠금");
        state.calls.push("list_playlists".to_string());
        if let Some(error) = FakeApi::take_error(&mut state, "list_playlists") {
            return Err(error);
        }
        Ok(state.playlists.clone())
    }

    async fn list_playlist_items(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistItemRef>, ApiError> {
        let mut state = self.state.lock().expect("잠금");
        state
            .calls
            .push(format!("list_playlist_items:{playlist_id}"));
        if let Some(error) = FakeApi::take_error(&mut state, "list_playlist_items") {
            return Err(error);
        }
        let mut items = state.items.get(playlist_id).cloned().unwrap_or_default();
        items.sort_by_key(|item| item.position);
        Ok(items)
    }

    async fn list_liked_videos(&self) -> Result<Vec<LikedVideo>, ApiError> {
        let mut state = self.state.lock().expect("잠금");
        state.calls.push("list_liked_videos".to_string());
        if let Some(error) = FakeApi::take_error(&mut state, "list_liked_videos") {
            return Err(error);
        }
        Ok(state.likes.clone())
    }

    async fn subscribe(&self, channel_id: &str) -> Result<(), ApiError> {
        let call = format!("subscribe:{channel_id}");
        let mut state = self.state.lock().expect("잠금");
        state.calls.push(call.clone());
        if let Some(error) = FakeApi::take_error(&mut state, &call) {
            return Err(error);
        }
        state.subscriptions.push(Subscription {
            channel_id: channel_id.to_string(),
            title: format!("채널 {channel_id}"),
        });
        Ok(())
    }

    async fn create_playlist(
        &self,
        title: &str,
        description: &str,
        privacy: Privacy,
    ) -> Result<String, ApiError> {
        let call = format!("create_playlist:{title}");
        let mut state = self.state.lock().expect("잠금");
        state.calls.push(call.clone());
        if let Some(error) = FakeApi::take_error(&mut state, &call) {
            return Err(error);
        }
        state.created += 1;
        let id = format!("PL_NEW_{}", state.created);
        state.playlists.push(PlaylistSummary {
            id: id.clone(),
            title: title.to_string(),
            description: description.to_string(),
            privacy,
            item_count: 0,
        });
        state.items.insert(id.clone(), Vec::new());
        Ok(id)
    }

    async fn add_playlist_item(&self, playlist_id: &str, video_id: &str) -> Result<(), ApiError> {
        let call = format!("add_playlist_item:{playlist_id}:{video_id}");
        let mut state = self.state.lock().expect("잠금");
        state.calls.push(call.clone());
        if let Some(error) = FakeApi::take_error(&mut state, &call) {
            return Err(error);
        }
        state.next_item += 1;
        let item_id = format!("PI_{}", state.next_item);
        let reverse_append = state.reverse_append;
        let items = state.items.entry(playlist_id.to_string()).or_default();
        if reverse_append {
            let mut numbers: Vec<(String, String, String, u64)> = items
                .iter()
                .map(|item| {
                    (
                        item.item_id.clone(),
                        item.video_id.clone(),
                        item.title.clone(),
                        item.position,
                    )
                })
                .collect();
            numbers.insert(
                0,
                (item_id, video_id.to_string(), format!("영상 {video_id}"), 0),
            );
            *items = numbers
                .into_iter()
                .enumerate()
                .map(|(index, (item_id, video_id, title, _))| PlaylistItemRef {
                    item_id,
                    video_id,
                    title,
                    position: index as u64,
                })
                .collect();
        } else {
            let position = items.len() as u64;
            items.push(PlaylistItemRef {
                item_id,
                video_id: video_id.to_string(),
                title: format!("영상 {video_id}"),
                position,
            });
        }
        Ok(())
    }

    async fn update_playlist_item_position(
        &self,
        playlist_id: &str,
        item_id: &str,
        video_id: &str,
        position: u64,
    ) -> Result<(), ApiError> {
        let call = format!("update_position:{playlist_id}:{video_id}:{position}");
        let mut state = self.state.lock().expect("잠금");
        state.calls.push(call.clone());
        if let Some(error) = FakeApi::take_error(&mut state, &call) {
            return Err(error);
        }
        if let Some(items) = state.items.get_mut(playlist_id)
            && let Some(item) = items.iter_mut().find(|item| item.item_id == item_id)
        {
            item.position = position;
        }
        Ok(())
    }

    async fn like(&self, video_id: &str) -> Result<(), ApiError> {
        let call = format!("like:{video_id}");
        let mut state = self.state.lock().expect("잠금");
        state.calls.push(call.clone());
        if let Some(error) = FakeApi::take_error(&mut state, &call) {
            return Err(error);
        }
        state.likes.push(LikedVideo {
            video_id: video_id.to_string(),
            title: format!("영상 {video_id}"),
        });
        Ok(())
    }
}

/// 테스트에서 쓰는 고정 시각(태평양 자정과 무관한 시점).
pub fn test_now() -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(2026, 10, 29, 12, 0, 0)
        .single()
        .expect("시각 생성")
}

/// 실행기를 준비해 액션을 실행한다.
pub async fn run_actions(
    paths: &Paths,
    api: &FakeApi,
    actions: &[Action],
    opts: ExecOptions,
    targets: std::collections::HashMap<String, String>,
    expected_orders: std::collections::HashMap<String, Vec<String>>,
) -> Result<RunSummary, AppError> {
    let now = test_now();
    let mut journal = Journal::open(paths, now).expect("journal 열기 실패");
    let mut quota = QuotaLedger::open(paths, 9_500, now).expect("쿼터 장부 열기 실패");
    let progress = ProgressReporter::with_tty(false);
    let mut executor = Executor::new(ExecutorDeps {
        api,
        journal: &mut journal,
        quota: &mut quota,
        progress: &progress,
        opts,
        targets,
        expected_orders,
    });
    executor.run(actions, now).await
}

/// 실행 옵션 기본값(지연 없음).
pub fn fast_options() -> ExecOptions {
    ExecOptions {
        limit: None,
        delay: std::time::Duration::ZERO,
        verify_order: false,
        write_cost: subscription_toolkit::migrate::WRITE_QUOTA_UNIT,
    }
}

/// 시청기록 픽스처 경로.
pub const HISTORY_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/takeout/watch-history.json"
);

/// 픽스처 원문.
pub fn history_fixture() -> String {
    std::fs::read_to_string(HISTORY_FIXTURE).expect("픽스처 읽기 실패")
}

/// `Takeout/YouTube and YouTube Music/history/watch-history.json` 구조를 만든다.
pub fn write_nested_takeout(root: &std::path::Path) -> std::path::PathBuf {
    let dir = root
        .join("Takeout")
        .join("YouTube and YouTube Music")
        .join("history");
    std::fs::create_dir_all(&dir).expect("디렉터리 생성 실패");
    let path = dir.join("watch-history.json");
    std::fs::write(&path, history_fixture()).expect("픽스처 쓰기 실패");
    path
}

/// 같은 구조의 Takeout zip을 만든다.
pub fn write_takeout_zip(path: &std::path::Path) {
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    let file = std::fs::File::create(path).expect("zip 생성 실패");
    let mut writer = zip::ZipWriter::new(file);
    writer
        .start_file(
            "Takeout/YouTube and YouTube Music/history/watch-history.json",
            SimpleFileOptions::default(),
        )
        .expect("zip 항목 생성 실패");
    writer
        .write_all(history_fixture().as_bytes())
        .expect("zip 쓰기 실패");
    writer.finish().expect("zip 마무리 실패");
}

/// 빈 대상 상태.
pub fn empty_target(channel_id: &str) -> TargetState {
    TargetState {
        channel: ChannelInfo {
            id: channel_id.to_string(),
            title: "대상 채널".to_string(),
        },
        subscription_ids: std::collections::HashSet::new(),
        playlist_summaries: Vec::new(),
        playlist_items: std::collections::HashMap::new(),
        liked_ids: std::collections::HashSet::new(),
    }
}

/// CLI 종단간 테스트용 가짜 YouTube 서버.
pub struct MockYouTube {
    state: std::sync::Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    source: MockAccount,
    target: MockAccount,
}

#[derive(Default)]
struct MockAccount {
    channel: Option<ChannelInfo>,
    subscriptions: Vec<String>,
    playlists: Vec<PlaylistSummary>,
    items: std::collections::HashMap<String, Vec<PlaylistItemRef>>,
    likes: Vec<String>,
    created: u32,
    next_item: u32,
    subscription_inserts: usize,
    created_playlists: usize,
}

impl MockYouTube {
    /// 원본/대상 채널을 갖춘 서버 상태를 만든다.
    pub fn new() -> std::sync::Arc<Self> {
        let mock = Self {
            state: std::sync::Mutex::new(MockState {
                source: MockAccount {
                    channel: Some(ChannelInfo {
                        id: "UC_SRC".to_string(),
                        title: "원본 채널".to_string(),
                    }),
                    ..MockAccount::default()
                },
                target: MockAccount {
                    channel: Some(ChannelInfo {
                        id: "UC_DST".to_string(),
                        title: "대상 채널".to_string(),
                    }),
                    ..MockAccount::default()
                },
            }),
        };
        std::sync::Arc::new(mock)
    }

    /// mock 서버에 모든 요청을 이 상태로 연결한다.
    pub async fn mount(self: &std::sync::Arc<Self>, server: &MockServer) {
        let mock = std::sync::Arc::clone(self);
        Mock::given(wiremock::matchers::any())
            .respond_with(move |request: &Request| mock.respond(request))
            .mount(server)
            .await;
    }

    fn account(&self, request: &Request) -> Option<&std::sync::Mutex<MockState>> {
        let _ = request;
        Some(&self.state)
    }

    /// 구독을 미리 넣는다.
    pub fn add_subscription(&self, account: Account, channel_id: &str) {
        let mut state = self.state.lock().expect("잠금");
        let target = match account {
            Account::Source => &mut state.source,
            Account::Target => &mut state.target,
        };
        target.subscriptions.push(channel_id.to_string());
    }

    /// 구독 목록.
    pub fn subscriptions(&self, account: Account) -> Vec<String> {
        let state = self.state.lock().expect("잠금");
        let target = match account {
            Account::Source => &state.source,
            Account::Target => &state.target,
        };
        target.subscriptions.clone()
    }

    /// 구독 insert 성공 횟수.
    pub fn subscription_inserts(&self, account: Account) -> usize {
        let state = self.state.lock().expect("잠금");
        let target = match account {
            Account::Source => &state.source,
            Account::Target => &state.target,
        };
        target.subscription_inserts
    }

    /// 플레이리스트를 미리 넣는다.
    pub fn add_playlist(&self, account: Account, id: &str, title: &str, video_ids: &[&str]) {
        let mut state = self.state.lock().expect("잠금");
        let target = match account {
            Account::Source => &mut state.source,
            Account::Target => &mut state.target,
        };
        let items = video_ids
            .iter()
            .enumerate()
            .map(|(index, video_id)| PlaylistItemRef {
                item_id: format!("PI_{id}_{index}"),
                video_id: (*video_id).to_string(),
                title: format!("영상 {video_id}"),
                position: index as u64,
            })
            .collect();
        target.playlists.push(PlaylistSummary {
            id: id.to_string(),
            title: title.to_string(),
            description: String::new(),
            privacy: Privacy::Private,
            item_count: video_ids.len() as u64,
        });
        target.items.insert(id.to_string(), items);
    }

    /// 생성된 플레이리스트 수.
    pub fn created_playlists(&self, account: Account) -> usize {
        let state = self.state.lock().expect("잠금");
        match account {
            Account::Source => state.source.created_playlists,
            Account::Target => state.target.created_playlists,
        }
    }

    /// 플레이리스트 항목 영상 ID.
    pub fn playlist_items(&self, account: Account, playlist_id: &str) -> Vec<String> {
        let state = self.state.lock().expect("잠금");
        let target = match account {
            Account::Source => &state.source,
            Account::Target => &state.target,
        };
        target
            .items
            .get(playlist_id)
            .map(|items| items.iter().map(|item| item.video_id.clone()).collect())
            .unwrap_or_default()
    }

    /// 플레이리스트 ID 목록.
    pub fn playlist_ids(&self, account: Account) -> Vec<String> {
        let state = self.state.lock().expect("잠금");
        let target = match account {
            Account::Source => &state.source,
            Account::Target => &state.target,
        };
        target
            .playlists
            .iter()
            .map(|item| item.id.clone())
            .collect()
    }

    fn respond(&self, request: &Request) -> ResponseTemplate {
        let bearer = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default()
            .to_string();
        let is_source = match bearer.as_str() {
            "source-token" => true,
            "target-token" => false,
            _ => {
                return ResponseTemplate::new(401).set_body_json(serde_json::json!({
                    "error": {"code": 401, "message": "Invalid Credentials", "errors": [{"reason": "authError", "message": "Invalid Credentials"}]}
                }));
            }
        };

        let mut state = self.state.lock().expect("잠금");
        let account = if is_source {
            &mut state.source
        } else {
            &mut state.target
        };

        let path = request.url.path().trim_start_matches("/youtube/v3");
        let method = request.method.as_str();

        match (method, path) {
            ("GET", "/channels") => {
                let channel = account.channel.clone();
                match channel {
                    Some(channel) => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "items": [{"id": channel.id, "snippet": {"title": channel.title}}]
                    })),
                    None => ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": []})),
                }
            }
            ("GET", "/subscriptions") => {
                let items: Vec<serde_json::Value> = account
                    .subscriptions
                    .iter()
                    .map(|channel_id| {
                        serde_json::json!({
                            "snippet": {
                                "title": format!("채널 {channel_id}"),
                                "resourceId": {"kind": "youtube#channel", "channelId": channel_id}
                            }
                        })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": items}))
            }
            ("POST", "/subscriptions") => {
                let body: serde_json::Value = request
                    .body_json()
                    .unwrap_or_else(|_| serde_json::json!({}));
                let channel_id = body["snippet"]["resourceId"]["channelId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                account.subscriptions.push(channel_id);
                account.subscription_inserts += 1;
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "new-subscription"}))
            }
            ("GET", "/playlists") => {
                let items: Vec<serde_json::Value> = account
                    .playlists
                    .iter()
                    .map(|playlist| {
                        serde_json::json!({
                            "id": playlist.id,
                            "snippet": {"title": playlist.title, "description": playlist.description},
                            "status": {"privacyStatus": playlist.privacy.api_status()},
                            "contentDetails": {"itemCount": playlist.item_count}
                        })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": items}))
            }
            ("POST", "/playlists") => {
                let body: serde_json::Value = request
                    .body_json()
                    .unwrap_or_else(|_| serde_json::json!({}));
                account.created += 1;
                let id = format!("PL_NEW_{}", account.created);
                account.playlists.push(PlaylistSummary {
                    id: id.clone(),
                    title: body["snippet"]["title"].as_str().unwrap_or_default().to_string(),
                    description: body["snippet"]["description"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                    privacy: body["status"]["privacyStatus"]
                        .as_str()
                        .and_then(Privacy::from_api_status)
                        .unwrap_or(Privacy::Private),
                    item_count: 0,
                });
                account.items.insert(id.clone(), Vec::new());
                account.created_playlists += 1;
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": id}))
            }
            ("GET", "/playlistItems") => {
                let playlist_id = query_param(request, "playlistId").unwrap_or_default();
                let mut items = account.items.get(&playlist_id).cloned().unwrap_or_default();
                items.sort_by_key(|item| item.position);
                let payload: Vec<serde_json::Value> = items
                    .iter()
                    .map(|item| {
                        serde_json::json!({
                            "id": item.item_id,
                            "snippet": {
                                "title": item.title,
                                "position": item.position,
                                "resourceId": {"kind": "youtube#video", "videoId": item.video_id}
                            }
                        })
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": payload}))
            }
            ("POST", "/playlistItems") => {
                let body: serde_json::Value = request
                    .body_json()
                    .unwrap_or_else(|_| serde_json::json!({}));
                let playlist_id = body["snippet"]["playlistId"].as_str().unwrap_or_default().to_string();
                let video_id = body["snippet"]["resourceId"]["videoId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                account.next_item += 1;
                let item_id = format!("PI_NEW_{}", account.next_item);
                let items = account.items.entry(playlist_id).or_default();
                let position = items.len() as u64;
                items.push(PlaylistItemRef {
                    item_id,
                    video_id: video_id.clone(),
                    title: format!("영상 {video_id}"),
                    position,
                });
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "new-item"}))
            }
            ("PUT", "/playlistItems") => {
                let body: serde_json::Value = request
                    .body_json()
                    .unwrap_or_else(|_| serde_json::json!({}));
                let playlist_id = body["snippet"]["playlistId"].as_str().unwrap_or_default().to_string();
                let video_id = body["snippet"]["resourceId"]["videoId"].as_str().unwrap_or_default().to_string();
                let position = body["snippet"]["position"].as_u64().unwrap_or(0);
                if let Some(items) = account.items.get_mut(&playlist_id)
                    && let Some(item) = items.iter_mut().find(|item| item.video_id == video_id)
                {
                    item.position = position;
                }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "updated"}))
            }
            ("GET", "/videos") => {
                let items: Vec<serde_json::Value> = account
                    .likes
                    .iter()
                    .map(|video_id| serde_json::json!({"id": video_id, "snippet": {"title": format!("영상 {video_id}")}}))
                    .collect();
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"items": items}))
            }
            ("POST", "/videos/rate") => ResponseTemplate::new(204),
            _ => ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": {"code": 404, "message": format!("unknown path {method} {path}"), "errors": [{"reason": "notFound"}]}
            })),
        }
    }
}
