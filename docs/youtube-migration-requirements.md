# YouTube 계정 이전 요구사항

이 문서는 `stk`가 지원하는 이전 범위, 제약, 쿼터 계산, 보안·보관 정책, 오류와 재개 규칙, 수용 기준을 정리한다. 설계 근거는 문서 끝의 출처를 따른다.

## 1. 이전 범위

| 항목 | 이전 | 방법 | 제약 |
| --- | --- | --- | --- |
| 구독 | 가능 | `subscriptions.insert` | 중복은 건너뜀, 구독 한도·최근 과다 요청 시 중단, 채널 없는 대상은 불가 |
| 플레이리스트와 항목 | 가능 | `playlists.insert` + `playlistItems.insert` | 항목 5,000개 상한, 수동 정렬 목록은 위치 지정 불가 |
| 좋아요 | 부분 | `videos.rate` | API가 최근 1,000개까지만 조회를 제공 |
| 시청기록 | 부분 | Takeout `watch-history.json` 파싱 → 리포트 + 비공개 플레이리스트 | YouTube API로 읽기/쓰기 불가, 시청기록 자체를 계정에 주입할 수 없음 |
| 좋아요/시청기록 외 개인 데이터 | 불가 | - | 아래 2장 참고 |

원본 계정의 데이터는 삭제하지 않는다. 모든 작업은 대상 계정으로의 복사다.

## 2. 이전할 수 없는 항목

| 항목 | 이유 |
| --- | --- |
| Watch Later(나중에 볼 동영상) | API 접근이 막혀 있고(`playlistOperationUnsupported`), Takeout 재생목록 내보내기에서도 제외된다 |
| 업로드 영상 | 채널 소유 자산이며 계정 이전 API가 없다 |
| 댓글 | 작성자 계정 귀속이며 대량 쓰기 API가 없다 |
| 멤버십, 구매/대여, 결제 | 계정 종속 상품 |
| 알림·추천 설정, 시청 기록 자체 | API 미제공. 시청기록은 리포트와 재현 플레이리스트로만 대체 |
| 채널명, 핸들, 채널 아트 | 채널 자체를 이전하는 기능이 없음 |

## 3. YouTube API 쿼터

- 기본 할당량: **하루 10,000 units**, 태평양 시간(PT) 자정에 초기화.
- 쓰기 요청 비용: `subscriptions.insert`, `playlists.insert`, `playlistItems.insert`, `videos.rate`, `playlistItems.update` 각 **50 units**.
- 목록 조회: 페이지당 **1 unit** (`maxResults=50`).
- 결과적으로 하루 최대 **200회 쓰기**(10,000 / 50)가 가능하다.

`stk`는 `--budget`(기본 9,500)만큼만 쓰기를 예약하고 남은 500 units를 읽기 버퍼로 남긴다. 대형 계정에서 읽기 쿼터가 500 units를 넘으면 `--budget`을 낮춰 실행하거나 쿼터 증액을 신청한다.

### 계산 예시

구독 500개와 플레이리스트 항목 1,000개를 복사하는 경우:

```text
구독   500 × 50 = 25,000 units
항목 1,000 × 50 = 50,000 units
합계            = 75,000 units
75,000 / 9,500  = 7.9 → 8일
```

`stk plan`(또는 `stk plan --json`의 `quota.estimated_days`)이 같은 계산을 보여준다. 실행 중 예산이 소진되면 종료 코드 4로 멈추고, 다음 날 같은 명령을 다시 실행하면 journal 덕분에 이어서 진행한다.

## 4. 인증

- OAuth 2.0 데스크톱 클라이언트, 루프백 리다이렉트(`http://127.0.0.1:<임의 포트>`) + PKCE(S256), `state` 검증.
- 스코프: `https://www.googleapis.com/auth/youtube.force-ssl`.
- `access_type=offline`, `prompt=consent`로 refresh token을 받는다. refresh token이 없으면 토큰을 저장하지 않고 중단한다.
- 로그인 직후 `channels.list(mine=true)`로 채널을 확인한다. 채널이 없으면 `youtube.com`에서 채널을 만든 뒤 재로그인해야 한다.
- 계정별 토큰 파일(`tokens/source.json`, `tokens/target.json`)에 클라이언트 ID/시크릿, refresh token, access token, 만료 시각, 채널 정보를 저장한다. 데스크톱 클라이언트 시크릿은 공개 정보로 취급한다.
- 액세스 토큰은 만료 60초 전에 자동 갱신하고, 갱신 결과를 토큰 파일에 다시 쓴다.
- OAuth 동의 화면이 Testing이면 refresh token이 7일 후 만료된다. `docs/setup.md`의 안내대로 In production 전환을 권장한다.

