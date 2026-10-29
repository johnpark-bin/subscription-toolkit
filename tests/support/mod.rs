//! 통합 테스트 공용 도우미.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use wiremock::{MockServer, Request, Respond, ResponseTemplate};

use subscription_toolkit::auth::{StaticTokenSource, TokenSource};
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

    /// 플레이리스트 요약.
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
        let items = state.items.entry(playlist_id.to_string()).or_default();
        let position = items.len() as u64;
        items.push(PlaylistItemRef {
            item_id: format!("PI_{playlist_id}_{position}"),
            video_id: video_id.to_string(),
            title: format!("영상 {video_id}"),
            position,
        });
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
