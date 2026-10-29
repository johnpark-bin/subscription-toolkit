//! YouTube Data API 요청/응답 모델.

use serde::{Deserialize, Serialize};

/// 플레이리스트 공개 범위.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Privacy {
    /// 공개.
    Public,
    /// 일부 공개.
    Unlisted,
    /// 비공개.
    Private,
}

impl Privacy {
    /// API의 `privacyStatus` 값.
    pub fn api_status(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Unlisted => "unlisted",
            Self::Private => "private",
        }
    }

    /// API 문자열을 해석한다. 알 수 없으면 `None`.
    pub fn from_api_status(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Self::Public),
            "unlisted" => Some(Self::Unlisted),
            "private" => Some(Self::Private),
            _ => None,
        }
    }
}

/// 원본 계정의 구독.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// 구독한 채널 ID.
    pub channel_id: String,
    /// 채널 이름.
    pub title: String,
}

/// 플레이리스트 요약(항목 제외).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    /// 플레이리스트 ID.
    pub id: String,
    /// 제목.
    pub title: String,
    /// 공개 범위.
    pub privacy: Privacy,
    /// 항목 수.
    pub item_count: u64,
}

/// 플레이리스트 항목.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItemRef {
    /// 항목 ID.
    pub item_id: String,
    /// 영상 ID.
    pub video_id: String,
    /// 영상 제목.
    pub title: String,
    /// 0부터 시작하는 위치.
    pub position: u64,
}

/// 원본 계정의 플레이리스트와 항목.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Playlist {
    /// 플레이리스트 ID.
    pub id: String,
    /// 제목.
    pub title: String,
    /// 설명.
    pub description: String,
    /// 공개 범위.
    pub privacy: Privacy,
    /// 위치 오름차순 항목.
    pub items: Vec<PlaylistItemRef>,
}

/// 좋아요 표시한 영상.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LikedVideo {
    /// 영상 ID.
    pub video_id: String,
    /// 영상 제목.
    pub title: String,
}

/// `nextPageToken`을 쓰는 목록 응답.
#[derive(Debug, Deserialize)]
pub struct Page<T> {
    /// 항목 목록.
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    /// 다음 페이지 토큰.
    #[serde(rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

/// `resourceId` 공통 구조.
#[derive(Debug, Clone, Deserialize)]
pub struct ResourceId {
    /// 리소스 종류(`youtube#channel`, `youtube#video`).
    pub kind: Option<String>,
    /// 채널 ID.
    #[serde(rename = "channelId")]
    pub channel_id: Option<String>,
    /// 영상 ID.
    #[serde(rename = "videoId")]
    pub video_id: Option<String>,
}

/// `channels.list` 항목.
#[derive(Debug, Deserialize)]
pub struct ChannelItem {
    /// 채널 ID.
    pub id: String,
    /// 스니펫.
    pub snippet: ChannelSnippet,
}

/// 채널 스니펫.
#[derive(Debug, Deserialize)]
pub struct ChannelSnippet {
    /// 채널 이름.
    pub title: String,
}

/// `subscriptions.list` 항목.
#[derive(Debug, Deserialize)]
pub struct SubscriptionItem {
    /// 스니펫.
    pub snippet: SubscriptionSnippet,
}

/// 구독 스니펫.
#[derive(Debug, Deserialize)]
pub struct SubscriptionSnippet {
    /// 구독한 채널 이름.
    pub title: Option<String>,
    /// 구독 대상 리소스.
    #[serde(rename = "resourceId")]
    pub resource_id: Option<ResourceId>,
}

/// `playlists.list` 항목.
#[derive(Debug, Deserialize)]
pub struct PlaylistResource {
    /// 플레이리스트 ID.
    pub id: String,
    /// 스니펫.
    pub snippet: PlaylistSnippet,
    /// 상태.
    pub status: Option<PlaylistStatus>,
    /// 세부 정보.
    #[serde(rename = "contentDetails")]
    pub content_details: Option<PlaylistContentDetails>,
}

/// 플레이리스트 스니펫.
#[derive(Debug, Deserialize)]
pub struct PlaylistSnippet {
    /// 제목.
    pub title: Option<String>,
    /// 설명.
    pub description: Option<String>,
}

/// 플레이리스트 상태.
#[derive(Debug, Deserialize)]
pub struct PlaylistStatus {
    /// 공개 범위.
    #[serde(rename = "privacyStatus")]
    pub privacy_status: Option<String>,
}

/// 플레이리스트 세부 정보.
#[derive(Debug, Deserialize)]
pub struct PlaylistContentDetails {
    /// 항목 수.
    #[serde(rename = "itemCount")]
    pub item_count: Option<u64>,
}

/// `playlistItems.list` 항목.
#[derive(Debug, Deserialize)]
pub struct PlaylistItemResource {
    /// 항목 ID.
    pub id: Option<String>,
    /// 스니펫.
    pub snippet: PlaylistItemSnippet,
    /// 세부 정보.
    #[serde(rename = "contentDetails")]
    pub content_details: Option<PlaylistContentDetailsItem>,
}

/// 플레이리스트 항목 스니펫.
#[derive(Debug, Deserialize)]
pub struct PlaylistItemSnippet {
    /// 영상 제목.
    pub title: Option<String>,
    /// 대상 리소스.
    #[serde(rename = "resourceId")]
    pub resource_id: Option<ResourceId>,
    /// 위치.
    pub position: Option<u64>,
}

/// 플레이리스트 항목 세부 정보.
#[derive(Debug, Deserialize)]
pub struct PlaylistContentDetailsItem {
    /// 영상 ID.
    #[serde(rename = "videoId")]
    pub video_id: Option<String>,
}

/// `videos.list` 항목.
#[derive(Debug, Deserialize)]
pub struct VideoItem {
    /// 영상 ID.
    pub id: String,
    /// 스니펫.
    pub snippet: Option<VideoSnippet>,
}

/// 영상 스니펫.
#[derive(Debug, Deserialize)]
pub struct VideoSnippet {
    /// 제목.
    pub title: Option<String>,
}

/// `playlists.insert` 응답.
#[derive(Debug, Deserialize)]
pub struct CreatedPlaylist {
    /// 새 플레이리스트 ID.
    pub id: String,
}

/// API 오류 응답 본문.
#[derive(Debug, Deserialize)]
pub struct ErrorEnvelope {
    /// 오류 상세.
    pub error: ErrorBody,
}

/// API 오류 상세.
#[derive(Debug, Deserialize)]
pub struct ErrorBody {
    /// 전체 메시지.
    pub message: Option<String>,
    /// 개별 오류.
    #[serde(default)]
    pub errors: Vec<ErrorItem>,
}

/// API 개별 오류.
#[derive(Debug, Deserialize)]
pub struct ErrorItem {
    /// 원인 코드.
    pub reason: Option<String>,
    /// 원인 메시지.
    pub message: Option<String>,
}
