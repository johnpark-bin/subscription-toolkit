//! 실행 중 사용하는 파일 경로와 권한.

use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs2::FileExt;

use crate::account::Account;
use crate::error::AppError;

/// `stk`가 읽고 쓰는 모든 파일의 위치.
///
/// 기본 루트는 `~/.config/subscription-toolkit`이고, `STK_HOME` 환경 변수로 덮어쓸 수 있다.
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
}

impl Paths {
    /// 환경 변수와 홈 디렉터리에서 경로를 찾는다.
    pub fn discover() -> Result<Self, AppError> {
        Self::discover_from(|key| std::env::var(key).ok())
    }

    /// 환경 변수 조회 함수를 주입해 경로를 찾는다.
    ///
    /// 테스트에서 환경 변수를 전역으로 바꾸지 않기 위한 진입점이다.
    pub fn discover_from<F>(lookup: F) -> Result<Self, AppError>
    where
        F: Fn(&str) -> Option<String>,
    {
        if let Some(root) = lookup("STK_HOME").filter(|value| !value.trim().is_empty()) {
            return Ok(Self::with_root(PathBuf::from(root)));
        }
        let base = directories::BaseDirs::new().ok_or_else(|| {
            AppError::Config("홈 디렉터리를 찾을 수 없습니다. STK_HOME을 지정하세요.".to_string())
        })?;
        Ok(Self::with_root(
            base.home_dir().join(".config").join("subscription-toolkit"),
        ))
    }

    /// 루트를 직접 지정한다.
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// 루트 디렉터리.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// OAuth 클라이언트 설정 파일.
    pub fn client_secret(&self) -> PathBuf {
        self.root.join("client_secret.json")
    }

    /// 계정별 토큰 디렉터리.
    pub fn tokens_dir(&self) -> PathBuf {
        self.root.join("tokens")
    }

    /// 계정 토큰 파일.
    pub fn token(&self, account: Account) -> PathBuf {
        self.tokens_dir().join(format!("{account}.json"))
    }

    /// 데이터 디렉터리.
    pub fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    /// 원본 스냅샷 디렉터리.
    pub fn source_dir(&self) -> PathBuf {
        self.data_dir().join("source")
    }

    /// 원본 계정 스냅샷.
    pub fn snapshot(&self) -> PathBuf {
        self.source_dir().join("snapshot.json")
    }

    /// 정규화한 시청기록.
    pub fn history(&self) -> PathBuf {
        self.data_dir().join("history.json")
    }

    /// 이전 계획.
    pub fn plan(&self) -> PathBuf {
        self.data_dir().join("plan.json")
    }

    /// 실행 journal.
    pub fn journal(&self) -> PathBuf {
        self.data_dir().join("journal.jsonl")
    }

    /// 쿼터 장부.
    pub fn quota(&self) -> PathBuf {
        self.data_dir().join("quota.json")
    }

    /// 동시 실행 방지 잠금 파일.
    pub fn lock_file(&self) -> PathBuf {
        self.data_dir().join(".lock")
    }

    /// 모든 디렉터리를 만들고 권한을 `0700`으로 맞춘다.
    pub fn ensure_layout(&self) -> Result<(), AppError> {
        for dir in [
            self.root.clone(),
            self.tokens_dir(),
            self.data_dir(),
            self.source_dir(),
        ] {
            self.ensure_dir(&dir)?;
        }
        Ok(())
    }

    /// 디렉터리를 만들고 권한을 `0700`으로 맞춘다.
    pub fn ensure_dir(&self, dir: &Path) -> Result<(), AppError> {
        fs::create_dir_all(dir).map_err(|err| {
            AppError::io(
                format!("디렉터리를 만들 수 없습니다: {}", dir.display()),
                err,
            )
        })?;
        restrict_mode(dir, 0o700).map_err(|err| {
            AppError::io(
                format!("디렉터리 권한을 설정할 수 없습니다: {}", dir.display()),
                err,
            )
        })
    }

    /// 파일 권한을 `0600`으로 맞춘다.
    pub fn restrict_file(&self, file: &Path) -> Result<(), AppError> {
        restrict_mode(file, 0o600).map_err(|err| {
            AppError::io(
                format!("파일 권한을 설정할 수 없습니다: {}", file.display()),
                err,
            )
        })
    }

    /// 파일을 원자적으로 쓴다(임시 파일 → 이름 변경).
    pub fn write_atomic(&self, path: &Path, contents: &[u8]) -> Result<(), AppError> {
        if let Some(parent) = path.parent() {
            self.ensure_dir(parent)?;
        }
        write_file_atomic(path, contents)
    }
}

/// 파일/디렉터리 권한을 바꾼다. Windows에서는 아무 것도 하지 않는다.
pub fn restrict_permissions(path: &Path, mode: u32) -> std::io::Result<()> {
    restrict_mode(path, mode)
}

/// 파일을 원자적으로 쓴다. 상위 디렉터리가 이미 있어야 한다.
pub fn write_file_atomic(path: &Path, contents: &[u8]) -> Result<(), AppError> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)
        .map_err(|err| AppError::io(format!("파일을 쓸 수 없습니다: {}", tmp.display()), err))?;
    restrict_mode(&tmp, 0o600).map_err(|err| {
        AppError::io(
            format!("파일 권한을 설정할 수 없습니다: {}", tmp.display()),
            err,
        )
    })?;
    fs::rename(&tmp, path).map_err(|err| {
        AppError::io(
            format!("파일을 교체할 수 없습니다: {}", path.display()),
            err,
        )
    })
}

/// 동시 실행을 막는 잠금 파일 핸들. 드롭되면 잠금이 풀린다.
#[derive(Debug)]
pub struct DataLock {
    file: std::fs::File,
}

impl DataLock {
    /// `<home>/data/.lock`에 배타 잠금을 건다.
    pub fn acquire(paths: &Paths) -> Result<Self, AppError> {
        paths.ensure_layout()?;
        let path = paths.lock_file();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|err| {
                AppError::io(
                    format!("잠금 파일을 열 수 없습니다: {}", path.display()),
                    err,
                )
            })?;
        file.try_lock_exclusive().map_err(|_| {
            AppError::Config(
                "다른 stk 프로세스가 실행 중입니다. 끝난 뒤 다시 시도하세요.".to_string(),
            )
        })?;
        Ok(Self { file })
    }
}

impl Drop for DataLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(unix)]
fn restrict_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn restrict_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_respects_stk_home() {
        let dir = tempfile::tempdir().expect("임시 디렉터리 생성 실패");
        let root = dir.path().join("stk-home");
        let paths =
            Paths::discover_from(|key| (key == "STK_HOME").then(|| root.display().to_string()))
                .expect("경로 탐색 실패");

        assert_eq!(paths.root(), root.as_path());
        assert_eq!(paths.client_secret(), root.join("client_secret.json"));
        assert_eq!(
            paths.token(Account::Source),
            root.join("tokens").join("source.json")
        );
        assert_eq!(
            paths.snapshot(),
            root.join("data").join("source").join("snapshot.json")
        );
        assert_eq!(paths.history(), root.join("data").join("history.json"));
        assert_eq!(paths.lock_file(), root.join("data").join(".lock"));

        paths.ensure_layout().expect("디렉터리 생성 실패");
        assert!(paths.source_dir().is_dir());
        assert!(paths.tokens_dir().is_dir());
    }

    #[test]
    fn paths_fall_back_to_config_home() {
        let paths = Paths::discover_from(|_| None).expect("경로 탐색 실패");
        assert!(paths.root().ends_with(".config/subscription-toolkit"));
    }
}
