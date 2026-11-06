# AGENTS.md

이 문서는 `subscription-toolkit` 저장소에서 작업하는 모든 코딩 에이전트와 개발자가 공유하는 **지침 정본**이다. 규칙과 충돌하는 요청은 이 문서를 근거로 먼저 확인하고, 사용자가 명시적으로 예외를 승인한 경우에만 따른다.

## 1. 지침 정본

- `AGENTS.md`가 유일한 편집 정본이다. 규칙은 여기서만 고친다.
- `CLAUDE.md`는 같은 디렉터리의 `AGENTS.md`를 가리키는 **상대 심링크**다(`CLAUDE.md -> AGENTS.md`). 별도 내용을 두지 않는다. 심링크를 쓸 수 없는 환경에서는 `CLAUDE.md`를 두지 않는다.
- `CLAUDE.md`에만 있던 유효한 지침은 이 문서로 통합하고, 중복·충돌은 이 문서 기준으로 해소한다.
- 하위 디렉터리에 별도 범위의 지침(예: `some/dir/AGENTS.md`)이 있으면 그 범위는 보존하고, 같은 원칙(정본 1개, 심링크, 중복 금지)을 적용한다. 현재 이 저장소에는 루트 외 지침 파일이 없다.

## 2. 프로젝트

- 구독 계정 관리 기능을 모아 두는 Rust CLI 툴킷이다.
- 첫 기능은 YouTube 계정 이전(구독, 플레이리스트, 좋아요, 시청기록 리포트)이다.
- 단일 바이너리 `stk`로 제공한다.
- 원본 계정의 데이터는 삭제하지 않는다. 모든 작업은 대상 계정으로의 복사다.
- OAuth API 경로(`plan`/`push`/`migrate`)와 브라우저 세션 경로(`browser ...`)를 별도 배선으로 유지한다.

## 3. 기술 스택

- Rust 2024 edition, MSRV 1.96.1(`rust-toolchain.toml`).
- 비동기 런타임 tokio, HTTP reqwest(rustls), CLI clap derive, CDP는 `tokio-tungstenite`.
- `#![forbid(unsafe_code)]`(`src/lib.rs`).
- 새 의존성은 (1) 표준/기존 의존성으로 해결 불가 (2) 유지보수 활발 (3) 라이선스 허용을 확인하고, 커밋 본문에 이유를 적는다.

## 4. 언어와 문서

- 직접 작성하는 Markdown, 코드 주석, CLI 도움말, 사용자 표시 문구, 커밋·PR 제목과 본문은 한국어로 쓴다.
- 명령어·식별자·경로·API 필드·제품명은 원문을 유지한다. 자동 생성물과 외부 라이선스·고지는 원문 그대로 보존한다.
- 오류 메시지는 한국어로 쓰되 원인 코드(`quotaExceeded`, `failedPrecondition` 등)는 그대로 포함한다.
- `README.md`는 프로젝트 소개와 매뉴얼의 진입점이다. 설치, 주요 명령, 구조, 운영, 문제 해결로 안내한다.
- `README.md`가 약 150줄을 넘거나 주제가 세분화되면 `docs/<주제>.md`로 분리하고, README에는 한 줄 설명과 상대 링크만 남긴다. 현재 분리 문서: `docs/usage.md`(명령·종료 코드·쿼터·파일 위치), `docs/setup.md`, `docs/browser-mode.md`, `docs/automation.md`, `docs/youtube-migration-requirements.md`, `docs/services/`.
- 사용자 동작이나 설치 절차가 바뀌면 같은 작업에서 관련 매뉴얼(`README.md`, `docs/usage.md`, `docs/setup.md`, `docs/services/youtube.md`)을 갱신한다.
- 서비스별 문서는 `docs/services/<service>.md`에 두고 `docs/services/README.md` 허브에서 링크한다. 새 서비스는 전처리, 이전 범위/한도, 인증, 쿼터, 오류·재개, 수용 기준 6개 절을 채운다.
- 공통 절차(설치, 보관 정책, 자동화)는 서비스 문서에 복사하지 않고 `docs/setup.md`, `docs/automation.md`를 링크한다.
- 프로젝트가 직접 관리하는 `CHANGELOG.md`와 분기·버전별 변경 이력 문서는 두지 않는다. 필요한 운영 지침은 매뉴얼로 옮기고, 변경 근거는 Git 이력·이슈·PR로 관리한다. 참조 링크나 생성 설정이 남아 있으면 함께 정리한다.
- 외부 의존성·라이선스·자동 생성물을 일괄 삭제하지 않는다. `OKF`처럼 의미가 불명확한 대상은 의미와 파일 범위를 확인한 뒤 처리하고, 불명확하면 사용자에게 대상을 확인한다.

