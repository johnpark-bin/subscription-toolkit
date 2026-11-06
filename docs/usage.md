# 사용법과 운영

`stk` 명령의 전체 목록, 출력 규칙, 종료 코드, 쿼터·재개 동작, 데이터 파일 위치를 정리한다. 준비 절차는 [setup.md](setup.md), 브라우저 세션 실행은 [browser-mode.md](browser-mode.md), 이전 범위와 제약은 [youtube-migration-requirements.md](youtube-migration-requirements.md)를 따른다.

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
stk browser playlists --port 9222 [--dry-run] [--yes]
stk browser history --port 9222 [--name "시청기록 (이전)"] [--limit N] [--dry-run] [--yes]
stk browser watch-later export --port 9223 [--expect-channel <id>] [--out <path>]
stk browser watch-later sync --port 9222 [--limit N] [--prune] [--dry-run] [--yes]

stk history import --takeout <dir|zip>
stk history report [--top 20] [--format markdown|json] [--out <path>]
stk history playlist [--name "시청기록 (이전)"] [--limit N] [--budget 9500] [--dry-run] [--yes]

stk purge [--tokens] [--yes]
```

- `plan`: 원본 스냅샷(24시간 캐시)과 대상 현재 상태를 읽어 차이를 계산하고 `data/plan.json`에 저장한다. 쓰기는 하지 않는다.
- `push`: 실행 직전에 대상 상태를 다시 읽어 액션을 재계산한 뒤 쓰기를 실행한다. `plan.json`의 원본 스냅샷이 24시간 이내면 재사용하고, 아니면 원본을 다시 읽는다. 원본 변경을 즉시 반영하려면 `stk plan --refresh` 후 `stk push`를 실행한다.
- `push all` 순서: 구독 → 플레이리스트 → 좋아요. 시청기록은 Takeout이 필요하므로 `history playlist`나 `browser history`로 따로 실행한다.
- `migrate`: 계획 → 확인 → 구독 → 플레이리스트 → 좋아요 실행을 한 번에 처리한다. 매일 반복할 때 쓰는 기본 명령이다.
- `status`: 저장된 계획과 journal을 비교해 유형별 완료/남은 수, 남은 쿼터, 예상 일수를 보여준다.
- `quota`: 오늘 장부를 보여주고(`show`), Google Cloud 콘솔 실측값으로 맞춘다(`sync`).
- `browser`: OAuth API 대신 사용자가 로그인한 Chrome에 CDP로 붙어 동기화를 실행하는 별도 백엔드다. `scripts/browser-chrome.sh`로 전용 프로필을 띄운 뒤 로그인해서 쓴다. 준비와 제약은 [browser-mode.md](browser-mode.md)를 따른다.
- `browser reconcile`: 브라우저 실측으로 좋아요·구독 집합을 비교해 빠진 항목만 채운다. API의 대상 상태가 늦게 반영될 때 쓴다.
- `browser playlists`: 원본 재생목록(계획 스냅샷)과 대상 제목을 비교해 없는 목록만 만든다.
- `browser history`: Takeout 시청기록을 비공개 재생목록으로 재현한다(최신순, 최대 5,000개).
- `browser watch-later`: API·Takeout에 없는 Watch Later를 브라우저로 내보내고 병합한다.
- `history playlist`: API 경로로 시청기록을 비공개 재생목록으로 만든다(항목당 50 units). 대량은 `browser history`가 낫다.

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

- YouTube Data API 기본 할당량은 하루 10,000 units이며 태평양 시간 자정에 초기화된다.
- 쓰기 요청은 액션당 50 units(`subscriptions.insert`, `playlists.insert`, `playlistItems.insert`, `videos.rate`, 위치 보정 `playlistItems.update`), 목록 조회는 페이지당 1 unit이다.
- `stk`는 `--budget`(기본 9,500)만큼만 쓰기를 예약하고, 남은 500 units는 읽기 버퍼로 남겨 둔다. 예산이 부족하면 종료 코드 4로 멈춘다.
- 성공·건너뜀은 `data/journal.jsonl`에 즉시 기록된다. 같은 명령을 다시 실행하면 완료된 액션은 건너뛰고 이어서 진행한다. 실패한 액션은 재시도 대상이다.
- `--limit N`은 데이터 유형별 상한이다. 한 유형이 상한에 도달해도 다른 유형은 계속 진행한다.
- `stk quota show`로 오늘 사용량/예산/단가/남은 쓰기 수를 확인한다. 콘솔 값과 장부가 다르면 `stk quota sync --used <콘솔값> [--write-cost <단가>]`로 맞춘다. `--write-cost`는 이후 실행에 저장된다.
- `--budget`을 생략하면 장부에 저장된 예산(없으면 9,500)을 쓴다.
- 브라우저(CDP) 경로는 Google Cloud 쿼터를 쓰지 않는다. 다만 대상 계정에는 좋아요·구독 반영 일일 상한이 관측된다.

무인 반복 실행은 [automation.md](automation.md)를 따른다.

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

## 테스트/개발 전용 환경 변수

실제 사용에서는 설정하지 않는다.

| 변수 | 의미 |
| --- | --- |
| `STK_HOME` | 설정·데이터 루트 재정의 |
| `STK_API_BASE` | API 주소(기본 `https://www.googleapis.com/youtube/v3`) |
| `STK_ACCESS_TOKEN` | 모든 계정에 쓸 고정 액세스 토큰 |
| `STK_SOURCE_ACCESS_TOKEN` | 원본 계정 고정 토큰(`STK_ACCESS_TOKEN`보다 우선) |
| `STK_TARGET_ACCESS_TOKEN` | 대상 계정 고정 토큰(`STK_ACCESS_TOKEN`보다 우선) |
| `STK_RETRY_BASE_MS` | 재시도 기본 지연(ms, 기본 1000) |

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