//! YouTube Data API v3 HTTP 클라이언트.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Method;
use reqwest::header::ACCEPT;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::auth::{ChannelLookup, TokenSource};
use crate::error::AppError;
use crate::youtube::models::{
    ChannelItem, CreatedPlaylist, ErrorEnvelope, Page, PlaylistItemRef, PlaylistResource,
    PlaylistSummary, Privacy, Subscription, SubscriptionItem, VideoItem,
};
use crate::youtube::{ApiError, ChannelInfo, LikedVideo, YouTubeApi};

/// YouTube Data API 기본 주소.
pub const DEFAULT_BASE: &str = "https://www.googleapis.com/youtube/v3";

const USER_AGENT: &str = "subscription-toolkit/0.1";
const TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_SIZE: u32 = 50;
const MAX_PAGES: usize = 10_000;
const MAX_ATTEMPTS: u32 = 3;
const MAX_BODY_CHARS: usize = 300;

/// 환경 변수 `STK_API_BASE`가 있으면 그 주소, 없으면 기본 주소.
pub fn base_from_env() -> String {
    std::env::var("STK_API_BASE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

fn retry_base_from_env() -> Duration {
    let millis = std::env::var("STK_RETRY_BASE_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(1_000);
    Duration::from_millis(millis)
}

fn build_http() -> Result<reqwest::Client, AppError> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .map_err(|err| AppError::Config(format!("HTTP 클라이언트를 만들 수 없습니다: {err}")))
}

/// 실제 HTTP를 사용하는 [`YouTubeApi`] 구현.
pub struct HttpYouTube {
    http: reqwest::Client,
    base: String,
    tokens: Arc<dyn TokenSource>,
    retry_base: Duration,
}

impl HttpYouTube {
    /// 클라이언트를 만든다.
    pub fn new(base: impl Into<String>, tokens: Arc<dyn TokenSource>) -> Result<Self, AppError> {
        Ok(Self {
            http: build_http()?,
            base: base.into().trim_end_matches('/').to_string(),
            tokens,
            retry_base: retry_base_from_env(),
        })
    }

    /// 재시도 기본 지연을 바꾼다(테스트용).
    pub fn with_retry_base(mut self, retry_base: Duration) -> Self {
        self.retry_base = retry_base;
        self
    }

    /// 재시도 기본 지연.
    pub fn retry_base(&self) -> Duration {
        self.retry_base
    }

    async fn execute(
        &self,
        method: Method,
        path: &str,
        query: Vec<(&'static str, String)>,
        body: Option<Value>,
    ) -> Result<(u16, String), ApiError> {
        let mut force_refresh = false;
        let mut attempt = 0u32;
        loop {
            let token = self.tokens.access_token(force_refresh).await?;
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", self.base))
                .query(&query)
                .bearer_auth(token)
                .header(ACCEPT, "application/json");
            if let Some(body) = &body {
                request = request.json(body);
            }

            let response = request
                .send()
                .await
                .map_err(|err| ApiError::Network(err.to_string()))?;
            let status = response.status().as_u16();
            let text = response
                .text()
                .await
                .map_err(|err| ApiError::Network(err.to_string()))?;

            if (200..300).contains(&status) {
                return Ok((status, text));
            }

            let error = map_google_error(status, &text);
            if status == 401 && !force_refresh {
                force_refresh = true;
                continue;
            }

            // 409(SERVICE_UNAVAILABLE/ABORTED)는 YouTube가 간헐적으로 돌려주는 일시 오류다.
            let retryable =
                matches!(error, ApiError::RateLimited(_)) || status >= 500 || status == 409;
            if retryable && attempt < MAX_ATTEMPTS {
                let delay = self.retry_base * 2u32.pow(attempt);
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }
            return Err(error);
        }
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        query: Vec<(&'static str, String)>,
        body: Option<Value>,
    ) -> Result<T, ApiError> {
        let (_, text) = self.execute(method, path, query, body).await?;
        serde_json::from_str(&text)
            .map_err(|err| ApiError::Decode(format!("응답을 해석할 수 없습니다({path}): {err}")))
    }

    async fn send_unit(
        &self,
        method: Method,
        path: &str,
        query: Vec<(&'static str, String)>,
        body: Option<Value>,
    ) -> Result<(), ApiError> {
        self.execute(method, path, query, body).await?;
        Ok(())
    }

    async fn list_all<R, T, F>(
        &self,
        path: &'static str,
        query: Vec<(&'static str, String)>,
        map: F,
    ) -> Result<Vec<T>, ApiError>
    where
        R: DeserializeOwned,
        F: Fn(R) -> Option<T>,
    {
        let mut collected = Vec::new();
        let mut page_token: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let mut params = query.clone();
            params.push(("maxResults", PAGE_SIZE.to_string()));
            if let Some(token) = page_token.clone() {
                params.push(("pageToken", token));
            }
            let page: Page<R> = self.send(Method::GET, path, params, None).await?;
            for item in page.items {
                if let Some(mapped) = map(item) {
                    collected.push(mapped);
                }
            }
            match page.next_page_token {
                Some(token) if !token.is_empty() => page_token = Some(token),
                _ => return Ok(collected),
            }
        }
        Err(ApiError::Decode(
            "페이지가 너무 많아 목록을 끝까지 읽을 수 없습니다".to_string(),
        ))
    }
}

fn snippet_query() -> Vec<(&'static str, String)> {
    vec![
        ("part", "snippet".to_string()),
        ("mine", "true".to_string()),
    ]
}

#[async_trait]
impl YouTubeApi for HttpYouTube {
    async fn channel(&self) -> Result<ChannelInfo, ApiError> {
        let page: Page<ChannelItem> = self
            .send(
                Method::GET,
                "/channels",
                vec![
                    ("part", "snippet".to_string()),
                    ("mine", "true".to_string()),
                ],
                None,
            )
            .await?;
        page.items
            .into_iter()
            .next()
            .map(|item| ChannelInfo {
                id: item.id,
                title: item.snippet.title,
            })
            .ok_or_else(|| {
                ApiError::NotFound(
                    "YouTube 채널이 없습니다. youtube.com에서 채널을 먼저 만드세요.".to_string(),
                )
            })
    }

    async fn list_subscriptions(&self) -> Result<Vec<Subscription>, ApiError> {
        self.list_all::<SubscriptionItem, _, _>("/subscriptions", snippet_query(), |item| {
            let channel_id = item.snippet.resource_id.as_ref()?.channel_id.clone()?;
            Some(Subscription {
                channel_id,
                title: item.snippet.title.unwrap_or_default(),
            })
        })
        .await
    }

    async fn list_playlists(&self) -> Result<Vec<PlaylistSummary>, ApiError> {
        self.list_all::<PlaylistResource, _, _>(
            "/playlists",
            vec![
                ("part", "snippet,contentDetails,status".to_string()),
                ("mine", "true".to_string()),
            ],
            |item| {
                let title = item.snippet.title.unwrap_or_default();
                let description = item.snippet.description.unwrap_or_default();
                let privacy = item
                    .status
                    .as_ref()
                    .and_then(|status| status.privacy_status.as_deref())
                    .and_then(Privacy::from_api_status)
                    .unwrap_or(Privacy::Private);
                let item_count = item
                    .content_details
                    .and_then(|details| details.item_count)
                    .unwrap_or(0);
                Some(PlaylistSummary {
                    id: item.id,
                    title,
                    description,
                    privacy,
                    item_count,
                })
            },
        )
        .await
    }

    async fn list_playlist_items(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistItemRef>, ApiError> {
        let mut items = self
            .list_all::<crate::youtube::models::PlaylistItemResource, _, _>(
                "/playlistItems",
                vec![
                    ("part", "snippet,contentDetails".to_string()),
                    ("playlistId", playlist_id.to_string()),
                ],
                |item| {
                    let video_id = match item.content_details.and_then(|details| details.video_id) {
                        Some(video_id) => video_id,
                        None => item.snippet.resource_id?.video_id?,
                    };
                    Some(PlaylistItemRef {
                        item_id: item.id.unwrap_or_default(),
                        video_id,
                        title: item.snippet.title.unwrap_or_default(),
                        position: item.snippet.position.unwrap_or(0),
                    })
                },
            )
            .await?;
        items.sort_by_key(|item| item.position);
        Ok(items)
    }

    async fn list_liked_videos(&self) -> Result<Vec<LikedVideo>, ApiError> {
        self.list_all::<VideoItem, _, _>(
            "/videos",
            vec![
                ("part", "snippet".to_string()),
                ("myRating", "like".to_string()),
            ],
            |item| {
                Some(LikedVideo {
                    video_id: item.id,
                    title: item
                        .snippet
                        .and_then(|snippet| snippet.title)
                        .unwrap_or_default(),
                })
            },
        )
        .await
    }

    async fn subscribe(&self, channel_id: &str) -> Result<(), ApiError> {
        let body = json!({
            "snippet": {
                "resourceId": {
                    "kind": "youtube#channel",
                    "channelId": channel_id,
                }
            }
        });
        self.send_unit(
            Method::POST,
            "/subscriptions",
            vec![("part", "snippet".to_string())],
            Some(body),
        )
        .await
    }

    async fn create_playlist(
        &self,
        title: &str,
        description: &str,
        privacy: Privacy,
    ) -> Result<String, ApiError> {
        let body = json!({
            "snippet": {"title": title, "description": description},
            "status": {"privacyStatus": privacy.api_status()},
        });
        let created: CreatedPlaylist = self
            .send(
                Method::POST,
                "/playlists",
                vec![("part", "snippet,status".to_string())],
                Some(body),
            )
            .await?;
        Ok(created.id)
    }

    async fn add_playlist_item(&self, playlist_id: &str, video_id: &str) -> Result<(), ApiError> {
        let body = json!({
            "snippet": {
                "playlistId": playlist_id,
                "resourceId": {"kind": "youtube#video", "videoId": video_id},
            }
        });
        self.send_unit(
            Method::POST,
            "/playlistItems",
            vec![("part", "snippet".to_string())],
            Some(body),
        )
        .await
    }

    async fn update_playlist_item_position(
        &self,
        playlist_id: &str,
        item_id: &str,
        video_id: &str,
        position: u64,
    ) -> Result<(), ApiError> {
        let body = json!({
            "id": item_id,
            "snippet": {
                "playlistId": playlist_id,
                "resourceId": {"kind": "youtube#video", "videoId": video_id},
                "position": position,
            }
        });
        self.send(
            Method::PUT,
            "/playlistItems",
            vec![("part", "snippet".to_string())],
            Some(body),
        )
        .await
    }

    async fn like(&self, video_id: &str) -> Result<(), ApiError> {
        self.send_unit(
            Method::POST,
            "/videos/rate",
            vec![("id", video_id.to_string()), ("rating", "like".to_string())],
            None,
        )
        .await
    }
}

/// 액세스 토큰으로 채널만 확인하는 최소 클라이언트.
pub struct ChannelProbe {
    http: reqwest::Client,
    base: String,
}

impl ChannelProbe {
    /// 기본 주소(환경 변수 `STK_API_BASE` 반영)로 만든다.
    pub fn new() -> Result<Self, AppError> {
        Ok(Self {
            http: build_http()?,
            base: base_from_env().trim_end_matches('/').to_string(),
        })
    }

    /// API 주소를 직접 지정한다.
    pub fn with_base(base: impl Into<String>) -> Result<Self, AppError> {
        Ok(Self {
            http: build_http()?,
            base: base.into().trim_end_matches('/').to_string(),
        })
    }
}

#[async_trait]
impl ChannelLookup for ChannelProbe {
    async fn channel_for_token(&self, access_token: &str) -> Result<Option<ChannelInfo>, ApiError> {
        let response = self
            .http
            .get(format!("{}/channels", self.base))
            .query(&[("part", "snippet"), ("mine", "true")])
            .bearer_auth(access_token)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|err| ApiError::Network(err.to_string()))?;
        let status = response.status().as_u16();
        let text = response
            .text()
            .await
            .map_err(|err| ApiError::Network(err.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(map_google_error(status, &text));
        }
        let page: Page<ChannelItem> = serde_json::from_str(&text)
            .map_err(|err| ApiError::Decode(format!("채널 응답을 해석할 수 없습니다: {err}")))?;
        Ok(page.items.into_iter().next().map(|item| ChannelInfo {
            id: item.id,
            title: item.snippet.title,
        }))
    }
}

fn truncate_body(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.chars().count() <= MAX_BODY_CHARS {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(MAX_BODY_CHARS).collect();
        out.push('…');
        out
    }
}

/// Google API 오류 응답을 [`ApiError`]로 매핑한다.
pub fn map_google_error(status: u16, raw: &str) -> ApiError {
    let parsed: Option<ErrorEnvelope> = serde_json::from_str(raw).ok();
    let (reason, message) = match parsed {
        Some(envelope) => {
            let first = envelope.error.errors.first();
            (
                first.and_then(|item| item.reason.clone()),
                first
                    .and_then(|item| item.message.clone())
                    .or(envelope.error.message.clone())
                    .unwrap_or_default(),
            )
        }
        None => (None, String::new()),
    };

    if let Some(reason) = reason.as_deref() {
        match reason {
            "quotaExceeded" => return ApiError::QuotaExceeded,
            "rateLimitExceeded" | "userRateLimitExceeded" => {
                return ApiError::RateLimited(message);
            }
            "emailNotVerified" => {
                return ApiError::Auth(format!("{message} (Google 계정 이메일 인증이 필요합니다)"));
            }
            "maxPlaylistExceeded" | "playlistTitleRequired" => {
                return ApiError::Forbidden(message);
            }
            "subscriptionDuplicate" => return ApiError::AlreadyExists(message),
            "subscriptionForbidden" => {
                if message.contains("maximum number of subscriptions") {
                    return ApiError::Forbidden(message);
                }
                if message.contains("Too many recent subscriptions") {
                    return ApiError::RetryLater(message);
                }
                return ApiError::Forbidden(message);
            }
            "publisherNotFound" => return ApiError::NotFound(message),
            "playlistContainsMaximumNumberOfVideos" => return ApiError::PlaylistFull,
            "playlistOperationUnsupported" => return ApiError::Unsupported(message),
            "manualSortRequired" => return ApiError::ManualSortRequired,
            "videoNotFound" | "playlistNotFound" | "channelNotFound" => {
                return ApiError::NotFound(message);
            }
            "videoRatingDisabled" => {
                return ApiError::Forbidden(format!("{message} (동영상 평가가 비활성화됨)"));
            }
            _ => {}
        }
    }

    let fallback = if message.is_empty() {
        truncate_body(raw)
    } else {
        message
    };
    match status {
        401 => ApiError::Auth(format!("{fallback} — `stk auth login`을 다시 실행하세요.")),
        403 => ApiError::Forbidden(fallback),
        404 => ApiError::NotFound(fallback),
        429 => ApiError::RateLimited(fallback),
        _ => ApiError::Http {
            status,
            body: truncate_body(raw),
        },
    }
}