## 5. 브랜치와 병합

- 작업 브랜치 이름은 `<type>/YYYY-MM-DD-<slug>`를 쓴다. 예: `feat/2026-11-06-browser-history`, `fix/2026-11-06-quota-ledger`.
- 병합 단위는 기능별 분류가 아니라 **사용자가 병합을 요청한 시점의 작업 범위**로 정한다.
- 개발 중 커밋은 gitmoji 유형별로 분리한다(6장). 병합 요청 시점에 개발 내용을 확인해 포함 범위를 확정하고, 그 범위의 커밋을 gitmoji 유형별로 정리한다.
- 여러 기능이 섞여 있어도 기능별 브랜치나 개별 커밋용 no-ff 병합으로 나누지 않는다. 요청 범위 밖의 사용자 변경과 진행 중인 다른 작업은 제외한다.
- 대상 작업이 여러 브랜치·worktree에 흩어져 있으면 요청 범위의 변경을 **하나의 통합 브랜치**에 모아 검증하고, default branch에는 한 번만 병합한다.
- 병합 전 순서: 원격 default branch 확인 → `git fetch` → 작업 브랜치 rebase → 충돌 해결 → 회귀 테스트와 필수 검사(12장) 실행.
- 병합 직전, 확정 범위의 변경 커밋들을 **단일 변경 커밋**으로 통합한 뒤 default branch에 `git merge --no-ff`로 한 번 병합한다.
- 커밋 통합은 깨끗한 작업 트리와 복구용 참조(예: 백업 브랜치·태그)를 확보한 뒤, merge-base 기준 `git reset --soft`와 재커밋으로 수행한다. 대표 gitmoji를 쓰고 본문에 유형별 변경을 요약한다.
- 결과 히스토리는 단일 변경 커밋과 이를 감싸는 merge 커밋으로 구성한다. merge 메시지는 Git 기본값을 유지한다.
- 금지: 개별 커밋마다 no-ff 병합, fast-forward 병합, merge 마커 없는 `git merge --squash`, default branch 직접 커밋, force push.
- `git switch <default> && git pull --ff-only`로 최신화했을 때 rebase 기준이 달라지면 rebase와 검증을 처음부터 반복한다.

## 6. 커밋 메시지

형식:

```text
<gitmoji> <한국어 요약>

<본문: 변경 이유와 영향>
```

| gitmoji | 사용 시점 |
| --- | --- |
| ✨ | 기능 |
| 🐛 | 수정 |
| ♻️ | 리팩토링 |
| 📝 | 문서 |
| ⬆️ | 의존성 |
| ⚡ | 성능 |
| ✅ | 테스트 |
| 👷 | 빌드·CI |
| 🔥 | 삭제 |
| 🎨 | 스타일 |
| 🔒 | 보안 |
| 🔧 | 설정 |

- 제목은 `<gitmoji> <한국어 요약>` 형태로 50자 이내, 마침표 없이 쓴다. `type(scope)` 접두사는 쓰지 않는다.
- 본문에는 변경 이유를 적는다. 동작 변경이면 영향 범위와 검증 방법을 함께 남긴다.
- 하나의 커밋은 하나의 gitmoji 유형에 해당하는 논리적 변경만 담는다. 리팩터링과 기능 추가를 섞지 않는다.
- `WIP`, `fix`, `update` 같은 의미 없는 제목을 금지한다.
- 병합 시 통합 커밋의 대표 gitmoji는 범위의 주된 변경 유형을 따르고, 본문에 유형별 요약을 적는다.

## 7. 완료된 worktree 정리

- 병합이 끝나면 해당 작업에 사용한 worktree의 변경이 default branch에 모두 반영됐는지 확인한다.
- 제거 전 미커밋 변경, 미추적 파일, 진행 중인 작업 여부를 확인한다. 보존할 내용이 있으면 남기고 이유를 보고한다.
- 제거 대상 밖의 worktree에서 `git worktree remove <path>`로 정상 제거하고, `git worktree list`로 남은 등록 상태를 확인한다. 기본 작업 디렉터리와 다른 진행 중 worktree는 유지한다.
- 강제 제거로 미반영 변경을 버리지 않는다. 정리한 worktree와 남겨 둔 대상을 완료 보고에 포함한다.

## 8. 코드 규칙

