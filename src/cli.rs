//! 명령 정의와 라우팅.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::Value;

use crate::account::Account;
use crate::auth::{self, FileTokenStore, LoginOptions, LoginOutcome};
use crate::browser::cdp::CdpClient;
use crate::browser::reconcile::{diff_missing, read_liked_videos, read_subscriptions, reconcile};
use crate::browser::sync::{BrowserSyncOptions, sync_actions, verify_account};
use crate::browser::watch_later::{
    WatchLaterFile, plan_watch_later, read_watch_later, sync_watch_later,
};
use crate::config::OAuthConfig;
use crate::error::AppError;
use crate::history::{self, HistoryFile};
use crate::migrate::executor::{ExecOptions, Executor, ExecutorDeps};
use crate::migrate::journal::Journal;
use crate::migrate::model::{Action, DataType, PushKind};
use crate::migrate::progress::ProgressReporter;
use crate::migrate::quota::{QuotaLedger, QuotaSnapshot};
use crate::migrate::status::build_status;
use crate::migrate::{
    WRITE_QUOTA_UNIT, build_plan, estimate_quota, fetch_snapshot, fetch_target_state,
    is_snapshot_fresh, load_or_collect_snapshot, load_plan, save_plan, save_snapshot, summary_json,
};
use crate::paths::{DataLock, Paths};
use crate::youtube::YouTubeApi;
use crate::youtube::client::{ChannelProbe, HttpYouTube, base_from_env};

/// 기본 쿼터 예산(units). 읽기용 500 units를 남겨 둔다.
pub const DEFAULT_BUDGET: u64 = 9_500;

/// 기본 쓰기 간격(ms).
pub const DEFAULT_DELAY_MS: u64 = 200;

/// `stk` 명령줄.
#[derive(Debug, Parser)]
#[command(
    name = "stk",
    version,
    about = "구독 계정 관리 CLI",
    long_about = "YouTube 계정의 구독·플레이리스트·좋아요를 다른 계정으로 복사하고, Takeout 시청기록을 분석합니다. 원본 계정의 데이터는 삭제하지 않습니다.",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    propagate_version = true,
    help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
)]
pub struct Cli {
    /// 디버그 로그를 출력합니다.
    #[arg(long, global = true)]
    pub verbose: bool,
    /// 경고 이상만 출력합니다.
    #[arg(long, global = true)]
    pub quiet: bool,
    /// 쓰기 요청 하나의 쿼터 단가(기본: 장부 값, 없으면 50).
    #[arg(long, global = true)]
    pub write_cost: Option<u64>,
    /// 도움말을 출력합니다.
    #[arg(long, short = 'h', global = true, action = clap::ArgAction::Help)]
    pub help: Option<bool>,
    /// 버전을 출력합니다.
    #[arg(long, short = 'V', global = true, action = clap::ArgAction::Version)]
    pub version: Option<bool>,
    /// 실행할 명령.
    #[command(subcommand)]
    pub command: Command,
}

/// 최상위 명령.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Google 계정 인증을 관리합니다.
    #[command(
        help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
    )]
    Auth {
        /// 인증 하위 명령.
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// 이전 계획을 계산합니다(쓰기 없음).
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Plan {
        /// 계산할 데이터 유형(쉼표 구분).
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            default_value = "subscriptions,playlists,likes"
        )]
        types: Vec<DataType>,
        /// 24시간 캐시를 무시하고 원본을 다시 읽습니다.
        #[arg(long)]
        refresh: bool,
        /// 요약을 JSON으로 출력합니다.
        #[arg(long)]
        json: bool,
    },
    /// 계획을 실행합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Push {
        /// 실행할 데이터 유형: subscriptions, playlists, likes, all.
        #[arg(value_enum, hide_possible_values = true)]
        kind: PushKind,
        /// 데이터 유형별 최대 액션 수.
        #[arg(long)]
        limit: Option<usize>,
        /// 오늘 사용할 쿼터 예산(units). 기본: 장부 값 또는 9500.
        #[arg(long)]
        budget: Option<u64>,
        /// 쓰기 요청 사이 지연(ms).
        #[arg(long, default_value_t = DEFAULT_DELAY_MS)]
        delay_ms: u64,
        /// 확인 없이 실행합니다.
        #[arg(long)]
        yes: bool,
    },
    /// Takeout 시청기록을 다룹니다.
    #[command(
        help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
    )]
    History {
        /// 시청기록 하위 명령.
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// 브라우저 세션(CDP)으로 동기화를 실행합니다.
    #[command(
        help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
    )]
    Browser {
        /// 브라우저 하위 명령.
        #[command(subcommand)]
        command: BrowserCommand,
    },
    /// 계획·확인·실행을 한 번에 수행합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Migrate {
        /// 데이터 유형별 최대 액션 수.
        #[arg(long)]
        limit: Option<usize>,
        /// 오늘 사용할 쿼터 예산(units). 기본: 장부 값 또는 9500.
        #[arg(long)]
        budget: Option<u64>,
        /// 쓰기 요청 사이 지연(ms).
        #[arg(long, default_value_t = DEFAULT_DELAY_MS)]
        delay_ms: u64,
        /// 원본 스냅샷 캐시를 무시하고 다시 읽습니다.
        #[arg(long)]
        refresh: bool,
        /// 확인 없이 실행합니다.
        #[arg(long)]
        yes: bool,
    },
    /// 이전 진행률과 남은 일수를 보여줍니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Status {
        /// JSON으로 출력합니다.
        #[arg(long)]
        json: bool,
    },
    /// 쿼터 장부를 조회하거나 콘솔 실측값으로 맞춥니다.
    #[command(
        help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
    )]
    Quota {
        /// 쿼터 하위 명령.
        #[command(subcommand)]
        command: QuotaCommand,
    },
    /// 저장한 API 데이터를 삭제합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Purge {
        /// 토큰 파일도 삭제합니다.
        #[arg(long)]
        tokens: bool,
        /// 확인 없이 삭제합니다.
        #[arg(long)]
        yes: bool,
    },
}

