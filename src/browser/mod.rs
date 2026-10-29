//! 브라우저 세션 기반 실행 백엔드.
//!
//! OAuth API 경로와 별개로, 사용자가 직접 로그인한 Chrome에 DevTools Protocol로 붙어
//! 동기화를 수행한다. 지문 위장이나 자동화 탐지 우회는 하지 않으며, 서비스 약관과 계정
//! 제재 위험은 사용자가 감수한다.

pub mod cdp;
pub mod page;
pub mod sync;

pub use cdp::{CdpClient, CdpError, PageResponse};
pub use sync::{BrowserSyncOptions, BrowserSyncReport, sync_actions, verify_account};
