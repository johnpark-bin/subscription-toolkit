# YouTube 계정 이전 운영 문서

YouTube 계정 이전을 실제로 실행하는 순서와 운영 요령을 정리한다. 준비 절차의 자세한 단계는 [준비 절차](../setup.md), 범위·쿼터·오류 정책의 근거는 [이전 요구사항](../youtube-migration-requirements.md)을 따른다. 이 문서는 두 문서와 중복되는 설명을 줄이고 실행에 필요한 것만 모은다.

## 1. 전처리 체크리스트

순서대로 확인한다. 각 항목의 자세한 단계는 [준비 절차](../setup.md)에 있다.

- [ ] Google Cloud 프로젝트를 만든다.
- [ ] YouTube Data API v3를 사용 설정한다.
- [ ] OAuth 동의 화면을 구성한다(User Type External, 스코프 `youtube.force-ssl`, 테스트 사용자 2명(원본·대상 계정) 등록, 가능하면 In production 전환).
- [ ] 데스크톱 앱 OAuth 클라이언트 JSON을 `~/.config/subscription-toolkit/client_secret.json`에 배치하고 권한을 `0600`으로 둔다. 다른 경로를 쓰면 `--client-secret <경로>`로 지정한다.
- [ ] 두 계정 모두 YouTube 채널을 가지고 있는지 확인한다. 브랜드 계정이거나 채널이 여러 개면 동의 화면에서 이전에 쓸 채널을 직접 선택한다.
- [ ] 두 계정에 로그인한다.

```bash
stk auth login --account source --login-hint '<원본 계정 이메일>'
stk auth login --account target --login-hint '<대상 계정 이메일>'
```

- [ ] 로그인 결과를 확인한다.

```bash
stk auth status
stk auth status --json
```

