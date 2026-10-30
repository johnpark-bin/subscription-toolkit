//! 브라우저 세션으로 계획 액션을 실행한다.
//!
//! OAuth API 경로와 별개다. 계획·journal만 공유하고, 실제 요청은 사용자가 로그인한
//! Chrome 페이지에서 실행된다.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use crate::browser::cdp::{CdpClient, CdpError, PageResponse};
use crate::browser::jitter::Jitter;
use crate::browser::page;
use crate::error::AppError;
use crate::migrate::journal::{Journal, JournalStatus};
use crate::migrate::model::{Action, ActionKind, DataType};

/// 브라우저 실행 옵션.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSyncOptions {
    /// 액션 사이 최소 대기 시간.
    pub delay: Duration,
    /// 최소 대기에 더할 무작위 폭(0~jitter).
    pub jitter: Duration,
    /// 재시도 기본 대기(2배씩 증가).
    pub retry_base: Duration,
    /// 데이터 유형별 최대 액션 수.
    pub limit: Option<usize>,
}

impl Default for BrowserSyncOptions {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(3_000),
            jitter: Duration::from_millis(1_500),
            retry_base: Duration::from_secs(5),
            limit: None,
        }
    }
}

/// 실행 결과.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserSyncReport {
    /// 성공 수.
    pub success: u64,
    /// 건너뜀 수.
    pub skipped: u64,
    /// 이미 완료되어 건너뛴 수.
    pub already_done: u64,
    /// (소스 플레이리스트 ID, 새 대상 플레이리스트 ID) 목록.
    pub target_playlists: Vec<(String, String)>,
}

impl BrowserSyncReport {
    /// 사람이 읽는 요약.
    pub fn render(&self) -> String {
        let mut out = format!(
            "브라우저 실행: 성공 {} · 건너뜀 {} · 이미 완료 {}\n",
            self.success, self.skipped, self.already_done
        );
        for (source, target) in &self.target_playlists {
            out.push_str(&format!("  새 플레이리스트: {source} → {target}\n"));
        }
        out
    }
}

fn cdp_error(error: CdpError) -> AppError {
    AppError::Config(format!("{error} — {}", error.hint()))
}

/// 현재 브라우저 세션의 채널을 확인한다.
///
/// `expected_channel_id`가 비어 있지 않으면 일치해야 한다.
pub async fn verify_account(
    cdp: &mut CdpClient,
    expected_channel_id: &str,
) -> Result<Value, AppError> {
    // 일반 YouTube 페이지의 ytcfg에는 CHANNEL_ID가 없을 수 있다. Studio는 채널 단위로
    // 동작하므로 현재 세션의 채널 ID를 확실히 알려준다.
    cdp.navigate(page::STUDIO_URL).await.map_err(cdp_error)?;
    let account = cdp.evaluate(&page::account_js()).await.map_err(cdp_error)?;

    let logged_in = account["logged_in"].as_bool().unwrap_or(false);
    let channel = account["channel_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if !logged_in {
        return Err(AppError::Auth(
            "브라우저에 YouTube 로그인이 필요합니다. Chrome에서 로그인한 뒤 다시 실행하세요."
                .to_string(),
        ));
    }
    if channel.is_empty() {
        return Err(AppError::Auth(
            "브라우저 세션의 채널을 확인할 수 없습니다. YouTube Studio에서 채널을 선택한 뒤 다시 실행하세요."
                .to_string(),
        ));
    }
    if !expected_channel_id.is_empty() && channel != expected_channel_id {
        return Err(AppError::Auth(format!(
            "브라우저 세션의 채널이 다릅니다(현재 {channel}, 기대 {expected_channel_id}). 올바른 계정으로 로그인한 Chrome 포트인지 확인하세요."
        )));
    }
    // 요청은 www.youtube.com 오리진에서 보내야 CORS를 통과한다.
    cdp.navigate(page::YOUTUBE_URL).await.map_err(cdp_error)?;
    Ok(account)
}

async fn send(
    cdp: &mut CdpClient,
    endpoint: &str,
    body: &Value,
    opts: &BrowserSyncOptions,
) -> Result<PageResponse, AppError> {
    let expression = page::request_js(endpoint, body);
    let mut attempt = 0u32;
    loop {
        let value = cdp.evaluate(&expression).await.map_err(cdp_error)?;
        let response = page::response_from(value).map_err(cdp_error)?;
        let retryable = response.is_rate_limited() || response.is_server_error();
        if !retryable || attempt >= 3 {
            return Ok(response);
        }
        tokio::time::sleep(opts.retry_base * 2u32.pow(attempt)).await;
        attempt += 1;
    }
}

