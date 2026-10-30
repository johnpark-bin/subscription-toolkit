//! 브라우저 세션 기반 실행 백엔드.
//!
//! OAuth API 경로와 별개로, 사용자가 직접 로그인한 Chrome에 DevTools Protocol로 붙어
//! 동기화를 수행한다. 지문 위장이나 자동화 탐지 우회는 하지 않으며, 서비스 약관과 계정
//! 제재 위험은 사용자가 감수한다.

pub mod cdp;
pub mod jitter;
pub mod page;
pub mod reconcile;
pub mod sync;
pub mod watch_later;

pub use cdp::{CdpClient, CdpError, PageResponse};
pub use reconcile::{
    ReconcileReport, diff_missing, read_liked_videos, read_subscriptions, reconcile,
};
pub use sync::{BrowserSyncOptions, BrowserSyncReport, sync_actions, verify_account};
pub use watch_later::{
    WatchLaterFile, WatchLaterItem, WatchLaterReport, plan_watch_later, read_watch_later,
    sync_watch_later,
};