/// 인증 하위 명령.
#[derive(Debug, Subcommand)]
#[command(
    help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
)]
pub enum AuthCommand {
    /// 계정으로 로그인합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Login {
        /// 로그인할 계정: source(원본) 또는 target(대상).
        #[arg(long, value_enum, hide_possible_values = true)]
        account: Account,
        /// OAuth 클라이언트 설정 파일 경로.
        #[arg(long)]
        client_secret: Option<PathBuf>,
        /// 로그인 화면에서 미리 채울 이메일.
        #[arg(long)]
        login_hint: Option<String>,
        /// 브라우저를 열지 않고 주소만 출력합니다.
        #[arg(long)]
        no_browser: bool,
    },
    /// 인증 상태를 출력합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Status {
        /// JSON으로 출력합니다.
        #[arg(long)]
        json: bool,
    },
    /// 저장된 토큰을 삭제합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Logout {
        /// 로그아웃할 계정: source(원본) 또는 target(대상).
        #[arg(long, value_enum, hide_possible_values = true)]
        account: Account,
    },
}

/// 브라우저 하위 명령.
#[derive(Debug, Subcommand)]
#[command(
    help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
)]
pub enum BrowserCommand {
    /// DevTools에 연결해 로그인한 채널을 확인합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Check {
        /// Chrome DevTools 포트.
        #[arg(long, default_value_t = 9222)]
        port: u16,
        /// 기대하는 채널 ID(지정하면 불일치 시 중단).
        #[arg(long)]
        expect_channel: Option<String>,
    },
    /// 두 브라우저 세션을 비교해 빠진 좋아요·구독만 반영합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Reconcile {
        /// 원본 계정 Chrome DevTools 포트. 생략하면 저장된 계획의 원본 스냅샷을 쓴다.
        #[arg(long)]
        source_port: Option<u16>,
        /// 대상 계정 Chrome DevTools 포트.
        #[arg(long, default_value_t = 9222)]
        target_port: u16,
        /// 비교할 유형(쉼표 구분): likes, subscriptions.
        #[arg(
            long,
            value_enum,
            value_delimiter = ',',
            default_value = "likes,subscriptions",
            hide_possible_values = true
        )]
        types: Vec<ReconcileType>,
        /// 유형별 최대 추가 개수.
        #[arg(long)]
        limit: Option<usize>,
        /// 액션 사이 최소 대기(ms).
        #[arg(long, default_value_t = 3_000)]
        delay_ms: u64,
        /// 최소 대기에 더할 무작위 폭(ms).
        #[arg(long, default_value_t = 1_500)]
        jitter_ms: u64,
        /// 실제 쓰기 없이 차이만 확인합니다.
        #[arg(long)]
        dry_run: bool,
        /// 확인 없이 실행합니다.
        #[arg(long)]
        yes: bool,
        /// 브라우저 채널 확인을 생략합니다(권장하지 않음).
        #[arg(long)]
        skip_account_check: bool,
    },
    /// Watch Later(나중에 볼 동영상)를 브라우저 세션으로 내보내고 동기화합니다.
    #[command(
        help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
    )]
    WatchLater {
        /// Watch Later 하위 명령.
        #[command(subcommand)]
        command: WatchLaterCommand,
    },
    /// 저장된 계획을 브라우저 세션으로 실행합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Sync {
        /// 실행할 데이터 유형: subscriptions, playlists, likes, all.
        #[arg(value_enum, hide_possible_values = true)]
        kind: PushKind,
        /// 대상 계정 Chrome DevTools 포트.
        #[arg(long, default_value_t = 9222)]
        port: u16,
        /// 데이터 유형별 최대 액션 수.
        #[arg(long)]
        limit: Option<usize>,
        /// 액션 사이 최소 대기(ms).
        #[arg(long, default_value_t = 3_000)]
        delay_ms: u64,
        /// 최소 대기에 더할 무작위 폭(ms). 0이면 고정 간격.
        #[arg(long, default_value_t = 1_500)]
        jitter_ms: u64,
        /// 실제 쓰기 없이 예정만 출력합니다.
        #[arg(long)]
        dry_run: bool,
        /// 확인 없이 실행합니다.
        #[arg(long)]
        yes: bool,
        /// 브라우저 채널 확인을 생략합니다(권장하지 않음).
        #[arg(long)]
        skip_account_check: bool,
    },
}

/// 브라우저 비교 대상 유형.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReconcileType {
    /// 좋아요.
    Likes,
    /// 구독.
    Subscriptions,
}

/// Watch Later 하위 명령.
#[derive(Debug, Subcommand)]
#[command(
    help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
)]
pub enum WatchLaterCommand {
    /// 원본 계정의 Watch Later를 파일로 내보냅니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Export {
        /// 원본 계정 Chrome DevTools 포트.
        #[arg(long, default_value_t = 9223)]
        port: u16,
        /// 저장 경로(기본: data/watch-later.json).
        #[arg(long)]
        out: Option<PathBuf>,
        /// 기대하는 채널 ID(기본: 계획의 원본 채널).
        #[arg(long)]
        expect_channel: Option<String>,
    },
    /// 내보낸 목록을 대상 계정의 Watch Later에 반영합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Sync {
        /// 대상 계정 Chrome DevTools 포트.
        #[arg(long, default_value_t = 9222)]
        port: u16,
        /// 최대 추가 개수.
        #[arg(long)]
        limit: Option<usize>,
        /// 액션 사이 최소 대기(ms).
        #[arg(long, default_value_t = 3_000)]
        delay_ms: u64,
        /// 최소 대기에 더할 무작위 폭(ms).
        #[arg(long, default_value_t = 1_500)]
        jitter_ms: u64,
        /// 실제 쓰기 없이 차이만 확인합니다.
        #[arg(long)]
        dry_run: bool,
        /// 확인 없이 실행합니다.
        #[arg(long)]
        yes: bool,
        /// 대상에만 있는 항목을 삭제합니다(원본과 동일하게 맞춤).
        #[arg(long)]
        prune: bool,
        /// 브라우저 채널 확인을 생략합니다(권장하지 않음).
        #[arg(long)]
        skip_account_check: bool,
    },
}

/// 쿼터 하위 명령.
#[derive(Debug, Subcommand)]
#[command(
    help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
)]
pub enum QuotaCommand {
    /// 오늘 장부와 남은 쓰기 수를 보여줍니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Show {
        /// JSON으로 출력합니다.
        #[arg(long)]
        json: bool,
    },
    /// Google 콘솔에서 확인한 사용량으로 장부를 맞춥니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Sync {
        /// 콘솔에 표시된 오늘 사용량(units).
        #[arg(long)]
        used: u64,
        /// 오늘 예산(units). 생략하면 장부 값 또는 9500.
        #[arg(long)]
        budget: Option<u64>,
        /// 쓰기 단가(units). 콘솔 사용량과 계산이 어긋날 때 지정합니다.
        #[arg(long)]
        write_cost: Option<u64>,
    },
}

