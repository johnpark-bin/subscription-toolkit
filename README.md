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
cargo build --release   # target/release/stk
```

## 명령 요약

```text
stk auth login --account <source|target> [--client-secret <path>] [--login-hint <email>] [--no-browser]
stk auth status [--json]
stk auth logout --account <source|target>

stk plan [--types subscriptions,playlists,likes] [--refresh] [--json]
stk push <subscriptions|playlists|likes|all> [--limit N] [--budget N] [--delay-ms 200] [--yes]
stk migrate [--limit N] [--budget N] [--delay-ms 200] [--refresh] [--yes]

stk status [--json]
stk quota show [--json]
stk quota sync --used <콘솔 사용량> [--budget N] [--write-cost N]

stk browser check --port 9222
stk browser sync <subscriptions|playlists|likes|all> --port 9222 [--limit N] [--delay-ms 3000] [--jitter-ms 1500] [--dry-run] [--yes]
stk browser reconcile [--source-port 9223] [--target-port 9222] [--types likes,subscriptions] [--via cdp|api] [--budget N] [--dry-run] [--yes]
stk browser watch-later export --port 9223 [--expect-channel <id>] [--out <path>]
stk browser watch-later sync --port 9222 [--limit N] [--prune] [--dry-run] [--yes]

stk history import --takeout <dir|zip>
stk history report [--top 20] [--format markdown|json] [--out <path>]
stk history playlist [--name "시청기록 (이전)"] [--limit N] [--budget 9500] [--dry-run] [--yes]

stk purge [--tokens] [--yes]
```

- `plan`: 원본 스냅샷(24시간 캐시)과 대상 현재 상태를 읽어 차이를 계산하고 `data/plan.json`에 저장한다. 쓰기는 하지 않는다.
- `push`: 실행 직전에 대상 상태를 다시 읽어 액션을 재계산한 뒤 쓰기를 실행한다. `plan.json`의 원본 스냅샷이 24시간 이내면 재사용하고, 아니면 원본을 다시 읽는다. 원본 변경을 즉시 반영하려면 `stk plan --refresh` 후 `stk push`를 실행한다.
- `push all` 순서: 구독 → 플레이리스트 → 좋아요. 시청기록은 Takeout이 필요하므로 `history playlist`로 따로 실행한다.
- `migrate`: 계획 → 확인 → 구독 → 플레이리스트 → 좋아요 실행을 한 번에 처리한다. 매일 반복할 때 쓰는 기본 명령이다.
- `status`: 저장된 계획과 journal을 비교해 유형별 완료/남은 수, 남은 쿼터, 예상 일수를 보여준다.
- `browser`: OAuth API 대신 사용자가 로그인한 Chrome에 CDP로 붙어 동기화를 실행하는 별도 백엔드다. 실행은 `scripts/browser-chrome.sh`로 전용 프로필을 띄운 뒤 로그인해서 쓴다. Watch Later는 API·Takeout으로 옮길 수 없어 이 경로(`browser watch-later`)로만 동기화한다. 계획·journal만 공유하며, 준비와 제약은 [docs/browser-mode.md](docs/browser-mode.md)를 따른다.
- `history playlist`: 최신 시청 순, 고유 videoId, 최대 5,000개, 비공개 플레이리스트. 같은 이름의 플레이리스트가 대상에 있으면 재사용하고 기존 항목은 건너뛴다.

쓰기 전에는 건수와 예상 쿼터를 보여주고 `yes` 입력을 받는다. `--yes`로 생략할 수 있으며, 표준 입력이 TTY가 아니면 `--yes`가 필수다(없으면 종료 코드 2).

## 출력과 종료 코드

stdout은 데이터/JSON, stderr는 진행률·로그·오류에 쓴다. 진행률은 TTY에서 유형별 진행 바(`구독 12/410`)로, TTY가 아니면 유형 시작/종료와 실패만 출력한다.

| 코드 | 의미 |
| --- | --- |
| 0 | 성공(건너뜀 포함) |
| 1 | 일반 오류 |
| 2 | 사용법 오류 |
| 3 | 인증 또는 채널 문제 |
| 4 | 쿼터 예산 소진 |
| 5 | 부분 실패 또는 재시도 필요(최근 구독 과다 등) |

## 쿼터와 재개

- `stk quota show`로 오늘 사용량/예산/단가/남은 쓰기 수를 확인한다.
- Google Cloud 콘솔의 **YouTube Data API v3 → Queries per day** 값과 장부가 다르면 `stk quota sync --used <콘솔값> [--write-cost <단가>]`로 맞춘다. `--write-cost`는 이후 실행에 저장되어 예산 계산에 계속 쓰인다.
- `--budget`을 생략하면 장부에 저장된 예산(없으면 9,500)을 쓴다.

- YouTube Data API 기본 할당량은 하루 10,000 units이며 태평양 시간 자정에 초기화된다.
- 쓰기 요청은 액션당 50 units(`subscriptions.insert`, `playlists.insert`, `playlistItems.insert`, `videos.rate`, 위치 보정 `playlistItems.update`), 목록 조회는 페이지당 1 unit이다.
- `stk`는 `--budget`(기본 9,500)만큼만 쓰기를 예약하고, 남은 500 units는 읽기 버퍼로 남겨 둔다. 예산이 부족하면 종료 코드 4로 멈춘다.
- 성공·건너뜀은 `data/journal.jsonl`에 즉시 기록된다. 같은 명령을 다시 실행하면 완료된 액션은 건너뛰고 이어서 진행한다. 실패한 액션은 재시도 대상이다.
- `--limit N`은 데이터 유형별 상한이다. 한 유형이 상한에 도달해도 다른 유형은 계속 진행한다.

## 개인정보와 데이터 보관

- OAuth 클라이언트 설정, 토큰, 스냅샷, journal, Takeout 원본은 저장소에 커밋하지 않는다.
- 토큰 파일은 `0600`, 디렉터리는 `0700`으로 만든다.
- API 데이터(스냅샷, 계획, journal, 쿼터 장부, 시청기록)는 최대 30일 보관하며, 30일이 지난 스냅샷·계획·journal 항목은 자동으로 정리된다.
- `stk purge`로 API 데이터를 즉시 삭제하고, `stk purge --tokens`로 토큰도 삭제한다.

## 파일 위치

기본 루트는 `~/.config/subscription-toolkit`이며 `STK_HOME`으로 덮어쓸 수 있다.

```text
client_secret.json         OAuth 데스크톱 클라이언트 설정
tokens/source.json         원본 계정 토큰(0600)
tokens/target.json         대상 계정 토큰(0600)
data/source/snapshot.json  원본 스냅샷(24시간 캐시)
data/history.json          정규화한 시청기록
data/watch-later.json      브라우저로 내보낸 Watch Later 목록
data/plan.json             이전 계획
data/journal.jsonl         실행 journal(재개용)
data/quota.json            태평양 날짜 기준 쿼터 장부
data/.lock                 동시 실행 방지 잠금
```

### 테스트/개발 전용 환경 변수

실제 사용에서는 설정하지 않는다.

| 변수 | 의미 |
| --- | --- |
| `STK_HOME` | 설정·데이터 루트 재정의 |
| `STK_API_BASE` | API 주소(기본 `https://www.googleapis.com/youtube/v3`) |
| `STK_ACCESS_TOKEN` | 모든 계정에 쓸 고정 액세스 토큰 |
| `STK_SOURCE_ACCESS_TOKEN` | 원본 계정 고정 토큰(`STK_ACCESS_TOKEN`보다 우선) |
| `STK_TARGET_ACCESS_TOKEN` | 대상 계정 고정 토큰(`STK_ACCESS_TOKEN`보다 우선) |
| `STK_RETRY_BASE_MS` | 재시도 기본 지연(ms, 기본 1000) |