- 모듈 경계: `cli`(입출력·라우팅), `auth`(OAuth·토큰), `youtube`(HTTP·모델·오류 매핑), `migrate`(순수 계획·실행·journal·쿼터), `history`(Takeout), `browser`(CDP·브라우저 실행).
- 순수 로직은 I/O와 분리하고 단위 테스트한다.
- `unwrap`/`expect`/`panic!`은 테스트와 프로세스 시작 지점 외 금지한다.
- 오류는 `thiserror`로 정의하고 종료 코드를 `docs/usage.md`에 문서화한다.
- stdout은 데이터/JSON, stderr는 진행률·로그·오류에 쓴다.
- 공개 항목에는 한국어 `///` 문서 주석을 단다.
- 시간은 주입 가능하게(`now: DateTime<Utc>`) 작성해 테스트를 결정적으로 만든다.
- 네트워크 호출은 `YouTubeApi` trait 뒤에 두고, 브라우저 실행은 `browser` 모듈의 CDP 계층 뒤에 둔다. CLI에서 직접 HTTP나 CDP를 호출하지 않는다.
- 브라우저 경로는 탐지 회피(지문 위장, 자동화 은닉)를 넣지 않는다. 페이싱·재시도만 다룬다.

## 9. 테스트

- 기능 변경에는 그 기능을 검증하는 **기능 단위 회귀 테스트**를 함께 추가한다. 정상 흐름, 경계값, 실패 조건, 상태 전이를 검증한다.
- 버그 수정은 재현 테스트의 실패를 먼저 확인한 뒤 수정하고, 통과를 확인한다.
- 구현을 그대로 복사한 테스트, mock 출력만 확인하는 테스트, 단순 not-throw 검사는 회귀 검증으로 인정하지 않는다. 사용자가 관찰 가능한 동작(명령 출력, API 요청, 파일 상태)을 검증한다.
- 실제 네트워크를 쓰지 않는다. HTTP는 wiremock(`tests/http_contract.rs`), CDP는 가짜 WebSocket 서버(`tests/browser_cdp.rs`), Takeout은 `tests/fixtures/takeout/` 픽스처를 쓴다.
- 시간/랜덤/환경에 의존하지 않는다. `STK_HOME`은 임시 디렉터리로 지정한다.
- 픽스처에는 개인정보를 넣지 않는다.
- 이 저장소에서 실제로 확인한 검사 명령은 12장에 있다. 변경과 관련된 테스트와 필수 검사를 실행하고, 실행하지 못한 검사는 이유를 명시한다.

## 10. 보안과 개인정보

- 실제 계정 이메일, `client_secret.json`, OAuth 토큰, 스냅샷, journal, Takeout 원본, `takeout/` 내용을 커밋하지 않는다. 문서/픽스처에는 이메일 플레이스홀더만 쓴다.
- 토큰 파일은 `0600`, 디렉터리는 `0700`으로 만든다. `client_secret.json`도 읽을 때 `0600`으로 맞춘다.
- 로그에 토큰, 이메일, 전체 API 응답을 남기지 않는다. 응답 본문은 필요한 만큼만 잘라 표시한다.
- YouTube API 데이터는 최대 30일 보관 후 삭제하거나 갱신한다. `stk purge`로 즉시 삭제할 수 있어야 한다.
- API 키와 토큰을 코드에 하드코딩하지 않는다.
- CDP 디버깅 포트는 로컬(`127.0.0.1`)만 사용하고, 작업이 끝나면 브라우저를 종료한다.

## 11. 디렉터리 구조

```text
src/cli.rs          명령 정의와 라우팅
src/auth/           OAuth 로그인, 토큰 저장
src/youtube/        API 클라이언트, 모델, 오류 매핑
src/migrate/        계획 계산, 실행기, journal, 쿼터, 진행률, 상태
src/history/        Takeout(JSON·HTML) 파싱, 리포트, 플레이리스트
src/browser/        CDP 클라이언트, 페이지 스크립트, 브라우저 실행 경로
scripts/            build.sh(빌드·설치), browser-chrome.sh(CDP용 Chrome 실행)
tests/              종단간/HTTP/CDP 계약 테스트, 픽스처
docs/               setup, usage, browser-mode, automation, 요구사항
docs/services/      서비스별 문서 허브와 규칙
takeout/            Takeout 원본 보관(커밋 금지, .gitignore)
```

## 12. 자주 쓰는 검사 명령

아래는 이 저장소에서 실제로 실행해 통과를 확인한 명령이다.

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features

scripts/build.sh --check          # 위 세 검사 후 release 빌드
scripts/build.sh --check --install
```

- 문서만 바꾼 작업도 링크 유효성(상대 경로 존재)을 확인하고, 코드에 영향이 있으면 위 검사를 실행한다.
- 브라우저 경로를 바꾼 작업은 `stk browser check --port <포트>`로 실제 세션 확인을 함께 한다(로그인된 Chrome이 필요하며, 없으면 미실행 사유를 보고한다).