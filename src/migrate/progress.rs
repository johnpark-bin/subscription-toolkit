//! 유형별 진행률 표시.

use std::io::IsTerminal;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use crate::migrate::model::DataType;

/// 진행률 출력기.
///
/// TTY에서는 유형별 진행 바를, TTY가 아니면 유형 시작/종료와 실패만 stderr에 쓴다.
pub struct ProgressReporter {
    tty: bool,
    multi: MultiProgress,
}

impl Default for ProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressReporter {
    /// stderr가 TTY인지 확인해 만든다.
    pub fn new() -> Self {
        Self::with_tty(std::io::stderr().is_terminal())
    }

    /// TTY 여부를 직접 지정한다(테스트용).
    pub fn with_tty(tty: bool) -> Self {
        Self {
            tty,
            multi: MultiProgress::new(),
        }
    }

    /// TTY 여부.
    pub fn is_tty(&self) -> bool {
        self.tty
    }

    /// 데이터 유형의 진행률을 시작한다.
    pub fn start_type(&self, data_type: DataType, total: u64) -> TypeProgress {
        if !self.tty {
            eprintln!("{} 0/{total}", data_type.label());
            return TypeProgress {
                bar: None,
                label: data_type.label(),
            };
        }
        let bar = self.multi.add(ProgressBar::new(total));
        let style = ProgressStyle::with_template("{prefix:>12} {pos}/{len}")
            .unwrap_or_else(|_| ProgressStyle::default_bar());
        bar.set_style(style);
        bar.set_prefix(data_type.label());
        TypeProgress {
            bar: Some(bar),
            label: data_type.label(),
        }
    }

    /// 사람이 읽는 메시지를 stderr에 남긴다.
    pub fn message(&self, text: &str) {
        if self.tty {
            let _ = self.multi.println(text);
        } else {
            eprintln!("{text}");
        }
    }
}

/// 한 데이터 유형의 진행률.
pub struct TypeProgress {
    bar: Option<ProgressBar>,
    label: &'static str,
}

impl TypeProgress {
    /// 진행률을 하나 올린다.
    pub fn advance(&self, _ok: bool) {
        if let Some(bar) = &self.bar {
            bar.inc(1);
        }
    }

    /// 진행률을 끝낸다.
    pub fn finish(&self) {
        match &self.bar {
            Some(bar) => bar.finish_and_clear(),
            None => eprintln!("{} 종료", self.label),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_tty_reporter_has_no_bars() {
        let reporter = ProgressReporter::with_tty(false);
        assert!(!reporter.is_tty());
        let progress = reporter.start_type(DataType::Subscriptions, 3);
        progress.advance(true);
        progress.finish();
    }

    #[test]
    fn tty_reporter_reports_tty() {
        let reporter = ProgressReporter::with_tty(true);
        assert!(reporter.is_tty());
        let progress = reporter.start_type(DataType::Likes, 1);
        progress.advance(false);
        progress.finish();
    }
}