- [ ] (선택) 시청기록을 이전하려면 Google Takeout에서 `watch-history.json`을 내려받는다. 내보내기 절차는 [준비 절차](../setup.md#8-takeout-시청기록-내보내기)를 따른다.
- [ ] 쿼터 시작 상태를 확인한다.

```bash
stk quota show
```

## 2. 이전 범위

| 항목 | 가능 여부 | 이유 |
| --- | --- | --- |
| 구독 | 가능 | `subscriptions.insert`로 복사하고 중복은 건너뛴다 |
| 플레이리스트와 항목 | 가능 | `playlists.insert` + `playlistItems.insert`로 복사한다(항목 5,000개 상한) |
| 좋아요 | 부분 | API 조회가 최근 1,000개까지만 제공되므로 그 범위만 복사된다 |
| 시청기록 | 부분 | API 읽기/쓰기가 없어 Takeout 리포트와 비공개 플레이리스트 재현으로만 대체한다 |
| Watch Later(나중에 볼 동영상) | 브라우저 모드만 | API·Takeout 불가. `stk browser watch-later`로 원본을 내보내 대상에 반영한다(API 쿼터 없음) |
| 업로드 영상 | 불가 | 채널 소유 자산이라 계정 사이를 옮기는 API가 없다 |
| 댓글 | 불가 | 작성자 계정에 귀속되며 대량 쓰기 API가 없다 |
| 멤버십 | 불가 | 계정 종속 상품이라 이전할 수 없다 |
| 구매·대여 | 불가 | 계정 종속 상품이라 이전할 수 없다 |
| 알림·추천 설정 | 불가 | 이를 읽고 쓰는 API가 제공되지 않는다 |

시청기록 자체를 대상 계정에 주입하는 방법은 없으며, `stk history playlist`가 만드는 비공개 플레이리스트가 대체 수단이다.

## 3. 실행 전략

기본 흐름은 하루 예산만큼 실행하고 다음 날 이어서 한다.

① 차이와 예상 쿼터를 확인한다(쓰기 없음).

```bash
stk plan
stk plan --json
stk plan --refresh   # 원본 변경을 즉시 반영하고 싶을 때
```

② 하루 예산만큼 실행한다. 전체를 한 번에 하면 구독 → 플레이리스트 → 좋아요 순으로 진행된다.

```bash
stk migrate --yes --budget 9500
```

유형별로 나누어 실행할 수도 있다(시청기록 제외).

```bash
stk push subscriptions --yes --budget 9500
stk push playlists --yes --budget 9500
stk push likes --yes --budget 9500
```

시청기록은 Takeout 가져오기가 먼저 필요하므로 별도로 실행한다.

```bash
stk history import --takeout <디렉터리_또는_zip_경로>
stk history report --top 20
stk history playlist --dry-run
stk history playlist --yes --budget 9500
```

쓰기 전에는 건수와 예상 쿼터를 보여주고 `yes` 입력을 받는다. `--yes`로 생략할 수 있고, 표준 입력이 TTY가 아니면 `--yes`가 필수다(없으면 종료 코드 2).

③ 진행률을 확인한다.

```bash
stk status
stk status --json
```

④ 예산이 소진되면(종료 코드 4) 다음 날 같은 명령을 반복한다. 완료분은 journal에 남아 이어서 진행된다.

⑤ 이전이 끝나면 API 데이터를 삭제한다(토큰은 유지).

```bash
stk purge --yes
```

매일 자동으로 반복하려면 [무인 반복 실행](../automation.md)을 따른다.

## 4. 쿼터 운영

- 쓰기 1건은 기본 50 units(`subscriptions.insert`, `playlists.insert`, `playlistItems.insert`, `videos.rate`, 위치 보정 `playlistItems.update`)이며, 목록 조회는 페이지당 1 unit이다.
- `--budget`(기본 9500)만큼만 쓰기를 예약하고 남은 500 units는 읽기 버퍼로 남겨 둔다. `push`·`migrate`·`history playlist`의 `--budget`을 생략하면 장부 값, 장부가 없으면 9500을 쓴다.
- 남은 쓰기 수는 `stk quota show`로 확인한다. 장부가 없으면 기본 단가·예산 기준 하루 쓰기 가능 건수를 보여준다.

```bash
stk quota show
stk quota show --json
```

- Google Cloud 콘솔의 "YouTube Data API v3 Queries per day" 실측이 장부와 어긋나면 콘솔 값으로 맞춘다. 실제 단가가 50과 다르면 `--write-cost`로 교정하고, 교정값은 장부에 저장되어 다음 실행에도 유지된다.

```bash
stk quota sync --used <콘솔에_표시된_오늘_사용량>
stk quota sync --used <콘솔값> --budget 9500 --write-cost 50
```

- 전역 옵션 `--write-cost <단가>`를 쓰면 해당 실행의 단가를 덮어쓴다.
- Google Cloud 콘솔 사용량은 몇 분에서 수십 분 지연된다. 실행 중에 읽은 값으로 `quota sync`를 하면 실제보다 적게 잡혀 곧바로 `quotaExceeded`(종료 코드 4)를 만날 수 있다. 실행이 끝나고 콘솔 값이 갱신된 뒤에 맞추고, `quotaExceeded` 자체를 "오늘 예산 소진"의 확정 신호로 삼는다.

### 예상 일수 계산 예

구독 239개, 플레이리스트 항목 1,179개, 좋아요 804개를 복사하는 경우(단가 50, 예산 9500 기준):

```text
쓰기 건수 = 239 + 1,179 + 804 = 2,222건
예상 쿼터 = 2,222 × 50 = 111,100 units
예상 일수 = 111,100 / 9,500 = 11.7 → 12일
```

플레이리스트를 새로 만들 때는 생성 건수(목록당 50 units)가 추가된다. `stk plan --json`의 `quota.estimated_days`가 같은 계산을 보여준다. 쿼터 정책의 근거와 대형 계정 읽기 버퍼 조정은 [이전 요구사항](../youtube-migration-requirements.md#3-youtube-api-쿼터)을 따른다.

## 5. 순서와 멱등성

- 성공·건너뜀은 `data/journal.jsonl`에 즉시 기록된다. 같은 명령을 다시 실행하면 완료된 액션은 건너뛰므로 중복 쓰기가 발생하지 않고, 실패한 액션만 재시도된다.
- `stk push`와 `stk migrate`는 실행 직전에 대상 상태를 다시 읽어 액션을 재계산한다. 대상 계정이 직접 바뀌었으면 `stk plan --refresh`로 원본 스냅샷부터 다시 읽는다.
- `playlistItems.insert`에는 `position`을 넣지 않는다(수동 정렬이 아닌 목록에서 거부될 수 있기 때문). 새로 만든 플레이리스트는 항목 추가가 끝난 뒤 대상을 다시 조회해 기대 순서와 비교하고, 다르면 `playlistItems.update`로 위치를 보정한다. 보정은 항목당 50 units가 든다.
- 이미 대상에 있던 제목의 플레이리스트를 재사용한 경우에는 기존 항목과 섞이므로 순서 보정을 하지 않는다.
- `--limit`으로 일부만 처리했거나 쿼터가 소진된 경우에는 순서 검증을 건너뛰고, 다음 실행에서 항목을 끝까지 채운 뒤 수행한다.
- 보정 중 `manualSortRequired`가 나오면 보정을 멈추고 YouTube 스튜디오에서 수동으로 정렬하라는 경고를 남긴다.

오류별 중단·건너뜀·재시도 규칙은 [이전 요구사항](../youtube-migration-requirements.md#6-오류-매핑)을 따른다.

## 6. 브라우저 세션 실행(선택)

OAuth API를 쓰지 않고, 사용자가 로그인한 Chrome에 CDP로 붙어 동기화를 실행할 수 있다. 계획·journal은 기존 경로와 공유하지만 요청은 브라우저 페이지에서 실행된다. 준비 절차, 약관·계정 위험, 제약은 [브라우저 세션 실행](../browser-mode.md)을 따른다.

```bash
stk browser check --port 9222
stk browser sync subscriptions --port 9222 --dry-run
stk browser sync subscriptions --port 9222 --limit 20 --yes
```

## 7. Watch Later 동기화

API와 Takeout으로는 옮길 수 없고, 브라우저 세션에서만 가능하다.

```bash
# 원본 계정 Chrome(예: 9223)에서 로그인 후
stk browser watch-later export --port 9223

# 대상 계정 Chrome(예: 9222)에 반영
stk browser watch-later sync --port 9222 --dry-run
stk browser watch-later sync --port 9222 --yes
# 대상에만 있는 항목까지 원본과 같게 만들려면
stk browser watch-later sync --port 9222 --prune --yes
```

- 추가는 원본의 오래된 항목부터 수행해 최신순 표시를 맞춘다. 다만 부분 동기화 후에는 순서가 어긋날 수 있다(위치 지정 API 없음).
- `--prune`은 대상에만 있는 항목을 삭제한다. 원본 내보내기 파일이 비어 있으면 안전을 위해 중단한다.
- 자세한 준비·문제 해결은 [브라우저 세션 실행](../browser-mode.md)을 따른다.

## 8. 자동화

여러 날 반복 실행을 스케줄러에 맡기는 방법은 [무인 반복 실행](../automation.md)을 따른다.

## 참고 링크

- [YouTube Data API 할당량 계산](https://developers.google.com/youtube/v3/determine_quota_cost)
- [데스크톱 앱용 OAuth](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Google Takeout](https://takeout.google.com/)
- [videos.list `myRating` 제한](https://developers.google.com/youtube/v3/docs/videos/list)
- [playlistItems.insert 오류](https://developers.google.com/youtube/v3/docs/playlistItems/insert)
