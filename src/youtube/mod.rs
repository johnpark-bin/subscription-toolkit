//! YouTube Data API v3 클라이언트.

pub mod client;
pub mod error;
pub mod models;

pub use client::{ChannelProbe, HttpYouTube};
pub use error::ApiError;
pub use models::{LikedVideo, Playlist, PlaylistItemRef, PlaylistSummary, Privacy, Subscription};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// 채널 식별 정보.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelInfo {
    /// 채널 ID(`UC...`).
    pub id: String,
    /// 채널 표시 이름.
    pub title: String,
}

/// 이전 작업이 사용하는 YouTube API 동작.
///
/// CLI는 이 trait만 쓰고 HTTP를 직접 호출하지 않는다.
#[async_trait]
pub trait YouTubeApi: Send + Sync {
    /// 인증된 사용자의 채널.
    async fn channel(&self) -> Result<ChannelInfo, ApiError>;

    /// 인증된 사용자의 구독 전체.
    async fn list_subscriptions(&self) -> Result<Vec<Subscription>, ApiError>;

    /// 인증된 사용자가 소유한 플레이리스트(항목 제외).
    async fn list_playlists(&self) -> Result<Vec<PlaylistSummary>, ApiError>;

    /// 플레이리스트 항목(위치 오름차순).
    async fn list_playlist_items(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistItemRef>, ApiError>;

    /// 좋아요 표시한 영상(최근 1,000개까지).
    async fn list_liked_videos(&self) -> Result<Vec<LikedVideo>, ApiError>;

    /// 채널을 구독한다.
    async fn subscribe(&self, channel_id: &str) -> Result<(), ApiError>;

    /// 플레이리스트를 만들고 ID를 돌려준다.
    async fn create_playlist(
        &self,
        title: &str,
        description: &str,
        privacy: Privacy,
    ) -> Result<String, ApiError>;

    /// 플레이리스트 끝에 영상을 추가한다.
    async fn add_playlist_item(&self, playlist_id: &str, video_id: &str) -> Result<(), ApiError>;

    /// 플레이리스트 항목의 위치를 바꾼다.
    async fn update_playlist_item_position(
        &self,
        playlist_id: &str,
        item_id: &str,
        video_id: &str,
        position: u64,
    ) -> Result<(), ApiError>;

    /// 영상에 좋아요를 표시한다.
    async fn like(&self, video_id: &str) -> Result<(), ApiError>;
}