## 5. 데이터 보관과 보안

- YouTube API로 얻은 데이터(스냅샷, 계획, journal, 쿼터 장부)는 목적 달성 후 최대 **30일** 보관한다. 30일이 지난 스냅샷·계획은 다음 실행에서 삭제되고, journal 항목도 로드 시 정리된다.
- `stk purge`로 API 데이터를 즉시 삭제하고, `stk purge --tokens`로 토큰까지 삭제한다.
- 토큰 파일은 `0600`, 디렉터리는 `0700`으로 만든다. 권한이 다르면 로드 시 고친다.
- 로그에 토큰, 이메일, 전체 API 응답을 남기지 않는다. API 오류 본문은 300자로 줄여 표시한다.
- 저장소에는 실제 이메일, `client_secret.json`, 토큰, 스냅샷, journal, Takeout 원본을 커밋하지 않는다. 문서와 픽스처에는 플레이스홀더만 쓴다.

## 6. 오류 매핑

| HTTP | reason | 처리 |
| --- | --- | --- |
| 401 | - | 토큰 1회 강제 갱신 후 재시도, 실패 시 인증 오류(종료 코드 3) |
| 403 | `quotaExceeded` | 즉시 중단(종료 코드 4) |
| 403 | `rateLimitExceeded`, `userRateLimitExceeded` | 1s/2s/4s 재시도 후 실패 처리 |
| 400 | `emailNotVerified` | 인증 오류로 중단(종료 코드 3), 이메일 인증 안내 |
| 400 | `maxPlaylistExceeded`, `playlistTitleRequired` | 실패 처리(종료 코드 1) |
| 400 | `subscriptionDuplicate` | 건너뜀(성공으로 집계) |
| 400 | `subscriptionForbidden` + "maximum number of subscriptions" | 즉시 중단(종료 코드 5), 구독 한도 안내 |
| 400 | `subscriptionForbidden` + "Too many recent subscriptions" | 즉시 중단(종료 코드 5), 몇 시간 후 재시도 안내 |
| 403 | `playlistContainsMaximumNumberOfVideos` | 해당 항목과 같은 플레이리스트의 남은 항목을 건너뜀 |
| 400/403 | `playlistOperationUnsupported` | 건너뜀 |
| 400 | `manualSortRequired` | 순서 보정 중단, 수동 정렬 경고 |
| 404 | `videoNotFound`, `playlistNotFound` | 건너뜀 |
| 409 | `SERVICE_UNAVAILABLE`(status `ABORTED`) | YouTube가 간헐적으로 돌려주는 일시 오류. 최대 3회 재시도 후 실패 처리(종료 코드 1) |
| 429/5xx | - | 최대 3회 지수 백오프(1s/2s/4s) 후 실패 처리(종료 코드 1) |

## 7. 멱등성과 재개

- 모든 액션은 고유 키를 가진다: `subscription:{channel_id}`, `playlist:{source_playlist_id}`, `playlist-item:{source_playlist_id}:{video_id}`, `like:{video_id}`, 시청기록은 `playlist:history`, `playlist-item:history:{video_id}`.
- 성공·건너뜀은 `data/journal.jsonl`에 한 줄씩 즉시 기록(`sync_data`)된다. 다음 실행은 완료 키를 건너뛰므로 같은 명령을 반복해도 중복 쓰기가 발생하지 않는다.
- 실패로 기록된 액션은 다음 실행에서 재시도한다.
- 플레이리스트 생성 결과(원본 ID → 새 대상 ID)는 journal의 `detail.target_playlist_id`에 남아, 중단 후에도 항목 추가를 이어갈 수 있다.
- `stk push`는 실행 직전에 대상 계획을 재계산한다. 대상에 이미 있는 구독·항목·좋아요는 다시 쓰지 않는다.