/// 시청기록 하위 명령.
#[derive(Debug, Subcommand)]
#[command(
    help_template = "{about-with-newline}\n사용법: {usage}\n\n명령:\n{subcommands}\n\n옵션:\n{options}"
)]
pub enum HistoryCommand {
    /// Takeout에서 시청기록을 가져옵니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Import {
        /// Takeout 디렉터리 또는 zip 경로.
        #[arg(long)]
        takeout: PathBuf,
    },
    /// 시청기록 통계를 출력합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Report {
        /// 상위 채널/영상 개수.
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// 출력 형식: markdown 또는 json.
        #[arg(
            long,
            value_enum,
            default_value = "markdown",
            hide_possible_values = true
        )]
        format: ReportFormat,
        /// 파일로 저장할 경로.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 시청기록을 비공개 플레이리스트로 재현합니다.
    #[command(help_template = "{about-with-newline}\n사용법: {usage}\n\n옵션:\n{options}")]
    Playlist {
        /// 플레이리스트 이름.
        #[arg(long, default_value = history::DEFAULT_PLAYLIST_NAME)]
        name: String,
        /// 최대 항목 수.
        #[arg(long)]
        limit: Option<usize>,
        /// 오늘 사용할 쿼터 예산(units). 기본: 장부 값 또는 9500.
        #[arg(long)]
        budget: Option<u64>,
        /// 실제 쓰기 없이 예상만 출력합니다.
        #[arg(long)]
        dry_run: bool,
        /// 확인 없이 실행합니다.
        #[arg(long)]
        yes: bool,
        /// 쓰기 요청 사이 지연(ms).
        #[arg(long, default_value_t = DEFAULT_DELAY_MS)]
        delay_ms: u64,
    },
}

/// 리포트 출력 형식.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    /// Markdown.
    Markdown,
    /// JSON.
    Json,
}

/// 명령을 실행하고 종료 코드를 돌려준다.
pub async fn run(cli: Cli) -> i32 {
    match dispatch(cli).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("오류: {error}");
            error.exit_code()
        }
    }
}

async fn dispatch(cli: Cli) -> Result<i32, AppError> {
    let paths = Paths::discover()?;
    let write_cost = cli.write_cost;
    match cli.command {
        Command::Auth { command } => auth_command(&paths, command).await,
        Command::Plan {
            types,
            refresh,
            json,
        } => plan_command(&paths, &types, refresh, json, write_cost).await,
        Command::Push {
            kind,
            limit,
            budget,
            delay_ms,
            yes,
        } => push_command(&paths, kind, limit, budget, delay_ms, yes, write_cost).await,
        Command::Migrate {
            limit,
            budget,
            delay_ms,
            refresh,
            yes,
        } => migrate_command(&paths, limit, budget, delay_ms, refresh, yes, write_cost).await,
        Command::Status { json } => status_command(&paths, json, write_cost).await,
        Command::Browser { command } => browser_command(&paths, command).await,
        Command::Quota { command } => quota_command(&paths, command, write_cost).await,
        Command::History { command } => history_command(&paths, command, write_cost).await,
        Command::Purge { tokens, yes } => purge_command(&paths, tokens, yes),
    }
}

fn build_api(paths: &Paths, account: Account) -> Result<Box<dyn YouTubeApi>, AppError> {
    let tokens = auth::resolve_token_source(paths, account)?;
    let client = HttpYouTube::new(base_from_env(), Arc::from(tokens))?;
    Ok(Box::new(client))
}

async fn auth_command(paths: &Paths, command: AuthCommand) -> Result<i32, AppError> {
    match command {
        AuthCommand::Login {
            account,
            client_secret,
            login_hint,
            no_browser,
        } => {
            let config_path = client_secret.unwrap_or_else(|| paths.client_secret());
            let config = OAuthConfig::load(&config_path)?;
            paths.ensure_layout()?;
            let store = FileTokenStore::new(paths.clone());
            let probe = ChannelProbe::new()?;
            let options = LoginOptions {
                no_browser,
                login_hint,
            };
            let outcome: LoginOutcome =
                auth::login(account, &config, &store, &options, &probe).await?;
            println!(
                "{} 계정 로그인 완료: {} ({})",
                account.label(),
                outcome.channel_title,
                outcome.channel_id
            );
            eprintln!("토큰을 저장했습니다: {}", outcome.token_path);
            Ok(0)
        }
        AuthCommand::Status { json } => {
            let report = auth::status(paths)?;
            if json {
                let value = serde_json::to_string_pretty(&report.to_json())
                    .map_err(|err| AppError::json("상태를 직렬화할 수 없습니다", err))?;
                println!("{value}");
            } else {
                for account in [Account::Source, Account::Target] {
                    let status = report.get(account);
                    if status.logged_in {
                        println!(
                            "{}: 로그인됨 · {} ({}) · refresh token {} · 토큰 파일 {}",
                            account.label(),
                            status.channel_title.as_deref().unwrap_or("-"),
                            status.channel_id.as_deref().unwrap_or("-"),
                            if status.has_refresh_token {
                                "있음"
                            } else {
                                "없음"
                            },
                            status.token_path
                        );
                    } else {
                        println!(
                            "{}: 로그인 안 됨 · `stk auth login --account {}`로 로그인하세요",
                            account.label(),
                            account
                        );
                    }
                }
            }
            Ok(0)
        }
        AuthCommand::Logout { account } => {
            if auth::logout(paths, account)? {
                println!("{} 계정 토큰을 삭제했습니다.", account.label());
            } else {
                println!("{} 계정에 저장된 토큰이 없습니다.", account.label());
            }
            Ok(0)
        }
    }
}

