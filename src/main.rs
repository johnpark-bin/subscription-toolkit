//! `stk` 명령줄 진입점.

use std::process::ExitCode;

use clap::Parser;
use subscription_toolkit::cli::{Cli, run};
use tracing_subscriber::EnvFilter;

fn init_tracing(verbose: bool, quiet: bool) {
    let default_level = if verbose {
        "debug"
    } else if quiet {
        "error"
    } else {
        "warn"
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.quiet);

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("오류: 비동기 런타임을 시작할 수 없습니다: {error}");
            return ExitCode::from(1);
        }
    };

    let code = runtime.block_on(run(cli));
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
