# 브라우저 세션 실행(CDP)

`stk browser`는 OAuth API 대신 **사용자가 직접 로그인한 Chrome**에 DevTools Protocol(CDP)로 붙어, 동기화 요청을 페이지 컨텍스트에서 실행하는 별도 백엔드다. 기존 OAuth 경로(`stk plan`/`stk push`/`stk migrate`)는 그대로 동작하며 서로 독립적이다.

## 경계와 위험

- 이 모드는 YouTube 웹앱이 쓰는 내부 엔드포인트를 호출한다. YouTube 서비스 약관은 자동화된 접근을 금지하고, 공식 자동화 경로는 YouTube Data API(OAuth)다. **약관 위반과 계정 제재 위험은 사용자 몫**이며, 대상 계정에는 일회용 계정을 쓰는 편이 안전하다.
- 지문 위장, 자동화 탐지 우회, 사람처럼 보이는 행동 위장은 구현하지 않는다. 실제 Chrome에 그대로 붙어 요청만 자동화한다.
- 과도한 요청을 피하기 위한 페이싱만 넣었다. 액션 간격은 `--delay-ms`(기본 3000ms)에 `--jitter-ms`(기본 1500ms) 범위의 무작위 지터를 더해 요청이 일정 간격으로 몰리지 않게 한다. 429/5xx는 5s·10s·20s로 재시도한다. 이는 속도 제한 완화용이며, 지문·행동 위장은 하지 않는다.

## 준비

1. **별도 프로필**로 Chrome을 실행한다(평소 프로필과 섞지 않는다). 저장소의 도우미 스크립트를 쓰면 경로·포트·프로필이 고정된다.

```bash
scripts/browser-chrome.sh              # 포트 9222, 프로필 ~/.config/subscription-toolkit/chrome-target
PORT=9223 scripts/browser-chrome.sh    # 원본 계정용 등 다른 포트
```

직접 실행하려면:

```bash
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --remote-debugging-port=9222 \
  --user-data-dir="$HOME/.config/subscription-toolkit/chrome-target" \
  --no-first-run
```

2. 열린 Chrome에서 **대상 계정으로 직접 로그인**하고 탭을 하나 열어 둔다.
3. 같은 방식으로 원본 계정용 Chrome을 다른 포트(예: 9223)와 다른 프로필로 띄울 수 있다. `browser sync`에는 대상 포트만 필요하다.
4. 포트는 `127.0.0.1`에만 열린다. 외부에 노출하지 말고, 사용이 끝나면 Chrome을 종료한다.

## 확인

```bash
stk browser check --port 9222
stk browser check --port 9222 --expect-channel UCVxgiGLgVg7dHfz36aMRqRg
```

- 출력: `브라우저 확인: 채널 UC... · 로그인됨 · 세션 0`
- 로그인하지 않았으면 종료 코드 3과 함께 재로그인 안내가 나온다.
- `--expect-channel`을 주면 다른 계정에 로그인한 경우 중단한다.
- 채널 ID는 `studio.youtube.com`(채널 단위 페이지)에서 확인한 뒤, 요청 실행을 위해 `www.youtube.com`으로 돌아온다. 일반 시청 페이지의 `ytcfg`에는 `CHANNEL_ID`가 없기 때문이다.
- Electron 앱(예: Obsidian)의 DevTools 포트에 붙으면 "Electron 앱입니다"로 즉시 실패한다.

## 실행

계획은 기존 읽기 경로로 만든다(원본 데이터는 OAuth 또는 이후 Takeout 경로).

```bash
stk plan                          # 차이/예상 쿼터 계산 (API 읽기)
stk browser sync subscriptions --port 9222 --dry-run
stk browser sync subscriptions --port 9222 --limit 20 --yes
stk browser sync playlists    --port 9222 --delay-ms 3000 --jitter-ms 1500 --limit 50 --yes
stk browser sync playlists     --port 9222 --yes
stk browser sync likes         --port 9222 --limit 50 --yes
```

- 실행 전 브라우저 세션의 채널이 계획의 대상 채널과 같은지 자동 확인한다(`--skip-account-check`로 생략 가능, 권장하지 않음).
- 성공·건너뜀은 기존 journal(`data/journal.jsonl`)에 같은 키로 기록되므로, 이후 `stk status`와 `stk push`가 그대로 이어받는다. **한 번 실행한 액션은 다시 요청하지 않는다.**
- 플레이리스트 생성 응답의 `playlistId`를 즉시 매핑에 넣어, 같은 실행에서 항목 추가가 이어진다.
- 쿼터 장부(`data/quota.json`)는 쓰지 않는다. 브라우저 모드는 Google Cloud 쿼터를 소비하지 않는다.
- 진행률은 TTY에서 요약만 출력하고, 완료 후 `stk status`로 확인한다.

## 브라우저 기준 조정(reconcile)

API의 대상 상태는 브라우저로 만든 좋아요·구독을 늦게 반영하거나 일부만 반영한다. 이때 계획(`stk plan`) 기반 sync는 실제로는 이미 있는 항목을 다시 쓰려 하고, 빠진 항목은 놓친다. `browser reconcile`은 두 세션에서 직접 읽어 집합을 비교한다.

```bash
# 원본 목록은 저장된 계획(API로 읽은 스냅샷)을 기본으로 쓴다
stk browser reconcile --target-port 9222 --dry-run
stk browser reconcile --target-port 9222 --types subscriptions --yes

# 원본 브라우저 세션이 있으면 그쪽에서 직접 읽을 수도 있다
stk browser reconcile --source-port 9223 --target-port 9222 --dry-run
```