async fn plan_command(
    paths: &Paths,
    types: &[DataType],
    refresh: bool,
    json: bool,
    write_cost: Option<u64>,
) -> Result<i32, AppError> {
    if types.is_empty() {
        return Err(AppError::Usage(
            "--types에는 subscriptions, playlists, likes 중 하나 이상을 지정해야 합니다."
                .to_string(),
        ));
    }
    let _lock = DataLock::acquire(paths)?;
    let now = Utc::now();

    let source_api = build_api(paths, Account::Source)?;
    let snapshot =
        load_or_collect_snapshot(paths, source_api.as_ref(), Account::Source, now, refresh).await?;

    let target_api = build_api(paths, Account::Target)?;
    let target = fetch_target_state(target_api.as_ref(), &snapshot.playlists).await?;
    let plan = build_plan(&snapshot, &target, types, now)?;
    save_plan(paths, &plan)?;

    let (budget, write_cost) = quota_context(paths, None, write_cost, now);
    let summary = summary_json(&plan, budget, write_cost);
    if json {
        let text = serde_json::to_string_pretty(&summary)
            .map_err(|err| AppError::json("계획 요약을 직렬화할 수 없습니다", err))?;
        println!("{text}");
    } else {
        print!("{}", render_plan_summary(&summary));
        eprintln!("계획을 저장했습니다: {}", paths.plan().display());
    }
    Ok(0)
}

fn field(value: &Value, path: &[&str]) -> String {
    let mut current = value;
    for key in path {
        current = &current[*key];
    }
    current
        .as_u64()
        .map(|number| number.to_string())
        .or_else(|| current.as_str().map(str::to_string))
        .unwrap_or_else(|| "-".to_string())
}

/// 사람이 읽는 계획 요약.
pub fn render_plan_summary(summary: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "원본 채널: {} ({})\n",
        field(summary, &["source", "channel_title"]),
        field(summary, &["source", "channel_id"])
    ));
    out.push_str(&format!(
        "대상 채널: {} ({})\n",
        field(summary, &["target", "channel_title"]),
        field(summary, &["target", "channel_id"])
    ));
    out.push_str(&format!(
        "구독: 원본 {} · 대상 기존 {} · 추가 {}\n",
        field(summary, &["subscriptions", "source"]),
        field(summary, &["subscriptions", "target_existing"]),
        field(summary, &["subscriptions", "to_add"])
    ));
    out.push_str(&format!(
        "플레이리스트: 원본 {} · 생성 {} · 항목 추가 {} · 건너뜀 {} · 잘림 {}\n",
        field(summary, &["playlists", "source"]),
        field(summary, &["playlists", "to_create"]),
        field(summary, &["playlists", "items_to_add"]),
        field(summary, &["playlists", "items_skipped"]),
        field(summary, &["playlists", "items_truncated"])
    ));
    out.push_str(&format!(
        "좋아요: 원본 {} · 대상 기존 {} · 추가 {} (최근 1,000개까지만 조회)\n",
        field(summary, &["likes", "source"]),
        field(summary, &["likes", "target_existing"]),
        field(summary, &["likes", "to_add"])
    ));
    out.push_str(&format!(
        "쿼터: 쓰기 {}건 · 예상 {} units · 예상 {}일\n",
        field(summary, &["quota", "write_actions"]),
        field(summary, &["quota", "estimated_units"]),
        field(summary, &["quota", "estimated_days"])
    ));
    out
}

fn confirm(prompt: &str, yes: bool) -> Result<bool, AppError> {
    if yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        return Err(AppError::Usage(
            "확인이 필요하지만 표준 입력이 TTY가 아닙니다. --yes를 붙여 다시 실행하세요."
                .to_string(),
        ));
    }
    eprintln!("{prompt}\n계속하려면 yes를 입력하세요:");
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|err| AppError::io("표준 입력을 읽을 수 없습니다", err))?;
    Ok(input.trim() == "yes")
}

async fn push_command(
    paths: &Paths,
    kind: PushKind,
    limit: Option<usize>,
    budget: Option<u64>,
    delay_ms: u64,
    yes: bool,
    write_cost: Option<u64>,
) -> Result<i32, AppError> {
    let _lock = DataLock::acquire(paths)?;
    let now = Utc::now();
    let types = kind.data_types();
    let (budget, write_cost) = quota_context(paths, budget, write_cost, now);

    let source_api = build_api(paths, Account::Source)?;
    let snapshot = match load_plan(paths, now)? {
        Some(plan) if is_snapshot_fresh(&plan.source, now) => plan.source,
        _ => {
            let snapshot = fetch_snapshot(source_api.as_ref(), Account::Source, now).await?;
            save_snapshot(paths, &snapshot)?;
            snapshot
        }
    };

    let target_api = build_api(paths, Account::Target)?;
    let target = fetch_target_state(target_api.as_ref(), &snapshot.playlists).await?;
    let plan = build_plan(&snapshot, &target, &types, now)?;

    if plan.actions.is_empty() {
        println!("이미 완료되었습니다. 실행할 작업이 없습니다.");
        return Ok(0);
    }

    let quota = estimate_quota(&plan, budget, write_cost);
    let prompt = format!(
        "쓰기 {}건, 예상 쿼터 {}/{} units입니다.",
        quota.write_actions, quota.estimated_units, budget
    );
    if !confirm(&prompt, yes)? {
        eprintln!("취소했습니다.");
        return Ok(0);
    }

    let orders = expected_orders(&plan.source.playlists);
    run_plan(
        paths,
        target_api.as_ref(),
        &plan.actions,
        plan.playlist_targets.clone(),
        orders,
        ExecOptions {
            limit,
            delay: Duration::from_millis(delay_ms),
            verify_order: true,
            write_cost,
        },
        budget,
        now,
    )
    .await
}

/// 쿼터 장부와 플래그에서 예산·단가를 결정한다.
fn quota_context(
    paths: &Paths,
    budget: Option<u64>,
    write_cost: Option<u64>,
    now: chrono::DateTime<Utc>,
) -> (u64, u64) {
    let snapshot: Option<QuotaSnapshot> = QuotaLedger::peek(paths, now);
    let budget = budget
        .or_else(|| snapshot.map(|item| item.limit))
        .unwrap_or(DEFAULT_BUDGET);
    let write_cost = write_cost
        .or_else(|| snapshot.map(|item| item.write_cost))
        .unwrap_or(WRITE_QUOTA_UNIT);
    (budget, write_cost.max(1))
}

