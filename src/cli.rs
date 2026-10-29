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
use crate::config::OAuthConfig;
use crate::error::AppError;
use crate::history::{self, HistoryFile};
use crate::migrate::executor::{ExecOptions, Executor, ExecutorDeps};
use crate::migrate::journal::Journal;
use crate::migrate::model::{DataType, PushKind};
use crate::migrate::progress::ProgressReporter;
use crate::migrate::quota::QuotaLedger;
use crate::migrate::{
    build_plan, estimate_quota, fetch_snapshot, fetch_target_state, is_snapshot_fresh,
    load_or_collect_snapshot, load_plan, save_plan, save_snapshot, summary_json,
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
        /// 오늘 사용할 쿼터 예산(units).
        #[arg(long, default_value_t = DEFAULT_BUDGET)]
        budget: u64,
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
        /// 오늘 사용할 쿼터 예산(units).
        #[arg(long, default_value_t = DEFAULT_BUDGET)]
        budget: u64,
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
    match cli.command {
        Command::Auth { command } => auth_command(&paths, command).await,
        Command::Plan {
            types,
            refresh,
            json,
        } => plan_command(&paths, &types, refresh, json).await,
        Command::Push {
            kind,
            limit,
            budget,
            delay_ms,
            yes,
        } => push_command(&paths, kind, limit, budget, delay_ms, yes).await,
        Command::History { command } => history_command(&paths, command).await,
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

    let summary = summary_json(&plan, DEFAULT_BUDGET);
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
    budget: u64,
    delay_ms: u64,
    yes: bool,
) -> Result<i32, AppError> {
    let _lock = DataLock::acquire(paths)?;
    let now = Utc::now();
    let types = kind.data_types();

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

    let quota = estimate_quota(&plan, budget);
    let prompt = format!(
        "쓰기 {}건, 예상 쿼터 {}/{} units입니다.",
        quota.write_actions, quota.estimated_units, budget
    );
    if !confirm(&prompt, yes)? {
        eprintln!("취소했습니다.");
        return Ok(0);
    }

    let mut journal = Journal::open(paths, now)?;
    let mut quota_ledger = QuotaLedger::open(paths, budget, now)?;
    let progress = ProgressReporter::new();
    let expected_orders = expected_orders(&plan.source.playlists);
    let mut executor = Executor::new(ExecutorDeps {
        api: target_api.as_ref(),
        journal: &mut journal,
        quota: &mut quota_ledger,
        progress: &progress,
        opts: ExecOptions {
            limit,
            delay: Duration::from_millis(delay_ms),
            verify_order: true,
        },
        targets: plan.playlist_targets.clone(),
        expected_orders,
    });
    let summary = executor.run(&plan.actions, now).await?;
    print!("{}", summary.render());
    if summary.limit_reached() {
        eprintln!("--limit에 도달했습니다. 다시 실행하면 이어서 진행합니다.");
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

async fn history_command(paths: &Paths, command: HistoryCommand) -> Result<i32, AppError> {
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
                    "예상 쿼터: {} units (쓰기 {}건)",
                    plan.actions.len() as u64 * crate::migrate::WRITE_QUOTA_UNIT,
                    plan.actions.len()
                );
                return Ok(0);
            }

            let estimate = plan.actions.len() as u64 * crate::migrate::WRITE_QUOTA_UNIT;
            let prompt = format!(
                "플레이리스트 '{}'에 항목 {}건을 추가합니다. 예상 쿼터 {estimate}/{budget} units.",
                plan.playlist_title,
                plan.video_ids.len()
            );
            if !confirm(&prompt, yes)? {
                eprintln!("취소했습니다.");
                return Ok(0);
            }

            let mut journal = Journal::open(paths, now)?;
            let mut quota_ledger = QuotaLedger::open(paths, budget, now)?;
            let progress = ProgressReporter::new();
            let mut targets = HashMap::new();
            if let Some(existing) = &plan.existing_playlist_id {
                targets.insert(history::HISTORY_PLAYLIST_ID.to_string(), existing.clone());
            }
            let mut orders = HashMap::new();
            orders.insert(
                history::HISTORY_PLAYLIST_ID.to_string(),
                plan.video_ids.clone(),
            );

            let mut executor = Executor::new(ExecutorDeps {
                api: target_api.as_ref(),
                journal: &mut journal,
                quota: &mut quota_ledger,
                progress: &progress,
                opts: ExecOptions {
                    limit: None,
                    delay: Duration::from_millis(delay_ms),
                    verify_order: true,
                },
                targets,
                expected_orders: orders,
            });
            let summary = executor.run(&plan.actions, now).await?;
            print!("{}", summary.render());
            println!(
                "플레이리스트 '{}' 처리 완료 (항목 {}건)",
                plan.playlist_title,
                plan.video_ids.len()
            );
            Ok(0)
        }
    }
}

fn purge_command(paths: &Paths, tokens: bool, yes: bool) -> Result<i32, AppError> {
    let _lock = DataLock::acquire(paths)?;
    let files = [
        paths.snapshot(),
        paths.history(),
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
