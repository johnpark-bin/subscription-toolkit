# subscription-toolkit

구독 계정 관리 도구 모음. 단일 바이너리 `stk`로 제공하는 Rust CLI다.

첫 기능은 YouTube 계정 이전이다. 원본 계정의 구독, 플레이리스트와 항목, 좋아요를 대상 계정으로 복사하고, Google Takeout 시청기록을 분석해 리포트와 비공개 플레이리스트를 만든다. 원본 계정의 데이터는 삭제하지 않는다.

## 요구 사항

- Rust 1.96.1 이상 (2024 edition)
- Google Cloud 프로젝트의 OAuth 데스크톱 클라이언트 (`youtube.force-ssl` 스코프)
- 원본/대상 두 계정 각각의 YouTube 채널

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
stk push <subscriptions|playlists|likes|all> [--limit N] [--budget 9500] [--delay-ms 200] [--yes]

stk history import --takeout <dir|zip>
stk history report [--top 20] [--format markdown|json] [--out <path>]
stk history playlist [--name "시청기록 (이전)"] [--limit N] [--budget 9500] [--dry-run] [--yes]

stk purge [--tokens] [--yes]
```

자세한 준비 절차는 `docs/setup.md`, 이전 가능 범위와 쿼터 요구사항은 `docs/youtube-migration-requirements.md`를 참고한다.

## 문서

| 문서 | 내용 |
| --- | --- |
| `docs/setup.md` | Google Cloud/OAuth 클라이언트 준비, 로그인, Takeout 내보내기, 문제 해결 |
| `docs/youtube-migration-requirements.md` | 이전 가능/불가능 항목, 쿼터 계산, 보관 정책, 오류와 재개 |
| `AGENTS.md` | 저장소 운영 규칙(브랜치, 커밋, 테스트, 보안) |

## 개발

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```