- `--source-port`를 생략하면 `stk plan --refresh`로 저장한 원본 스냅샷(구독·좋아요)을 쓴다. API 읽기만 필요하고, 원본 브라우저 로그인 없이 동작한다. 스냅샷이 없으면 사용법 오류로 중단한다.

- 좋아요는 `browse(VLLL)`, 구독은 `browse(FEchannels)`로 읽고, 빠진 항목만 `/like/like`·`/subscription/subscribe`로 추가한다.
- journal 키는 `like:{video_id}`, `subscription:{channel_id}`로 계획 기반 실행과 공유하되, **대상 실측을 우선**한다(이전 성공 기록이 있어도 대상에 없으면 다시 시도).
- 대상에만 있는 항목은 건드리지 않는다. 즉 결과는 "대상 ⊇ 원본"이다. 완전 동일로 만들려면 별도의 취소·해지 기능이 필요하다(현재 미지원).
- 읽기 한계: 좋아요 목록 헤더의 개수는 비공개·삭제 영상까지 포함하므로 실제 조회 가능한 수와 다르다(예: 832 표시 vs 721 조회).

## Watch Later(나중에 볼 동영상)

API와 Takeout에서 빠지는 항목이라 브라우저 세션으로만 동기화한다. 검증된 동작:

| 동작 | 요청 | 결과 |
| --- | --- | --- |
| 조회 | `browse(browseId=VLWL)` | `playlistVideoRenderer.videoId` 목록(continuation 지원) |
| 추가 | `browse/edit_playlist` + `playlistId=WL`, `ACTION_ADD_VIDEO` | `STATUS_SUCCEEDED` |
| 삭제 | `ACTION_REMOVE_VIDEO_BY_VIDEO_ID` | `STATUS_SUCCEEDED` |

```bash
# 원본 계정 Chrome(다른 포트/프로필)에서 로그인 후
stk browser watch-later export --port 9223
stk browser watch-later sync --port 9222 --dry-run
stk browser watch-later sync --port 9222 --yes
stk browser watch-later sync --port 9222 --prune --yes   # 대상 전용 항목 삭제
```

- `export`는 기본적으로 계획의 원본 채널과 세션이 일치하는지 확인한다. 다른 계정에서 내보내려면 `--expect-channel`을 지정한다.
- 추가는 원본의 오래된 항목부터 수행해 최신순 표시를 맞춘다. Watch Later에는 위치 지정 API가 없어 부분 동기화 후 순서는 완전히 같지 않을 수 있다.
- `--prune`은 원본에 없는 대상 항목을 삭제한다. 내보낸 파일이 비어 있으면 안전을 위해 실행하지 않는다.
- journal 키는 `watch-later:{video_id}`, 삭제는 `watch-later-remove:{video_id}`로 기록되어 재실행 시 중복 작업을 하지 않는다.

## 제약

- 원본 데이터 읽기는 아직 OAuth/Takeout 경로만 지원한다. 브라우저로 원본 구독/재생목록을 직접 읽는 기능은 없다.
- 시청기록은 API/브라우저로 옮길 수 없어 Takeout 기반(`stk history`)만 지원한다.
- Watch Later는 브라우저 모드에서만 지원한다(API·Takeout 불가).
- 재생 순서 보정은 하지 않는다. 계획 순서대로 추가만 하며, 재생목록 UI에서 수동 확인이 필요하다.
- 좋아요/구독의 UI 반영은 페이지 새로고침 후 확인된다.
- 페이지 구조나 내부 엔드포인트가 바뀌면 실패할 수 있다. 실패 시 journal에는 아무 것도 기록되지 않으므로, 원인을 고친 뒤 같은 명령을 다시 실행하면 된다.
- 생성(`playlist/create`)은 성공했지만 응답 해석 실패 등으로 journal에 기록되지 않을 수 있다. 이때는 `stk plan`을 다시 실행해 계획을 갱신하면, 제목이 같은 대상 플레이리스트를 재사용하므로 중복 생성되지 않는다(이 문서의 복구 절차).

## 문제 해결

| 증상 | 원인 | 해결 |
| --- | --- | --- |
| `DevTools 엔드포인트에 연결할 수 없습니다` | Chrome을 디버깅 포트로 띄우지 않음 | `--remote-debugging-port=<포트>`로 실행 |
| `이 포트는 Chrome이 아니라 Electron 앱입니다` | 다른 앱의 DevTools 포트 | Chrome 포트를 지정 |
| `웹 페이지 탭을 찾지 못했습니다` | 탭이 없음 | Chrome 창에 탭을 하나 연다 |
| `브라우저에 YouTube 로그인이 필요합니다` | 로그아웃 상태 | 해당 Chrome에서 로그인 |
| `브라우저 세션의 채널이 다릅니다` | 다른 계정 로그인 | 계정을 바꾸거나 다른 프로필 사용 |
| `요청이 제한되었습니다(HTTP 429)` | 요청 과다 | `--delay-ms`·`--jitter-ms`를 늘리고 시간을 두고 재실행 |
| `CDP 응답 시간 초과` | 페이지가 멈춤 | 탭을 새로고침하고 재실행 |

## 참고

- [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/)
- [YouTube 서비스 약관](https://www.youtube.com/t/terms)
- [YouTube API Services Developer Policies](https://developers.google.com/youtube/terms/developer-policies)