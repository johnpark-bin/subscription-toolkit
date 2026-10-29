//! 액션 실행, 재개, 쿼터 예산 집행.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::error::AppError;
use crate::migrate::WRITE_QUOTA_UNIT;
use crate::migrate::journal::{Journal, JournalStatus};
use crate::migrate::model::{Action, ActionKind, DataType};
use crate::migrate::order::{self, OrderReport};
use crate::migrate::progress::{ProgressReporter, TypeProgress};
use crate::migrate::quota::QuotaLedger;
use crate::youtube::{ApiError, YouTubeApi};

/// 실행 옵션.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOptions {
    /// 데이터 유형별 최대 액션 수.
    pub limit: Option<usize>,
    /// 쓰기 요청 사이 지연.
    pub delay: Duration,
    /// 플레이리스트 순서 검증/보정 여부.
    pub verify_order: bool,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            limit: None,
            delay: Duration::from_millis(200),
            verify_order: true,
        }
    }
}

/// 데이터 유형별 실행 결과.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TypeSummary {
    /// 성공 수.
    pub success: u64,
    /// 건너뜀 수.
    pub skipped: u64,
    /// 실패 수.
    pub failed: u64,
    /// `--limit`에 도달했는지 여부.
    pub limit_reached: bool,
}

/// 실행 요약.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    /// 데이터 유형별 결과.
    pub types: BTreeMap<DataType, TypeSummary>,
    /// 이번 실행에서 사용한 쿼터(units).
    pub used_quota: u64,
    /// 순서 검증/보정 결과.
    pub orders: Vec<OrderReport>,
}

impl RunSummary {
    /// 전체 성공 수.
    pub fn success_total(&self) -> u64 {
        self.types.values().map(|item| item.success).sum()
    }

    /// 전체 건너뜀 수.
    pub fn skipped_total(&self) -> u64 {
        self.types.values().map(|item| item.skipped).sum()
    }

    /// 전체 실패 수.
    pub fn failed_total(&self) -> u64 {
        self.types.values().map(|item| item.failed).sum()
    }

    /// `--limit`에 도달한 유형이 있는지 확인한다.
    pub fn limit_reached(&self) -> bool {
        self.types.values().any(|item| item.limit_reached)
    }

    /// 사람이 읽는 요약 문자열(줄바꿈 포함).
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (data_type, summary) in &self.types {
            let mut line = format!(
                "{}: 성공 {} · 건너뜀 {}",
                data_type.label(),
                summary.success,
                summary.skipped
            );
            if summary.failed > 0 {
                line.push_str(&format!(" · 실패 {}", summary.failed));
            }
            if summary.limit_reached {
                line.push_str(" · --limit 도달");
            }
            out.push_str(&line);
            out.push('\n');
        }
        out.push_str(&format!("사용 쿼터: {} units\n", self.used_quota));
        for report in &self.orders {
            if let Some(warning) = &report.warning {
                out.push_str(&format!("경고: {warning}\n"));
            }
        }
        out
    }
}

/// 실행기가 의존하는 객체 묶음.
pub struct ExecutorDeps<'a> {
    /// 대상 계정 API.
    pub api: &'a dyn YouTubeApi,
    /// 실행 journal.
    pub journal: &'a mut Journal,
    /// 쿼터 장부.
    pub quota: &'a mut QuotaLedger,
    /// 진행률 출력기.
    pub progress: &'a ProgressReporter,
    /// 실행 옵션.
    pub opts: ExecOptions,
    /// 소스 플레이리스트 ID → 대상 플레이리스트 ID.
    pub targets: HashMap<String, String>,
    /// 소스 플레이리스트 ID → 기대 영상 순서.
    pub expected_orders: HashMap<String, Vec<String>>,
}

/// 액션을 순서대로 실행하는 실행기.
pub struct Executor<'a> {
    deps: ExecutorDeps<'a>,
}

enum Outcome {
    Done(Option<Value>),
    Full,
}

enum StepError {
    Api(ApiError),
    Internal(String),
}

enum Failure {
    Skip(String),
    Failed(String),
    Abort(AppError),
}

impl<'a> Executor<'a> {
    /// 의존성을 받아 실행기를 만든다.
    pub fn new(deps: ExecutorDeps<'a>) -> Self {
        Self { deps }
    }

