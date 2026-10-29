# AGENTS.md

이 문서는 `subscription-toolkit` 저장소에서 작업하는 모든 개발자와 에이전트의 규칙이다. 규칙과 충돌하는 요청은 이 문서를 근거로 먼저 확인하고, 사용자가 명시적으로 예외를 승인한 경우에만 따른다.

## 1. 프로젝트

- 구독 계정 관리 기능을 모아 두는 Rust CLI 툴킷이다.
- 첫 기능은 YouTube 계정 이전(구독, 플레이리스트, 좋아요, 시청기록 리포트)이다.
- 단일 바이너리 `stk`로 제공한다.
- 원본 계정의 데이터는 삭제하지 않는다. 모든 작업은 대상 계정으로의 복사다.

## 2. 기술 스택

- Rust 2024 edition, MSRV 1.96.1(`rust-toolchain.toml`).
- 비동기 런타임 tokio, HTTP reqwest(rustls), CLI clap derive.
- `#![forbid(unsafe_code)]`.
- 새 의존성은 (1) 표준/기존 의존성으로 해결 불가 (2) 유지보수 활발 (3) 라이선스 허용을 확인하고, 커밋 본문에 이유를 적는다.

## 3. 언어와 문서

- 모든 Markdown 문서, 커밋 메시지, 코드 주석, CLI 도움말, 사용자 출력은 한국어로 작성한다.
- 코드 식별자(타입/함수/변수/모듈), API 필드명, 로그의 기술 키는 영어를 유지한다.
- 오류 메시지는 한국어로 쓰되 원인 코드(`quotaExceeded` 등)는 그대로 포함한다.
- 동작이 바뀌면 같은 커밋에서 `README.md`와 `docs/`를 갱신한다.

## 4. 브랜치와 머지

- `main`은 항상 빌드와 테스트가 통과하는 상태로 유지한다.
- `main`에 직접 커밋하지 않는다. 예외: 저장소 최초 커밋.
- 브랜치 이름: `<type>/<kebab-slug>` (예: `feat/subscription-copy`, `fix/quota-ledger`).
- 기능 브랜치의 커밋은 작업 단위로 나누되, 각 커밋은 독립적으로 빌드와 테스트를 통과해야 한다.
- 머지는 **no-ff만** 사용한다. 기능 브랜치의 커밋 이력을 보존한다.

```bash
git switch main
git pull --ff-only
git merge --no-ff feat/subscription-copy -m "🔀 merge(subscriptions): 구독 복사 기능 병합"
git push origin main
git branch -d feat/subscription-copy
```

- `git merge --squash`, fast-forward 머지, 이미 push된 커밋의 rebase 재작성은 금지한다.
- 머지 커밋 메시지는 `🔀 merge(<scope>): <한국어 요약>` 형식이다.
- 머지 전 확인: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, 문서 갱신, 비밀정보 미포함.

## 5. 커밋 메시지 (gitmoji)

형식:

```text
<gitmoji> <type>(<scope>): <한국어 요약>

<본문: 왜/무엇을, 72자 줄바꿈>
Refs: #12
```

| gitmoji | type | 사용 시점 |
| --- | --- | --- |
| ✨ | feat | 새 기능 |
| 🐛 | fix | 버그 수정 |
| ♻️ | refactor | 동작 변화 없는 구조 개선 |
| ✅ | test | 테스트 추가/수정 |
| 📝 | docs | 문서 |
| 🔧 | chore | 빌드/설정/의존성 |
| ⚡️ | perf | 성능 |
| 🚨 | style | 포맷/린트 |
| 🔥 | remove | 코드/파일 삭제 |
| 🔐 | security | 보안 |
| 🎉 | init | 최초 커밋 |
| 🔀 | merge | 머지 커밋(main 전용) |
| 🚀 | release | 릴리스/버전 |

- 제목은 50자 이내, 마침표 없이 한국어로 쓴다.
- 하나의 커밋은 하나의 논리적 변경만 담는다. 리팩터링과 기능 추가를 섞지 않는다.
- 동작 변경 커밋에는 그 동작을 검증하는 회귀 테스트를 함께 넣는다.
- `WIP`, `fix`, `update` 같은 의미 없는 제목을 금지한다.
- scope는 `auth`, `youtube`, `plan`, `executor`, `subscriptions`, `playlists`, `likes`, `history`, `cli`, `config`, `docs`, `ci` 중에서 고른다.

예:

```text
✨ feat(subscriptions): 구독 복사 실행 구현

원본 계정의 구독을 읽어 대상 계정에 중복 없이 추가한다.
쿼터 소진 시 journal을 남기고 다음 실행에서 이어서 처리한다.
```

## 6. 코드 규칙

- 모듈 경계: `cli`(입출력), `auth`, `youtube`(HTTP), `migrate`(순수 도메인/실행), `history`(Takeout).
- 순수 로직은 I/O와 분리하고 단위 테스트한다.
- `unwrap`/`expect`/`panic!`은 테스트와 프로세스 시작 지점 외 금지한다.
- 오류는 `thiserror`로 정의하고 종료 코드를 문서화한다.
- stdout은 데이터/JSON, stderr는 진행률/로그/오류에 사용한다.
- 공개 항목에는 한국어 `///` 문서 주석을 단다.
- 시간은 주입 가능하게(`now: DateTime<Utc>`) 작성해 테스트를 결정적으로 만든다.
- 네트워크 호출은 `YouTubeApi` trait 뒤에 둔다. CLI에서 직접 HTTP를 호출하지 않는다.

## 7. 테스트

- 기능 단위 회귀 테스트를 원칙으로 한다. 버그 수정은 실패하는 테스트를 먼저 추가한다.
- 실제 네트워크를 사용하지 않는다. HTTP는 wiremock, Takeout은 픽스처를 쓴다.
- 시간/랜덤/환경에 의존하지 않는다. `STK_HOME`은 임시 디렉터리로 지정한다.
- 사용자가 관찰 가능한 동작(명령 출력, API 요청, 파일 상태)을 검증한다. 사설 함수 호출만 확인하는 테스트는 금지한다.
- 픽스처에는 개인정보를 넣지 않는다.
- 실행: `cargo test --all-features`.

## 8. 보안과 개인정보

- 실제 계정 이메일, `client_secret.json`, OAuth 토큰, 스냅샷, journal, Takeout 원본을 커밋하지 않는다. 문서/픽스처에는 이메일 플레이스홀더만 쓴다.
- 토큰 파일은 0600, 디렉터리는 0700으로 만든다.
- 로그에 토큰, 이메일, 전체 API 응답을 남기지 않는다.
- YouTube API 데이터는 최대 30일 보관 후 삭제하거나 갱신한다. `stk purge`로 즉시 삭제할 수 있어야 한다.
- API 키와 토큰을 코드에 하드코딩하지 않는다.

## 9. 디렉터리 구조

```text
src/cli.rs          명령 정의와 라우팅
src/auth/           OAuth 로그인, 토큰 저장
src/youtube/        API 클라이언트, 모델, 오류 매핑
src/migrate/        계획 계산, 실행, journal, 쿼터, 진행률
src/history/        Takeout 파싱, 리포트, 플레이리스트
tests/              종단간/HTTP 계약 테스트, 픽스처
docs/               설치/요구사항 문서
```

## 10. 자주 쓰는 명령

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo run -- --help
```