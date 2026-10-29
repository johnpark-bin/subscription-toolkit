//! 시청기록 통계와 Markdown/JSON 리포트.

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Utc};
use serde::Serialize;
use serde_json::{Value, json};

use crate::history::parse::HistoryEntry;

/// 채널별 시청 수.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChannelCount {
    /// 채널 이름.
    pub name: String,
    /// 시청 수.
    pub count: usize,
}

/// 영상별 시청 수.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VideoCount {
    /// 영상 ID.
    pub video_id: String,
    /// 영상 제목.
    pub title: String,
    /// 시청 수.
    pub count: usize,
}

/// 월별 시청 수.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MonthCount {
    /// `YYYY-MM`.
    pub month: String,
    /// 시청 수.
    pub count: usize,
}

/// 시청기록 통계.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryReport {
    /// 전체 이벤트 수.
    pub total_events: usize,
    /// 고유 영상 수(광고 포함).
    pub unique_videos: usize,
    /// 고유 채널 수(광고 제외).
    pub unique_channels: usize,
    /// 영상 ID를 얻지 못한 이벤트 수.
    pub unparsed_events: usize,
    /// 광고 이벤트 수.
    pub ad_events: usize,
    /// 가장 이른 시청 시각.
    pub first_watched_at: Option<DateTime<Utc>>,
    /// 가장 늦은 시청 시각.
    pub last_watched_at: Option<DateTime<Utc>>,
    /// 상위 채널.
    pub top_channels: Vec<ChannelCount>,
    /// 상위 영상.
    pub top_videos: Vec<VideoCount>,
    /// 월별 시청 수(오름차순).
    pub monthly: Vec<MonthCount>,
}

/// 시청기록에서 통계를 만든다.
///
/// `top`은 상위 채널/영상 개수다. 0이면 목록을 비운다.
pub fn build_report(entries: &[HistoryEntry], top: usize) -> HistoryReport {
    let mut channel_counts: HashMap<&str, usize> = HashMap::new();
    let mut video_counts: HashMap<&str, (String, usize)> = HashMap::new();
    let mut month_counts: HashMap<String, usize> = HashMap::new();
    let mut unique_videos: HashMap<&str, ()> = HashMap::new();
    let mut unique_channels: HashMap<&str, ()> = HashMap::new();

    let mut first_watched_at: Option<DateTime<Utc>> = None;
    let mut last_watched_at: Option<DateTime<Utc>> = None;
    let mut unparsed_events = 0usize;
    let mut ad_events = 0usize;

    for entry in entries {
        if entry.is_ad {
            ad_events += 1;
        }
        match entry.video_id.as_deref() {
            Some(video_id) => {
                unique_videos.insert(video_id, ());
                let slot = video_counts
                    .entry(video_id)
                    .or_insert_with(|| (entry.title.clone(), 0));
                slot.1 += 1;
            }
            None => unparsed_events += 1,
        }

        if !entry.is_ad {
            if let Some(channel_id) = entry
                .channel_id
                .as_deref()
                .or(entry.channel_name.as_deref())
            {
                unique_channels.insert(channel_id, ());
            }
            if let Some(name) = entry.channel_name.as_deref() {
                *channel_counts.entry(name).or_insert(0) += 1;
            }
        }

        if let Some(watched_at) = entry.watched_at {
            *month_counts
                .entry(format!("{}-{:02}", watched_at.year(), watched_at.month()))
                .or_insert(0) += 1;
            first_watched_at = Some(match first_watched_at {
                Some(current) if current <= watched_at => current,
                _ => watched_at,
            });
            last_watched_at = Some(match last_watched_at {
                Some(current) if current >= watched_at => current,
                _ => watched_at,
            });
        }
    }

    let mut top_channels: Vec<ChannelCount> = channel_counts
        .into_iter()
        .map(|(name, count)| ChannelCount {
            name: name.to_string(),
            count,
        })
        .collect();
    top_channels.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.name.cmp(&right.name))
    });
    top_channels.truncate(top);

    let mut top_videos: Vec<VideoCount> = video_counts
        .into_iter()
        .map(|(video_id, (title, count))| VideoCount {
            video_id: video_id.to_string(),
            title,
            count,
        })
        .collect();
    top_videos.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.title.cmp(&right.title))
    });
    top_videos.truncate(top);

    let mut monthly: Vec<MonthCount> = month_counts
        .into_iter()
        .map(|(month, count)| MonthCount { month, count })
        .collect();
    monthly.sort_by(|left, right| left.month.cmp(&right.month));

    HistoryReport {
        total_events: entries.len(),
        unique_videos: unique_videos.len(),
        unique_channels: unique_channels.len(),
        unparsed_events,
        ad_events,
        first_watched_at,
        last_watched_at,
        top_channels,
        top_videos,
        monthly,
    }
}

fn format_time(value: Option<DateTime<Utc>>) -> String {
    value
        .map(|time| time.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "-".to_string())
}

impl HistoryReport {
    /// `stk history report --format json` 출력.
    pub fn to_json(&self) -> Value {
        json!({
            "total_events": self.total_events,
            "unique_videos": self.unique_videos,
            "unique_channels": self.unique_channels,
            "unparsed_events": self.unparsed_events,
            "ad_events": self.ad_events,
            "first_watched_at": self.first_watched_at,
            "last_watched_at": self.last_watched_at,
            "top_channels": self.top_channels,
            "top_videos": self.top_videos,
            "monthly": self.monthly,
        })
    }

    /// Markdown 리포트.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# YouTube 시청기록 분석\n\n");
        out.push_str(&format!("- 전체 이벤트: {}\n", self.total_events));
        out.push_str(&format!("- 고유 영상: {}\n", self.unique_videos));
        out.push_str(&format!("- 고유 채널: {}\n", self.unique_channels));
        out.push_str(&format!("- 광고 항목: {}\n", self.ad_events));
        out.push_str(&format!("- 파싱 실패: {}\n", self.unparsed_events));
        out.push_str(&format!(
            "- 기간: {} ~ {}\n",
            format_time(self.first_watched_at),
            format_time(self.last_watched_at)
        ));

        out.push_str("\n## 상위 채널\n\n");
        if self.top_channels.is_empty() {
            out.push_str("기록이 없습니다.\n");
        } else {
            out.push_str("| 채널 | 시청 수 |\n| --- | --- |\n");
            for channel in &self.top_channels {
                out.push_str(&format!("| {} | {} |\n", channel.name, channel.count));
            }
        }

        out.push_str("\n## 상위 영상\n\n");
        if self.top_videos.is_empty() {
            out.push_str("기록이 없습니다.\n");
        } else {
            out.push_str("| 영상 | 시청 수 |\n| --- | --- |\n");
            for video in &self.top_videos {
                out.push_str(&format!(
                    "| {} ({}) | {} |\n",
                    video.title, video.video_id, video.count
                ));
            }
        }

        out.push_str("\n## 월별 시청 수\n\n");
        if self.monthly.is_empty() {
            out.push_str("기록이 없습니다.\n");
        } else {
            out.push_str("| 월 | 시청 수 |\n| --- | --- |\n");
            for month in &self.monthly {
                out.push_str(&format!("| {} | {} |\n", month.month, month.count));
            }
        }

        out
    }
}