    /// 액션 목록을 실행한다.
    ///
    /// 완료된 키는 journal을 근거로 건너뛰고, 쿼터 예산을 사전 예약한다. 실패·인증·쿼터
    /// 오류는 실행을 중단하며, 그때까지의 성공은 journal에 남아 다음 실행에서 이어진다.
    pub async fn run(
        &mut self,
        actions: &[Action],
        now: DateTime<Utc>,
    ) -> Result<RunSummary, AppError> {
        let mut summary = RunSummary::default();

        for (source, target) in self.deps.journal.created_playlists() {
            self.deps
                .targets
                .entry(source.clone())
                .or_insert_with(|| target.clone());
        }
        let mut created: HashSet<String> = self
            .deps
            .journal
            .created_playlists()
            .keys()
            .cloned()
            .collect();
        let mut incomplete: HashSet<String> = HashSet::new();

        let mut totals: BTreeMap<DataType, u64> = BTreeMap::new();
        for action in actions {
            *totals.entry(action.kind.data_type()).or_default() += 1;
        }

        let mut bars: BTreeMap<DataType, TypeProgress> = BTreeMap::new();
        let mut attempted: BTreeMap<DataType, u64> = BTreeMap::new();
        let mut full_playlists: HashSet<String> = HashSet::new();
        let mut first_write = true;

        for action in actions {
            let data_type = action.kind.data_type();
            let entry = summary.types.entry(data_type).or_default();

            if self.deps.journal.is_complete(&action.key) {
                entry.skipped += 1;
                continue;
            }

            let attempt_count = attempted.entry(data_type).or_insert(0);
            if let Some(limit) = self.deps.opts.limit
                && *attempt_count >= limit as u64
            {
                entry.limit_reached = true;
                if let ActionKind::AddPlaylistItem {
                    source_playlist_id, ..
                } = &action.kind
                {
                    incomplete.insert(source_playlist_id.clone());
                }
                continue;
            }

            if let ActionKind::AddPlaylistItem {
                source_playlist_id, ..
            } = &action.kind
                && full_playlists.contains(source_playlist_id)
            {
                self.record(
                    &action.key,
                    JournalStatus::Skipped,
                    Some(Value::String("playlistFull".to_string())),
                    now,
                )?;
                entry.skipped += 1;
                continue;
            }

            if !first_write && !self.deps.opts.delay.is_zero() {
                tokio::time::sleep(self.deps.opts.delay).await;
            }
            first_write = false;

            if let std::collections::btree_map::Entry::Vacant(slot) = bars.entry(data_type) {
                let total = totals.get(&data_type).copied().unwrap_or(0);
                slot.insert(self.deps.progress.start_type(data_type, total));
            }
            let bar = &bars[&data_type];

            self.deps.quota.reserve(WRITE_QUOTA_UNIT)?;
            *attempt_count += 1;

            match self.execute_action(action).await {
                Ok(Outcome::Done(detail)) => {
                    if let ActionKind::CreatePlaylist {
                        source_playlist_id, ..
                    } = &action.kind
                    {
                        created.insert(source_playlist_id.clone());
                    }
                    self.record(&action.key, JournalStatus::Success, detail, now)?;
                    entry.success += 1;
                    bar.advance(true);
                }
                Ok(Outcome::Full) => {
                    if let ActionKind::AddPlaylistItem {
                        source_playlist_id, ..
                    } = &action.kind
                    {
                        full_playlists.insert(source_playlist_id.clone());
                        incomplete.insert(source_playlist_id.clone());
                    }
                    self.record(
                        &action.key,
                        JournalStatus::Skipped,
                        Some(Value::String("playlistFull".to_string())),
                        now,
                    )?;
                    entry.skipped += 1;
                    bar.advance(false);
                }
                Err(error) => match classify(error, &action.kind) {
                    Failure::Skip(reason) => {
                        self.record(
                            &action.key,
                            JournalStatus::Skipped,
                            Some(Value::String(reason)),
                            now,
                        )?;
                        entry.skipped += 1;
                        bar.advance(false);
                    }
                    Failure::Failed(message) => {
                        self.record(
                            &action.key,
                            JournalStatus::Failed,
                            Some(Value::String(message.clone())),
                            now,
                        )?;
                        entry.failed += 1;
                        finish_bars(&bars);
                        return Err(AppError::Config(format!(
                            "{} 처리에 실패했습니다: {message}",
                            action.key
                        )));
                    }
                    Failure::Abort(app_error) => {
                        finish_bars(&bars);
                        return Err(app_error);
                    }
                },
            }
        }

        finish_bars(&bars);

        if self.deps.opts.verify_order {
            let mut sources: Vec<String> = created.iter().cloned().collect();
            sources.sort();
            for source in sources {
                if incomplete.contains(&source) {
                    continue;
                }
                let Some(target) = self.deps.targets.get(&source).cloned() else {
                    continue;
                };
                let Some(expected) = self.deps.expected_orders.get(&source).cloned() else {
                    continue;
                };
                let report = order::verify_and_repair(
                    self.deps.api,
                    &target,
                    &expected,
                    self.deps.quota,
                    self.deps.progress,
                )
                .await?;
                if let Some(warning) = &report.warning {
                    self.deps.progress.message(warning);
                }
                summary.orders.push(report);
            }
        }

        summary.used_quota = self.deps.quota.used();
        Ok(summary)
    }

