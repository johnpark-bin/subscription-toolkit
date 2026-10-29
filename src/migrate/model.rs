//! 이전 계획의 도메인 모델.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::youtube::{
    ChannelInfo, LikedVideo, Playlist, PlaylistItemRef, PlaylistSummary, Privacy, Subscription,
};

/// 이전할 데이터 유형.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, ValueEnum, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum DataType {
    /// 구독.
    Subscriptions,
    /// 플레이리스트와 항목.
    Playlists,
    /// 좋아요.
    Likes,
}

/// `stk push`가 받는 대상.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum PushKind {
    /// 구독만.
    Subscriptions,
    /// 플레이리스트만.
    Playlists,
    /// 좋아요만.
    Likes,
    /// 세 유형 모두(구독 → 플레이리스트 → 좋아요).
    All,
}

impl PushKind {
    /// 이 명령이 처리할 데이터 유형.
    pub fn data_types(self) -> Vec<DataType> {
        match self {
            Self::Subscriptions => vec![DataType::Subscriptions],
            Self::Playlists => vec![DataType::Playlists],
            Self::Likes => vec![DataType::Likes],
            Self::All => vec![
                DataType::Subscriptions,
                DataType::Playlists,
                DataType::Likes,
            ],
        }
    }
}

/// 원본 계정 스냅샷.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 스냅샷을 만든 계정.
    pub account: Account,
    /// 계정의 채널.
    pub channel: ChannelInfo,
    /// 수집 시각.
    pub fetched_at: DateTime<Utc>,
    /// 구독 목록.
    pub subscriptions: Vec<Subscription>,
    /// 시스템 플레이리스트를 제외한 소유 플레이리스트.
    pub playlists: Vec<Playlist>,
    /// 좋아요 표시한 영상(최근 1,000개까지).
    pub likes: Vec<LikedVideo>,
}

impl Snapshot {
    /// 스냅샷이 `ttl` 이내인지 확인한다.
    pub fn is_fresh(&self, now: DateTime<Utc>, ttl: chrono::Duration) -> bool {
        now.signed_duration_since(self.fetched_at) <= ttl
    }
}

/// 대상 계정의 현재 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetState {
    /// 대상 계정의 채널.
    pub channel: ChannelInfo,
    /// 이미 구독 중인 채널 ID.
    pub subscription_ids: HashSet<String>,
    /// 대상 계정의 플레이리스트 요약.
    pub playlist_summaries: Vec<PlaylistSummary>,
    /// 재사용 대상 플레이리스트 ID → 항목.
    pub playlist_items: HashMap<String, Vec<PlaylistItemRef>>,
    /// 이미 좋아요를 누른 영상 ID.
    pub liked_ids: HashSet<String>,
}

/// 이전 계획.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// 계획 생성 시각.
    pub generated_at: DateTime<Utc>,
    /// 원본 스냅샷.
    pub source: Snapshot,
    /// 대상 현재 상태.
    pub target: TargetState,
    /// 소스 플레이리스트 ID → 재사용할 대상 플레이리스트 ID.
    pub playlist_targets: HashMap<String, String>,
    /// 실행할 액션 목록(순서대로).
    pub actions: Vec<Action>,
}

/// 계획의 개별 액션.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    /// journal 키. 재개 시 완료 여부 판단에 쓴다.
    pub key: String,
    /// 액션 종류와 인자.
    pub kind: ActionKind,
}

/// 액션 종류.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionKind {
    /// 채널 구독.
    Subscribe {
        /// 구독할 채널 ID.
        channel_id: String,
        /// 채널 이름.
        title: String,
    },
    /// 플레이리스트 생성.
    CreatePlaylist {
        /// 원본 플레이리스트 ID(또는 `history`).
        source_playlist_id: String,
        /// 새 제목.
        title: String,
        /// 설명.
        description: String,
        /// 공개 범위.
        privacy: Privacy,
    },
    /// 플레이리스트 항목 추가.
    AddPlaylistItem {
        /// 원본 플레이리스트 ID(또는 `history`).
        source_playlist_id: String,
        /// 영상 ID.
        video_id: String,
        /// 영상 제목.
        title: String,
    },
    /// 좋아요 표시.
    Like {
        /// 영상 ID.
        video_id: String,
        /// 영상 제목.
        title: String,
    },
}

impl ActionKind {
    /// 액션이 속한 데이터 유형.
    pub fn data_type(&self) -> DataType {
        match self {
            Self::Subscribe { .. } => DataType::Subscriptions,
            Self::CreatePlaylist { .. } | Self::AddPlaylistItem { .. } => DataType::Playlists,
            Self::Like { .. } => DataType::Likes,
        }
    }
}

impl DataType {
    /// 한국어 이름.
    pub fn label(self) -> &'static str {
        match self {
            Self::Subscriptions => "구독",
            Self::Playlists => "플레이리스트",
            Self::Likes => "좋아요",
        }
    }
}
