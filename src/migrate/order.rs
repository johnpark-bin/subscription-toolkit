//! 플레이리스트 항목 순서 검증과 보정.

use std::collections::{HashMap, HashSet};

use crate::error::AppError;
use crate::migrate::progress::ProgressReporter;
use crate::migrate::quota::QuotaLedger;
use crate::youtube::{ApiError, YouTubeApi};

/// 순서 검증 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderReport {
    /// 대상 플레이리스트 ID.
    pub playlist_id: String,
    /// 위치를 고친 항목 수.
    pub repaired: usize,
    /// 보정을 중단한 이유(있으면 경고로 출력한다).
    pub warning: Option<String>,
}

/// 대상 플레이리스트의 항목 순서를 기대 순서와 비교하고, 다르면 위치를 고친다.
///
/// 대상에 실제로 존재하는 기대 영상만 순서대로 비교한다(중간에 실패한 항목은 건너뛴다).
/// 위치를 바꾸는 요청은 항목당 `write_cost`만큼 예산을 쓴다.
pub async fn verify_and_repair(
    api: &dyn YouTubeApi,
    playlist_id: &str,
    expected_video_ids: &[String],
    quota: &mut QuotaLedger,
    progress: &ProgressReporter,
    write_cost: u64,
) -> Result<OrderReport, AppError> {
    let items = api.list_playlist_items(playlist_id).await?;

    let by_video: HashMap<&str, (u64, String)> = items
        .iter()
        .map(|item| {
            (
                item.video_id.as_str(),
                (item.position, item.item_id.clone()),
            )
        })
        .collect();

    let mut desired: Vec<(&str, u64, String)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for video_id in expected_video_ids {
        if !seen.insert(video_id.as_str()) {
            continue;
        }
        if let Some((position, item_id)) = by_video.get(video_id.as_str()) {
            desired.push((video_id.as_str(), *position, item_id.clone()));
        }
    }

    let desired_ids: HashSet<&str> = desired.iter().map(|(video_id, _, _)| *video_id).collect();
    let actual: Vec<&str> = items
        .iter()
        .filter(|item| desired_ids.contains(item.video_id.as_str()))
        .map(|item| item.video_id.as_str())
        .collect();
    let wanted: Vec<&str> = desired.iter().map(|(video_id, _, _)| *video_id).collect();

    if actual == wanted {
        return Ok(OrderReport {
            playlist_id: playlist_id.to_string(),
            repaired: 0,
            warning: None,
        });
    }

    let mut repaired = 0usize;
    for (index, (video_id, position, item_id)) in desired.iter().enumerate() {
        let index = index as u64;
        if *position == index {
            continue;
        }
        if let Err(error) = quota.reserve(write_cost.max(1)) {
            return Ok(OrderReport {
                playlist_id: playlist_id.to_string(),
                repaired,
                warning: Some(format!(
                    "쿼터 예산이 부족해 순서 보정을 중단했습니다: {error}"
                )),
            });
        }
        match api
            .update_playlist_item_position(playlist_id, item_id, video_id, index)
            .await
        {
            Ok(()) => repaired += 1,
            Err(ApiError::ManualSortRequired) => {
                return Ok(OrderReport {
                    playlist_id: playlist_id.to_string(),
                    repaired,
                    warning: Some(
                        "이 플레이리스트는 수동 정렬이 필요합니다. YouTube 스튜디오에서 항목 순서를 직접 바꿔 주세요."
                            .to_string(),
                    ),
                });
            }
            Err(error) => return Err(AppError::Api(error)),
        }
    }

    if repaired > 0 {
        progress.message(&format!(
            "플레이리스트 {playlist_id} 순서를 {repaired}건 보정했습니다"
        ));
    }
    Ok(OrderReport {
        playlist_id: playlist_id.to_string(),
        repaired,
        warning: None,
    })
}