## 8. 플레이리스트 순서

- `playlistItems.insert`에는 `position`을 넣지 않는다. 수동 정렬이 아닌 목록에서는 위치 지정이 거부될 수 있기 때문이다.
- 새로 만든 플레이리스트는 항목 추가 후 다시 조회해 기대 순서와 비교하고, 다르면 `playlistItems.update`로 위치를 보정한다(항목당 50 units).
- 재사용한 플레이리스트는 기존 항목과 섞이므로 순서를 바꾸지 않는다.
- `manualSortRequired`가 나오면 보정을 멈추고 YouTube 스튜디오에서 수동으로 정렬하도록 경고한다.
- `--limit`, 쿼터 소진, 항목 상한으로 일부만 처리한 경우 순서 검증을 건너뛰고 완료 후에 수행한다.

## 9. 수용 기준

1. `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features`가 통과한다.
2. `stk plan`은 원본/대상 건수와 예상 쿼터·일수를 출력하고 `data/plan.json`을 남긴다. 쓰기는 하지 않는다.
3. `stk push subscriptions --limit 2 --yes`는 성공 2건으로 종료 코드 0을 반환하고, 다시 실행하면 insert 0건(멱등)이다.
4. `--budget 50`에서 두 번째 쓰기는 종료 코드 4로 중단되고, journal에는 첫 액션만 성공으로 남는다.
5. 이미 구독한 채널(`subscriptionDuplicate`)과 삭제된 영상(`videoNotFound`)은 건너뛰고 계속 진행한다(종료 코드 0).
6. 토큰이 없으면 종료 코드 3과 재로그인 안내가 나온다.
7. `stk history import` 결과는 `total_events`, `unique_videos`, `unique_channels`, `unparsed_events`, `ad_events`, 기간, 상위 채널/영상, 월별 통계를 Markdown/JSON으로 제공한다.
8. `stk history playlist`는 광고를 제외하고 최신순 중복 제거한 항목을 비공개 플레이리스트로 만들며, 5,000개를 넘으면 잘라내고 리포트한다.
9. `stk purge --yes` 이후 `data/`에 API 데이터 파일이 없고 토큰은 유지된다. `--tokens`를 주면 토큰도 삭제된다.

## 10. 출처

- [YouTube Data API v3 revision history](https://developers.google.com/youtube/v3/revision_history) — 시청기록(`HL`/`WL`) API 지원 중단
- [Determine quota cost](https://developers.google.com/youtube/v3/determine_quota_cost) — 요청별 쿼터 비용
- [subscriptions.insert](https://developers.google.com/youtube/v3/docs/subscriptions/insert) — 중복·한도 오류
- [playlists.list](https://developers.google.com/youtube/v3/docs/playlists/list) — `mine=true` 동작
- [playlistItems.insert](https://developers.google.com/youtube/v3/docs/playlistItems/insert) — 위치/상한 오류
- [videos.list](https://developers.google.com/youtube/v3/docs/videos/list) — `myRating=like` 최근 1,000개 제한
- [videos.rate](https://developers.google.com/youtube/v3/docs/videos/rate) — 평가 오류
- [OAuth 2.0 for native apps](https://developers.google.com/identity/protocols/oauth2/native-app) — 루프백 + PKCE
- [OAuth 2.0 token expiration](https://developers.google.com/identity/protocols/oauth2#expiration) — Testing 7일 만료
- [Google Cloud OAuth audience](https://support.google.com/cloud/answer/15549945) — 테스트 사용자 100명 한도
- [YouTube API Services Developer Policies](https://developers.google.com/youtube/terms/developer-policies) — Authorized Data 30일 보관, 철회 시 7일 내 삭제
- [Takeout 시청기록 형식 안내(2차 자료)](https://playbackstats.com/guides/youtube-watch-history-json) — `watch-history.json` 구조
- [Takeout 재생목록 내보내기 안내(2차 자료)](https://www.marqly.com/blog/export-youtube-watch-later) — Watch Later 미포함