/// 액션 목록을 실행기로 돌리고 결과를 출력한다.
#[allow(clippy::too_many_arguments)]
async fn run_plan(
    paths: &Paths,
    api: &dyn YouTubeApi,
    actions: &[Action],
    targets: HashMap<String, String>,
    expected_orders: HashMap<String, Vec<String>>,
    opts: ExecOptions,
    budget: u64,
    now: chrono::DateTime<Utc>,
) -> Result<i32, AppError> {
    let mut journal = Journal::open(paths, now)?;
    let mut quota_ledger = QuotaLedger::open_with_cost(paths, budget, Some(opts.write_cost), now)?;
    let progress = ProgressReporter::new();
    let mut executor = Executor::new(ExecutorDeps {
        api,
        journal: &mut journal,
        quota: &mut quota_ledger,
        progress: &progress,
        opts,
        targets,
        expected_orders,
    });
    let summary = executor.run(actions, now).await?;
    print!("{}", summary.render());
    if summary.limit_reached() {
        eprintln!("--limit에 도달했습니다. 같은 명령을 다시 실행하면 이어서 진행합니다.");
    } else if summary.skipped_total() > 0 {
        eprintln!("건너뛴 항목은 journal에 기록되어 다시 시도하지 않습니다.");
    }
    Ok(0)
}

fn expected_orders(playlists: &[crate::youtube::Playlist]) -> HashMap<String, Vec<String>> {
    playlists
        .iter()
        .map(|playlist| {
            (
                playlist.id.clone(),
                playlist
                    .items
                    .iter()
                    .map(|item| item.video_id.clone())
                    .collect(),
            )
        })
        .collect()
}

async fn history_command(
    paths: &Paths,
    command: HistoryCommand,
    write_cost: Option<u64>,
) -> Result<i32, AppError> {
    match command {
        HistoryCommand::Import { takeout } => {
            let file = history::import(paths, &takeout, Utc::now())?;
            let report = history::build_report(&file.entries, 0);
            println!(
                "시청기록 {}건을 가져왔습니다: 원본 {}, 고유 영상 {}, 파싱 실패 {}, 광고 {}",
                report.total_events,
                file.source_file,
                report.unique_videos,
                report.unparsed_events,
                report.ad_events
            );
            eprintln!("저장 위치: {}", paths.history().display());
            Ok(0)
        }
        HistoryCommand::Report { top, format, out } => {
            let file: HistoryFile = history::load(paths)?;
            let report = history::build_report(&file.entries, top);
            let (payload, label) = match format {
                ReportFormat::Markdown => (report.to_markdown(), "Markdown"),
                ReportFormat::Json => {
                    let text = serde_json::to_string_pretty(&report.to_json())
                        .map_err(|err| AppError::json("리포트를 직렬화할 수 없습니다", err))?;
                    (text, "JSON")
                }
            };
            match out {
                Some(path) => {
                    if let Some(parent) = path
                        .parent()
                        .filter(|parent| !parent.as_os_str().is_empty())
                    {
                        std::fs::create_dir_all(parent).map_err(|err| {
                            AppError::io(
                                format!("디렉터리를 만들 수 없습니다: {}", parent.display()),
                                err,
                            )
                        })?;
                    }
                    std::fs::write(&path, payload).map_err(|err| {
                        AppError::io(format!("리포트를 쓸 수 없습니다: {}", path.display()), err)
                    })?;
                    eprintln!("{label} 리포트를 저장했습니다: {}", path.display());
                }
                None => println!("{payload}"),
            }
            Ok(0)
        }
        HistoryCommand::Playlist {
            name,
            limit,
            budget,
            dry_run,
            yes,
            delay_ms,
        } => {
            let _lock = DataLock::acquire(paths)?;
            let now = Utc::now();
            let (budget, write_cost) = quota_context(paths, budget, write_cost, now);
            let file = history::load(paths)?;
            let target_api = build_api(paths, Account::Target)?;
            let target = fetch_target_state(target_api.as_ref(), &[]).await?;
            let plan = history::plan_history_playlist(&file, &target, &name, limit)?;

            if dry_run {
                println!(
                    "플레이리스트 '{}': 항목 {}건, 생성 {}, 기존 건너뜀 {}, 잘림 {}",
                    plan.playlist_title,
                    plan.video_ids.len(),
                    if plan.existing_playlist_id.is_some() {
                        "없음(기존 재사용)"
                    } else {
                        "1건"
                    },
                    plan.skipped_existing,
                    plan.truncated
                );
                println!(
                    "예상 쿼터: {} units (쓰기 {}건, 단가 {})",
                    plan.actions.len() as u64 * write_cost,
                    plan.actions.len(),
                    write_cost
                );
                return Ok(0);
            }

            let estimate = plan.actions.len() as u64 * write_cost;
            let prompt = format!(
                "플레이리스트 '{}'에 항목 {}건을 추가합니다. 예상 쿼터 {estimate}/{budget} units.",
                plan.playlist_title,
                plan.video_ids.len()
            );
            if !confirm(&prompt, yes)? {
                eprintln!("취소했습니다.");
                return Ok(0);
            }

            let mut targets = HashMap::new();
            if let Some(existing) = &plan.existing_playlist_id {
                targets.insert(history::HISTORY_PLAYLIST_ID.to_string(), existing.clone());
            }
            let mut orders = HashMap::new();
            orders.insert(
                history::HISTORY_PLAYLIST_ID.to_string(),
                plan.video_ids.clone(),
            );

            run_plan(
                paths,
                target_api.as_ref(),
                &plan.actions,
                targets,
                orders,
                ExecOptions {
                    limit: None,
                    delay: Duration::from_millis(delay_ms),
                    verify_order: true,
                    write_cost,
                },
                budget,
                now,
            )
            .await?;
            println!(
                "플레이리스트 '{}' 처리 완료 (계획 항목 {}건)",
                plan.playlist_title,
                plan.video_ids.len()
            );
            Ok(0)
        }
    }
}