## 문서

| 문서 | 내용 |
| --- | --- |
| [docs/services/README.md](docs/services/README.md) | 서비스별 문서 허브(현재 YouTube, 이후 서비스 추가 규칙) |
| [docs/services/youtube.md](docs/services/youtube.md) | YouTube 전처리 체크리스트와 일일 실행 전략 |
| [docs/setup.md](docs/setup.md) | Google Cloud/OAuth 클라이언트 준비, 로그인, Takeout 내보내기, 문제 해결 |
| [docs/youtube-migration-requirements.md](docs/youtube-migration-requirements.md) | 이전 가능/불가능 항목, 쿼터 계산, 보관 정책, 오류와 재개 |
| [docs/automation.md](docs/automation.md) | cron/launchd로 매일 무인 실행하는 방법과 안전장치 |
| [docs/browser-mode.md](docs/browser-mode.md) | CDP 브라우저 세션으로 실행하는 별도 백엔드(준비·경계·문제 해결) |
| [AGENTS.md](AGENTS.md) | 저장소 운영 규칙(브랜치, 커밋, 테스트, 보안) |

## 빌드와 설치

```bash
scripts/build.sh                        # release 빌드
scripts/build.sh --check                # fmt/clippy/test 검증 후 빌드
scripts/build.sh --check --install      # 검증 후 ~/.local/bin/stk 설치
scripts/build.sh --install /usr/local/bin
scripts/build.sh --debug                # debug 프로필 빌드
scripts/build.sh --clean                # target 정리 후 빌드
```

`--install`은 바이너리를 `0755`로 복사하고 PATH 안내와 스케줄러(launchd/cron)에 넣을 절대 경로를 출력한다.

## 개발

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

테스트는 실제 네트워크를 쓰지 않는다. HTTP는 wiremock, Takeout은 `tests/fixtures/takeout/watch-history.json`을 사용하며, 모든 테스트는 임시 `STK_HOME`에서 실행된다.