    fn record(
        &mut self,
        key: &str,
        status: JournalStatus,
        detail: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.deps.journal.record(key, status, detail, now)
    }

    async fn execute_action(&mut self, action: &Action) -> Result<Outcome, StepError> {
        match &action.kind {
            ActionKind::Subscribe { channel_id, .. } => {
                self.deps
                    .api
                    .subscribe(channel_id)
                    .await
                    .map_err(StepError::Api)?;
                Ok(Outcome::Done(None))
            }
            ActionKind::CreatePlaylist {
                source_playlist_id,
                title,
                description,
                privacy,
            } => {
                let target = self
                    .deps
                    .api
                    .create_playlist(title, description, *privacy)
                    .await
                    .map_err(StepError::Api)?;
                self.deps
                    .targets
                    .insert(source_playlist_id.clone(), target.clone());
                Ok(Outcome::Done(Some(json!({"target_playlist_id": target}))))
            }
            ActionKind::AddPlaylistItem {
                source_playlist_id,
                video_id,
                ..
            } => {
                let Some(target) = self.deps.targets.get(source_playlist_id).cloned() else {
                    return Err(StepError::Internal(format!(
                        "재사용할 플레이리스트를 찾을 수 없습니다: {source_playlist_id}"
                    )));
                };
                match self.deps.api.add_playlist_item(&target, video_id).await {
                    Ok(()) => Ok(Outcome::Done(None)),
                    Err(ApiError::PlaylistFull) => Ok(Outcome::Full),
                    Err(error) => Err(StepError::Api(error)),
                }
            }
            ActionKind::Like { video_id, .. } => {
                self.deps.api.like(video_id).await.map_err(StepError::Api)?;
                Ok(Outcome::Done(None))
            }
        }
    }
}

fn finish_bars(bars: &BTreeMap<DataType, TypeProgress>) {
    for bar in bars.values() {
        bar.finish();
    }
}

fn classify(error: StepError, kind: &ActionKind) -> Failure {
    let error = match error {
        StepError::Internal(message) => return Failure::Failed(message),
        StepError::Api(error) => error,
    };

    match &error {
        ApiError::Auth(_) => {
            return Failure::Abort(AppError::Auth(format!("인증 오류: {error}")));
        }
        ApiError::QuotaExceeded => {
            return Failure::Abort(AppError::Quota(
                "일일 쿼터를 초과했습니다. 태평양 시간 자정 이후에 다시 실행하세요.".to_string(),
            ));
        }
        ApiError::RetryLater(_) => {
            return Failure::Abort(AppError::Partial(format!(
                "{error} — 몇 시간 후에 다시 실행해 주세요."
            )));
        }
        ApiError::RateLimited(_) | ApiError::Network(_) => {
            return Failure::Failed(format!("{error}"));
        }
        ApiError::Http { status, .. } if *status >= 500 => {
            return Failure::Failed(format!("{error}"));
        }
        ApiError::Forbidden(_) if error.is_subscription_limit() => {
            return Failure::Abort(AppError::Partial(format!(
                "구독 한도에 도달했습니다({error}). 대상 계정의 구독을 정리한 뒤 다시 실행하세요."
            )));
        }
        _ => {}
    }

    match (&error, kind) {
        (ApiError::AlreadyExists(_), ActionKind::Subscribe { .. }) => {
            Failure::Skip("subscriptionDuplicate".to_string())
        }
        (ApiError::NotFound(_), ActionKind::Subscribe { .. }) => {
            Failure::Skip("publisherNotFound".to_string())
        }
        (ApiError::Forbidden(_), ActionKind::Subscribe { .. }) => {
            Failure::Skip("subscriptionForbidden".to_string())
        }
        (ApiError::NotFound(_), ActionKind::AddPlaylistItem { .. }) => {
            Failure::Skip("videoNotFound".to_string())
        }
        (ApiError::NotFound(_), ActionKind::Like { .. }) => {
            Failure::Skip("videoNotFound".to_string())
        }
        (ApiError::Forbidden(_), ActionKind::Like { .. }) => {
            Failure::Skip("videoRatingDisabled".to_string())
        }
        (ApiError::Unsupported(_), ActionKind::AddPlaylistItem { .. }) => {
            Failure::Skip("playlistOperationUnsupported".to_string())
        }
        _ => Failure::Failed(format!("{error}")),
    }
}