async fn watch_later_command(paths: &Paths, command: WatchLaterCommand) -> Result<i32, AppError> {
    let now = Utc::now();
    match command {
        WatchLaterCommand::Export {
            port,
            out,
            expect_channel,
        } => {
            let plan_channel = load_plan(paths, now)?.map(|plan| plan.source.channel.id);
            let expected = expect_channel.or(plan_channel).unwrap_or_default();
            let mut cdp = CdpClient::attach(port)
                .await
                .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
            let account = verify_account(&mut cdp, &expected).await?;
            let items = read_watch_later(&mut cdp, 50).await?;
            cdp.close().await;

            let file = WatchLaterFile {
                exported_at: now,
                channel_id: account["channel_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                items,
            };
            let path = out.unwrap_or_else(|| paths.watch_later());
            let payload = serde_json::to_vec_pretty(&file)
                .map_err(|error| AppError::json("Watch Later를 직렬화할 수 없습니다", error))?;
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).map_err(|error| {
                    AppError::io(
                        format!("디렉터리를 만들 수 없습니다: {}", parent.display()),
                        error,
                    )
                })?;
            }
            std::fs::write(&path, payload).map_err(|error| {
                AppError::io(
                    format!("Watch Later를 쓸 수 없습니다: {}", path.display()),
                    error,
                )
            })?;
            println!(
                "Watch Later {}건을 내보냈습니다: 채널 {}",
                file.items.len(),
                file.channel_id
            );
            eprintln!("저장 위치: {}", path.display());
            Ok(0)
        }
        WatchLaterCommand::Sync {
            port,
            limit,
            delay_ms,
            jitter_ms,
            dry_run,
            yes,
            prune,
            skip_account_check,
        } => {
            let path = paths.watch_later();
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    AppError::Usage(
                        "Watch Later 내보내기 파일이 없습니다. `stk browser watch-later export --port <원본 포트>`를 먼저 실행하세요."
                            .to_string(),
                    )
                } else {
                    AppError::io(
                        format!("Watch Later 파일을 읽을 수 없습니다: {}", path.display()),
                        error,
                    )
                }
            })?;
            let file: WatchLaterFile = serde_json::from_str(&raw).map_err(|error| {
                AppError::json(
                    format!("Watch Later 파일을 해석할 수 없습니다: {}", path.display()),
                    error,
                )
            })?;
            let opts = BrowserSyncOptions {
                delay: Duration::from_millis(delay_ms),
                jitter: Duration::from_millis(jitter_ms),
                retry_base: Duration::from_secs(5),
                limit,
            };

            let mut cdp = CdpClient::attach(port)
                .await
                .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
            if !skip_account_check {
                let expected = load_plan(paths, now)?
                    .map(|plan| plan.target.channel.id)
                    .unwrap_or_default();
                verify_account(&mut cdp, &expected).await?;
            }

            let target = read_watch_later(&mut cdp, 50).await?;
            let target_ids: std::collections::HashSet<String> =
                target.iter().map(|item| item.video_id.clone()).collect();
            let source_ids: std::collections::HashSet<String> = file
                .items
                .iter()
                .map(|item| item.video_id.clone())
                .collect();
            let to_add = plan_watch_later(&file.items, &target_ids, limit);
            let to_remove = if prune {
                target
                    .iter()
                    .filter(|item| !source_ids.contains(&item.video_id))
                    .count()
            } else {
                0
            };

            if dry_run {
                cdp.close().await;
                println!(
                    "Watch Later 차이: 원본 {} · 대상 {} · 추가 {} · 삭제 {}",
                    file.items.len(),
                    target.len(),
                    to_add.len(),
                    to_remove
                );
                for item in to_add.iter().take(5) {
                    println!("+ {}", item.video_id);
                }
                return Ok(0);
            }

            let prompt = format!(
                "Watch Later에 {}건 추가{}. 대상 계정 상태가 바뀝니다.",
                to_add.len(),
                if prune {
                    format!(", {}건 삭제", to_remove)
                } else {
                    String::new()
                }
            );
            if !confirm(&prompt, yes)? {
                cdp.close().await;
                eprintln!("취소했습니다.");
                return Ok(0);
            }

            let mut journal = Journal::open(paths, now)?;
            let report =
                sync_watch_later(&mut cdp, &file.items, &mut journal, &opts, prune, now).await?;
            cdp.close().await;
            print!("{}", report.render());
            Ok(0)
        }
    }
}

