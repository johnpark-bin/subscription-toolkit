//! 이전 계획 계산과 실행.

pub mod model;
pub mod plan;

use std::path::Path;

use chrono::{DateTime, Utc};

use crate::account::Account;
use crate::error::AppError;
use crate::paths::Paths;
use crate::youtube::YouTubeApi;

pub use model::{Action, ActionKind, DataType, Plan, PushKind, Snapshot, TargetState};
pub use plan::{
    DATA_RETENTION_DAYS, PLAYLIST_ITEM_LIMIT, QuotaEstimate, SNAPSHOT_TTL_HOURS, WRITE_QUOTA_UNIT,
    build_plan, display_titles, estimate_quota, fetch_snapshot, fetch_target_state, summary_json,
};

/// 데이터 보관 기한을 넘겼는지 확인한다.
pub fn is_expired(timestamp: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now.signed_duration_since(timestamp) > chrono::Duration::days(DATA_RETENTION_DAYS)
}

fn read_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    label: &str,
) -> Result<Option<T>, AppError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(AppError::io(
                format!("{label}을 읽을 수 없습니다: {}", path.display()),
                err,
            ));
        }
    };
    let value = serde_json::from_str(&raw).map_err(|err| {
        AppError::json(
            format!("{label}을 해석할 수 없습니다: {}", path.display()),
            err,
        )
    })?;
    Ok(Some(value))
}

/// 저장된 스냅샷을 읽는다. 30일이 지난 파일은 지우고 `None`을 돌려준다.
pub fn load_snapshot(paths: &Paths, now: DateTime<Utc>) -> Result<Option<Snapshot>, AppError> {
    let path = paths.snapshot();
    let Some(snapshot): Option<Snapshot> = read_json(&path, "스냅샷")? else {
        return Ok(None);
    };
    if is_expired(snapshot.fetched_at, now) {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(snapshot))
}

/// 스냅샷을 저장한다.
pub fn save_snapshot(paths: &Paths, snapshot: &Snapshot) -> Result<(), AppError> {
    let raw = serde_json::to_vec_pretty(snapshot)
        .map_err(|err| AppError::json("스냅샷을 직렬화할 수 없습니다", err))?;
    paths.write_atomic(&paths.snapshot(), &raw)
}

/// 24시간 이내 스냅샷을 재사용하고, 아니면 새로 수집한다.
pub async fn load_or_collect_snapshot(
    paths: &Paths,
    api: &dyn YouTubeApi,
    account: Account,
    now: DateTime<Utc>,
    refresh: bool,
) -> Result<Snapshot, AppError> {
    if !refresh
        && let Some(snapshot) = load_snapshot(paths, now)?
        && snapshot.is_fresh(now, chrono::Duration::hours(SNAPSHOT_TTL_HOURS))
    {
        return Ok(snapshot);
    }
    let snapshot = fetch_snapshot(api, account, now).await?;
    save_snapshot(paths, &snapshot)?;
    Ok(snapshot)
}

/// 저장된 계획을 읽는다. 30일이 지난 파일은 지우고 `None`을 돌려준다.
pub fn load_plan(paths: &Paths, now: DateTime<Utc>) -> Result<Option<Plan>, AppError> {
    let path = paths.plan();
    let Some(plan): Option<Plan> = read_json(&path, "계획")? else {
        return Ok(None);
    };
    if is_expired(plan.generated_at, now) {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    Ok(Some(plan))
}

/// 계획을 저장한다.
pub fn save_plan(paths: &Paths, plan: &Plan) -> Result<(), AppError> {
    let raw = serde_json::to_vec_pretty(plan)
        .map_err(|err| AppError::json("계획을 직렬화할 수 없습니다", err))?;
    paths.write_atomic(&paths.plan(), &raw)
}

/// 스냅샷이 24시간 이내인지 확인한다.
pub fn is_snapshot_fresh(snapshot: &Snapshot, now: DateTime<Utc>) -> bool {
    snapshot.is_fresh(now, chrono::Duration::hours(SNAPSHOT_TTL_HOURS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::youtube::ChannelInfo;
    use std::collections::{HashMap, HashSet};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T12:00:00Z")
            .expect("시각 파싱")
            .with_timezone(&Utc)
    }

    fn snapshot(fetched_at: DateTime<Utc>) -> Snapshot {
        Snapshot {
            account: Account::Source,
            channel: ChannelInfo {
                id: "UC_SRC".to_string(),
                title: "원본 채널".to_string(),
            },
            fetched_at,
            subscriptions: Vec::new(),
            playlists: Vec::new(),
            likes: Vec::new(),
        }
    }

    #[test]
    fn load_snapshot_drops_expired_file() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        save_snapshot(&paths, &snapshot(now() - chrono::Duration::days(31))).expect("저장");

        assert!(load_snapshot(&paths, now()).expect("로드").is_none());
        assert!(
            !paths.snapshot().exists(),
            "30일 지난 스냅샷은 삭제해야 한다"
        );
    }

    #[test]
    fn plan_round_trip() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let plan = Plan {
            generated_at: now(),
            source: snapshot(now()),
            target: TargetState {
                channel: ChannelInfo {
                    id: "UC_DST".to_string(),
                    title: "대상 채널".to_string(),
                },
                subscription_ids: HashSet::new(),
                playlist_summaries: Vec::new(),
                playlist_items: HashMap::new(),
                liked_ids: HashSet::new(),
            },
            playlist_targets: HashMap::new(),
            actions: Vec::new(),
        };
        save_plan(&paths, &plan).expect("저장");
        let loaded = load_plan(&paths, now())
            .expect("로드")
            .expect("계획이 있어야 한다");
        assert_eq!(loaded, plan);
        assert!(is_snapshot_fresh(&loaded.source, now()));
        assert!(!is_snapshot_fresh(
            &loaded.source,
            now() + chrono::Duration::hours(25)
        ));
    }
}
