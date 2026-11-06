# subscription-toolkit

구독 계정 관리 도구 모음. 단일 바이너리 `stk`로 제공하는 Rust CLI다.

첫 기능은 YouTube 계정 이전이다. 원본 계정의 구독, 플레이리스트와 항목, 좋아요를 대상 계정으로 복사하고, Google Takeout 시청기록을 분석해 리포트와 비공개 플레이리스트를 만든다. 원본 계정의 데이터는 삭제하지 않는다. 모든 작업은 대상 계정으로의 복사다.

## 요구 사항

- Rust 1.96.1 이상(2024 edition)
- Google Cloud 프로젝트의 OAuth 데스크톱 클라이언트(`youtube.force-ssl` 스코프)
- 원본/대상 두 계정 각각의 YouTube 채널
- Google Takeout 시청기록(선택, `history` 명령에만 필요)

준비 절차는 [docs/setup.md](docs/setup.md)를 따른다.

## 설치

```bash
cargo install --path .
# 또는
scripts/build.sh --install ~/.local/bin
```

## 주요 명령

| 목적 | 명령 |
| --- | --- |
| 로그인/상태 | `stk auth login --account source`, `stk auth status` |
| 계획 확인 | `stk plan [--refresh] [--json]` |
| 실행 | `stk push <subscriptions\|playlists\|likes\|all> [--yes]`, `stk migrate --yes` |
| 진행/쿼터 | `stk status`, `stk quota show`, `stk quota sync --used <콘솔값>` |
| 시청기록 | `stk history import --takeout <zip>`, `stk browser history --port 9222 --yes` |
| 브라우저 세션 | `stk browser check --port 9222`, `stk browser reconcile --target-port 9222` |
| 정리 | `stk purge [--tokens] --yes` |

전체 옵션, 종료 코드, 쿼터·재개 규칙, 파일 위치는 [docs/usage.md](docs/usage.md)에 있다. 브라우저(CDP) 백엔드는 [docs/browser-mode.md](docs/browser-mode.md), 무인 반복 실행은 [docs/automation.md](docs/automation.md)를 따른다.

## 구조

| 경로 | 역할 |
| --- | --- |
| `src/cli.rs` | 명령 정의와 라우팅 |
| `src/auth/` | OAuth 로그인, 토큰 저장 |
| `src/youtube/` | API 클라이언트, 모델, 오류 매핑 |
| `src/migrate/` | 계획, 실행기, journal, 쿼터, 진행률 |
| `src/history/` | Takeout 파싱, 리포트, 플레이리스트 |
| `src/browser/` | CDP 클라이언트, 브라우저 기반 동기화 |
| `scripts/` | 빌드·설치, 브라우저 실행 도우미 |
| `tests/` | 종단간·HTTP 계약 테스트, 픽스처 |

## 문서

| 문서 | 내용 |
| --- | --- |
| [docs/usage.md](docs/usage.md) | 전체 명령, 종료 코드, 쿼터와 재개, 파일 위치, 환경 변수 |
| [docs/setup.md](docs/setup.md) | Google Cloud/OAuth 준비, 로그인, Takeout 내보내기, 문제 해결 |
| [docs/browser-mode.md](docs/browser-mode.md) | CDP 브라우저 세션 실행(준비·경계·한계·문제 해결) |
| [docs/services/README.md](docs/services/README.md) | 서비스별 문서 허브와 새 서비스 문서 규칙 |
| [docs/services/youtube.md](docs/services/youtube.md) | YouTube 전처리 체크리스트와 일일 실행 전략 |
| [docs/youtube-migration-requirements.md](docs/youtube-migration-requirements.md) | 이전 가능/불가능 범위, 쿼터 계산, 보관 정책, 오류 |
| [docs/automation.md](docs/automation.md) | cron/launchd 무인 반복 실행과 안전장치 |
| [AGENTS.md](AGENTS.md) | 저장소 운영 규칙(지침 정본, 브랜치·커밋·병합, 테스트, 보안) |

## 개발

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

테스트는 실제 네트워크를 쓰지 않는다. HTTP는 wiremock, Takeout은 `tests/fixtures/takeout/`의 픽스처를 사용하며, 모든 테스트는 임시 `STK_HOME`에서 실행된다.