async fn browser_command(paths: &Paths, command: BrowserCommand) -> Result<i32, AppError> {
    match command {
        BrowserCommand::Check {
            port,
            expect_channel,
        } => {
            let mut cdp = CdpClient::attach(port)
                .await
                .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
            let account = verify_account(&mut cdp, expect_channel.as_deref().unwrap_or("")).await?;
            cdp.close().await;
            println!(
                "브라우저 확인: 채널 {} · 로그인됨 · 세션 {}",
                account["channel_id"].as_str().unwrap_or("-"),
                account["session_index"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| account["session_index"]
                        .as_u64()
                        .map(|value| value.to_string()))
                    .unwrap_or_else(|| "-".to_string())
            );
            Ok(0)
        }
        BrowserCommand::Reconcile {
            source_port,
            target_port,
            types,
            limit,
            delay_ms,
            jitter_ms,
            dry_run,
            yes,
            skip_account_check,
        } => {
            let now = Utc::now();
            let plan = load_plan(paths, now)?;
            let opts = BrowserSyncOptions {
                delay: Duration::from_millis(delay_ms),
                jitter: Duration::from_millis(jitter_ms),
                retry_base: Duration::from_secs(5),
                limit,
            };

            let want_likes = types.contains(&ReconcileType::Likes);
            let want_subs = types.contains(&ReconcileType::Subscriptions);
            let (source_likes, source_subs) = match source_port {
                Some(port) => {
                    let mut source_cdp = CdpClient::attach(port)
                        .await
                        .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
                    if !skip_account_check {
                        let expected = plan
                            .as_ref()
                            .map(|plan| plan.source.channel.id.clone())
                            .unwrap_or_default();
                        verify_account(&mut source_cdp, &expected).await?;
                    }
                    let likes = if want_likes {
                        Some(
                            read_liked_videos(&mut source_cdp, 50)
                                .await?
                                .into_iter()
                                .map(|item| item.video_id)
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        None
                    };
                    let subs = if want_subs {
                        Some(
                            read_subscriptions(&mut source_cdp, 50)
                                .await?
                                .into_iter()
                                .map(|item| item.video_id)
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        None
                    };
                    source_cdp.close().await;
                    (likes, subs)
                }
                None => {
                    let Some(snapshot) = plan.as_ref().map(|plan| &plan.source) else {
                        return Err(AppError::Usage(
                            "--source-port를 생략하려면 저장된 계획이 필요합니다. `stk plan --refresh`를 먼저 실행하세요."
                                .to_string(),
                        ));
                    };
                    let likes = want_likes.then(|| {
                        snapshot
                            .likes
                            .iter()
                            .map(|item| item.video_id.clone())
                            .collect::<Vec<_>>()
                    });
                    let subs = want_subs.then(|| {
                        snapshot
                            .subscriptions
                            .iter()
                            .map(|item| item.channel_id.clone())
                            .collect::<Vec<_>>()
                    });
                    (likes, subs)
                }
            };

            let mut target_cdp = CdpClient::attach(target_port)
                .await
                .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
            if !skip_account_check {
                let expected = plan
                    .as_ref()
                    .map(|plan| plan.target.channel.id.clone())
                    .unwrap_or_default();
                verify_account(&mut target_cdp, &expected).await?;
            }

            if dry_run {
                let mut lines = Vec::new();
                if let Some(likes) = &source_likes {
                    let target = read_liked_videos(&mut target_cdp, 50).await?;
                    let target_ids: std::collections::HashSet<String> =
                        target.iter().map(|item| item.video_id.clone()).collect();
                    let missing = diff_missing(likes, &target_ids, limit);
                    lines.push(format!(
                        "좋아요: 원본 {} · 대상 {} · 추가 {}",
                        likes.len(),
                        target.len(),
                        missing.len()
                    ));
                }
                if let Some(subs) = &source_subs {
                    let target = read_subscriptions(&mut target_cdp, 50).await?;
                    let target_ids: std::collections::HashSet<String> =
                        target.iter().map(|item| item.video_id.clone()).collect();
                    let missing = diff_missing(subs, &target_ids, limit);
                    lines.push(format!(
                        "구독: 원본 {} · 대상 {} · 추가 {}",
                        subs.len(),
                        target.len(),
                        missing.len()
                    ));
                }
                target_cdp.close().await;
                for line in lines {
                    println!("{line}");
                }
                return Ok(0);
            }

            let total_add = source_likes.as_ref().map(|v| v.len()).unwrap_or(0)
                + source_subs.as_ref().map(|v| v.len()).unwrap_or(0);
            let prompt = format!(
                "브라우저 기준으로 빠진 항목만 반영합니다(원본 좋아요 {}건 · 구독 {}건 기준). 대상 계정 상태가 바뀝니다.",
                source_likes.as_ref().map(|v| v.len()).unwrap_or(0),
                source_subs.as_ref().map(|v| v.len()).unwrap_or(0)
            );
            let _ = total_add;
            if !confirm(&prompt, yes)? {
                target_cdp.close().await;
                eprintln!("취소했습니다.");
                return Ok(0);
            }

            let mut journal = Journal::open(paths, now)?;
            let report = reconcile(
                &mut target_cdp,
                source_likes.as_deref(),
                source_subs.as_deref(),
                &mut journal,
                &opts,
                now,
            )
            .await?;
            target_cdp.close().await;
            print!("{}", report.render());
            Ok(0)
        }
        BrowserCommand::WatchLater { command } => watch_later_command(paths, command).await,
        BrowserCommand::Sync {
            kind,
            port,
            limit,
            delay_ms,
            jitter_ms,
            dry_run,
            yes,
            skip_account_check,
        } => {
            let now = Utc::now();
            let Some(plan) = load_plan(paths, now)? else {
                return Err(AppError::Usage(
                    "저장된 계획이 없습니다. `stk plan`을 먼저 실행하세요.".to_string(),
                ));
            };
            let types = kind.data_types();
            let actions: Vec<Action> = plan
                .actions
                .iter()
                .filter(|action| types.contains(&action.kind.data_type()))
                .cloned()
                .collect();

            let pending = {
                let journal = Journal::open(paths, now)?;
                actions
                    .iter()
                    .filter(|action| !journal.is_complete(&action.key))
                    .count()
            };
            if pending == 0 {
                println!("이미 완료되었습니다. 브라우저에서 실행할 작업이 없습니다.");
                return Ok(0);
            }

            if dry_run {
                println!(
                    "브라우저 실행 예정: {pending}건 (유형별 상한 {}, 대기 {}ms)",
                    limit
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "없음".to_string()),
                    delay_ms
                );
                println!("간격: 최소 {delay_ms}ms + 지터 0~{jitter_ms}ms");
                for action in actions.iter().take(10) {
                    println!("- {}", action.key);
                }
                if pending > 10 {
                    println!("- … 외 {}건", pending - 10);
                }
                return Ok(0);
            }

            let prompt = format!(
                "브라우저 세션으로 쓰기 {pending}건을 실행합니다. 대상 계정 상태가 실제로 바뀝니다."
            );
            if !confirm(&prompt, yes)? {
                eprintln!("취소했습니다.");
                return Ok(0);
            }

            let mut cdp = CdpClient::attach(port)
                .await
                .map_err(|error| AppError::Config(format!("{error} — {}", error.hint())))?;
            if !skip_account_check {
                verify_account(&mut cdp, &plan.target.channel.id).await?;
            }

            let mut journal = Journal::open(paths, now)?;
            let mut targets = plan.playlist_targets.clone();
            let opts = BrowserSyncOptions {
                delay: Duration::from_millis(delay_ms),
                jitter: Duration::from_millis(jitter_ms),
                retry_base: Duration::from_secs(5),
                limit,
            };
            let report =
                sync_actions(&mut cdp, &actions, &mut journal, &mut targets, &opts, now).await?;
            cdp.close().await;
            print!("{}", report.render());
            eprintln!("진행률은 `stk status`로 확인하고, 다음 실행은 같은 명령을 반복하면 됩니다.");
            Ok(0)
        }
    }
}

async fn status_command(
    paths: &Paths,
    json: bool,
    write_cost: Option<u64>,
) -> Result<i32, AppError> {
    let now = Utc::now();
    let Some(plan) = load_plan(paths, now)? else {
        return Err(AppError::Usage(
            "저장된 계획이 없습니다. `stk plan` 또는 `stk migrate`를 먼저 실행하세요.".to_string(),
        ));
    };
    let journal = Journal::open(paths, now)?;
    let snapshot = QuotaLedger::peek(paths, now);
    let (budget, write_cost) = quota_context(paths, None, write_cost, now);
    let status = build_status(&plan, &journal, snapshot, write_cost, budget);
    if json {
        let text = serde_json::to_string_pretty(&status)
            .map_err(|err| AppError::json("진행률을 직렬화할 수 없습니다", err))?;
        println!("{text}");
    } else {
        print!("{}", status.render());
        if status.pending_actions > 0 {
            eprintln!(
                "다음 실행: `stk migrate{}` 또는 `stk push <유형>`",
                if status.pending_days > 1 {
                    " (하루 예산 한도까지 자동 진행)"
                } else {
                    ""
                }
            );
        } else {
            eprintln!("이전이 끝났습니다. `stk purge`로 API 데이터를 정리할 수 있습니다.");
        }
    }
    Ok(0)
}

async fn quota_command(
    paths: &Paths,
    command: QuotaCommand,
    write_cost: Option<u64>,
) -> Result<i32, AppError> {
    let now = Utc::now();
    match command {
        QuotaCommand::Show { json } => {
            let snapshot = QuotaLedger::peek(paths, now);
            if json {
                let payload = serde_json::json!({"today": snapshot});
                let text = serde_json::to_string_pretty(&payload)
                    .map_err(|err| AppError::json("쿼터 상태를 직렬화할 수 없습니다", err))?;
                println!("{text}");
            } else {
                match snapshot {
                    Some(snapshot) => {
                        println!("태평양 날짜: {} (오늘)", snapshot.date_pt);
                        println!(
                            "사용/예산: {}/{} units (남은 {})",
                            snapshot.used,
                            snapshot.limit,
                            snapshot.remaining()
                        );
                        println!(
                            "쓰기 단가: {} units → 남은 쓰기 {}건",
                            snapshot.write_cost,
                            snapshot.writes_left()
                        );
                        println!("리셋: 태평양 시간 자정");
                    }
                    None => {
                        let cost = write_cost.unwrap_or(WRITE_QUOTA_UNIT);
                        println!(
                            "오늘 쿼터 장부가 없습니다. 태평양 자정 이후 첫 쓰기에서 자동 초기화됩니다."
                        );
                        println!(
                            "기본 단가 {} units · 기본 예산 {} units → 하루 {}건",
                            cost,
                            DEFAULT_BUDGET,
                            DEFAULT_BUDGET / cost.max(1)
                        );
                    }
                }
            }
            Ok(0)
        }
        QuotaCommand::Sync {
            used,
            budget,
            write_cost: sync_cost,
        } => {
            let (budget, cost) = quota_context(paths, budget, sync_cost.or(write_cost), now);
            let mut ledger = QuotaLedger::open_with_cost(paths, budget, Some(cost), now)?;
            ledger.sync_used(used)?;
            let snapshot = ledger.snapshot();
            println!(
                "쿼터 장부를 맞췄습니다: {}/{} units · 단가 {} units · 남은 쓰기 {}건",
                snapshot.used,
                snapshot.limit,
                snapshot.write_cost,
                snapshot.writes_left()
            );
            Ok(0)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn migrate_command(
    paths: &Paths,
    limit: Option<usize>,
    budget: Option<u64>,
    delay_ms: u64,
    refresh: bool,
    yes: bool,
    write_cost: Option<u64>,
) -> Result<i32, AppError> {
    let _lock = DataLock::acquire(paths)?;
    let now = Utc::now();
    let (budget, write_cost) = quota_context(paths, budget, write_cost, now);
    let types = [
        DataType::Subscriptions,
        DataType::Playlists,
        DataType::Likes,
    ];

    let source_api = build_api(paths, Account::Source)?;
    let snapshot =
        load_or_collect_snapshot(paths, source_api.as_ref(), Account::Source, now, refresh).await?;
    let target_api = build_api(paths, Account::Target)?;
    let target = fetch_target_state(target_api.as_ref(), &snapshot.playlists).await?;
    let plan = build_plan(&snapshot, &target, &types, now)?;
    save_plan(paths, &plan)?;

    if plan.actions.is_empty() {
        println!("이미 완료되었습니다. 실행할 작업이 없습니다.");
        return Ok(0);
    }

    let summary = summary_json(&plan, budget, write_cost);
    print!("{}", render_plan_summary(&summary));
    eprintln!("계획을 저장했습니다: {}", paths.plan().display());

    let estimate = estimate_quota(&plan, budget, write_cost);
    let prompt = format!(
        "쓰기 {}건, 예상 쿼터 {}/{} units입니다.",
        estimate.write_actions, estimate.estimated_units, budget
    );
    if !confirm(&prompt, yes)? {
        eprintln!("취소했습니다.");
        return Ok(0);
    }

    let orders = expected_orders(&plan.source.playlists);
    run_plan(
        paths,
        target_api.as_ref(),
        &plan.actions,
        plan.playlist_targets.clone(),
        orders,
        ExecOptions {
            limit,
            delay: Duration::from_millis(delay_ms),
            verify_order: true,
            write_cost,
        },
        budget,
        now,
    )
    .await
}

fn purge_command(paths: &Paths, tokens: bool, yes: bool) -> Result<i32, AppError> {
    let _lock = DataLock::acquire(paths)?;
    let files = [
        paths.snapshot(),
        paths.history(),
        paths.watch_later(),
        paths.plan(),
        paths.journal(),
        paths.quota(),
    ];
    let existing: Vec<PathBuf> = files.into_iter().filter(|path| path.exists()).collect();

    if existing.is_empty() && !tokens {
        println!("삭제할 API 데이터가 없습니다.");
        return Ok(0);
    }

    let prompt = format!(
        "다음 {}개 파일을 삭제합니다{}. 이 작업은 되돌릴 수 없습니다.",
        existing.len(),
        if tokens { " (토큰 포함)" } else { "" }
    );
    if !confirm(&prompt, yes)? {
        eprintln!("취소했습니다.");
        return Ok(0);
    }

    for path in &existing {
        std::fs::remove_file(path).map_err(|err| {
            AppError::io(
                format!("파일을 삭제할 수 없습니다: {}", path.display()),
                err,
            )
        })?;
    }
    if tokens {
        let tokens_dir = paths.tokens_dir();
        match std::fs::remove_dir_all(&tokens_dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(AppError::io(
                    format!(
                        "토큰 디렉터리를 삭제할 수 없습니다: {}",
                        tokens_dir.display()
                    ),
                    err,
                ));
            }
        }
    }

    if existing.is_empty() {
        println!("삭제한 API 데이터가 없습니다.");
    } else {
        println!("삭제한 파일:");
        for path in &existing {
            println!("- {}", path.display());
        }
    }
    if tokens {
        println!("토큰을 삭제했습니다. 다시 사용하려면 로그인하세요.");
    }
    Ok(0)
}