fn failure(key: &str, response: &PageResponse) -> AppError {
    if response.is_auth_problem() {
        AppError::Auth(format!(
            "{key}: 브라우저 세션이 만료되었거나 권한이 없습니다(HTTP {}). Chrome에서 다시 로그인하세요.",
            response.status
        ))
    } else if response.is_rate_limited() || response.is_server_error() {
        AppError::Partial(format!(
            "{key}: 요청이 제한되었습니다(HTTP {}). 잠시 후 다시 실행하세요.",
            response.status
        ))
    } else if !response.error.is_empty() {
        AppError::Config(format!("{key}: {}", response.error))
    } else {
        AppError::Config(format!(
            "{key}: HTTP {} — {}",
            response.status, response.body
        ))
    }
}

/// 계획 액션을 브라우저 세션으로 실행한다.
pub async fn sync_actions(
    cdp: &mut CdpClient,
    actions: &[Action],
    journal: &mut Journal,
    targets: &mut HashMap<String, String>,
    opts: &BrowserSyncOptions,
    now: DateTime<Utc>,
) -> Result<BrowserSyncReport, AppError> {
    let mut report = BrowserSyncReport::default();
    let mut attempted: HashMap<DataType, u64> = HashMap::new();
    let mut pacing = Jitter::from_clock();
    let mut first = true;

    for action in actions {
        if journal.is_complete(&action.key) {
            report.already_done += 1;
            continue;
        }

        let data_type = action.kind.data_type();
        let count = attempted.entry(data_type).or_insert(0);
        if let Some(limit) = opts.limit
            && *count >= limit as u64
        {
            continue;
        }

        if !first {
            let delay = opts.delay + pacing.duration(opts.jitter);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        first = false;
        *count += 1;

        match &action.kind {
            ActionKind::Subscribe { channel_id, .. } => {
                let response = send(
                    cdp,
                    page::SUBSCRIBE_ENDPOINT,
                    &page::subscribe_body(channel_id),
                    opts,
                )
                .await?;
                if response.ok {
                    journal.record(&action.key, JournalStatus::Success, None, now)?;
                    report.success += 1;
                } else if response.body.contains("already") {
                    journal.record(
                        &action.key,
                        JournalStatus::Skipped,
                        Some(Value::String("subscriptionDuplicate".to_string())),
                        now,
                    )?;
                    report.skipped += 1;
                } else {
                    return Err(failure(&action.key, &response));
                }
            }
            ActionKind::CreatePlaylist {
                source_playlist_id,
                title,
                description,
                privacy,
            } => {
                let response = send(
                    cdp,
                    page::CREATE_PLAYLIST_ENDPOINT,
                    &page::create_playlist_body(title, description, privacy.api_status()),
                    opts,
                )
                .await?;
                if !response.ok {
                    return Err(failure(&action.key, &response));
                }
                let playlist_id = response
                    .json()
                    .and_then(|value| {
                        value["playlistId"]
                            .as_str()
                            .map(str::to_string)
                            .or_else(|| value["playlistId"].as_u64().map(|id| id.to_string()))
                    })
                    .ok_or_else(|| {
                        AppError::Config(format!(
                            "{}: 응답에 playlistId가 없습니다 — {}",
                            action.key, response.body
                        ))
                    })?;
                targets.insert(source_playlist_id.clone(), playlist_id.clone());
                journal.record(
                    &action.key,
                    JournalStatus::Success,
                    Some(json!({"target_playlist_id": playlist_id})),
                    now,
                )?;
                report
                    .target_playlists
                    .push((source_playlist_id.clone(), playlist_id));
                report.success += 1;
            }
            ActionKind::AddPlaylistItem {
                source_playlist_id,
                video_id,
                ..
            } => {
                let target = targets.get(source_playlist_id).cloned().ok_or_else(|| {
                    AppError::Config(format!(
                        "{}: 대상 플레이리스트 ID를 찾을 수 없습니다. 계획을 다시 만드세요.",
                        action.key
                    ))
                })?;
                let response = send(
                    cdp,
                    page::EDIT_PLAYLIST_ENDPOINT,
                    &page::add_item_body(&target, video_id),
                    opts,
                )
                .await?;
                if response.ok && !response.body.contains("STATUS_FAILED") {
                    journal.record(&action.key, JournalStatus::Success, None, now)?;
                    report.success += 1;
                } else if response.body.contains("already") {
                    journal.record(
                        &action.key,
                        JournalStatus::Skipped,
                        Some(Value::String("alreadyInPlaylist".to_string())),
                        now,
                    )?;
                    report.skipped += 1;
                } else {
                    return Err(failure(&action.key, &response));
                }
            }
            ActionKind::Like { video_id, .. } => {
                let response =
                    send(cdp, page::LIKE_ENDPOINT, &page::like_body(video_id), opts).await?;
                if response.ok {
                    journal.record(&action.key, JournalStatus::Success, None, now)?;
                    report.success += 1;
                } else {
                    return Err(failure(&action.key, &response));
                }
            }
        }
    }

    Ok(report)
}
