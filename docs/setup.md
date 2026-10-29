# 준비 절차: Google Cloud, OAuth, Takeout

이 문서는 `stk`를 실제 계정에 사용하기 전에 필요한 준비를 순서대로 설명한다. 실제 계정 이메일은 문서에 쓰지 않고 `<원본 계정 이메일>`, `<대상 계정 이메일>` 플레이스홀더로 표기한다.

## 0. 준비물

- Google Cloud 계정(개인 계정 가능)
- 원본/대상 YouTube 계정 두 개와 각 계정의 YouTube 채널
- Google Takeout 시청기록(시청기록 리포트/플레이리스트를 만들 때만)

## 1. Google Cloud 프로젝트 만들기

1. <https://console.cloud.google.com/>에 로그인한다.
2. 상단 프로젝트 선택기에서 **새 프로젝트**를 만든다(예: `subscription-toolkit`).
3. 만든 프로젝트를 선택한 상태로 진행한다.

## 2. YouTube Data API v3 사용 설정

1. **API 및 서비스 → 라이브러리**로 이동한다.
2. `YouTube Data API v3`를 검색해 **사용 설정**을 누른다.

## 3. OAuth 동의 화면 구성

1. **API 및 서비스 → OAuth 동의 화면**으로 이동한다.
2. User Type은 **External**을 선택한다.
3. 앱 이름(예: `subscription-toolkit`), 사용자 지원 이메일, 개발자 연락처를 입력한다.
4. **범위 추가**에서 다음 스코프를 추가한다.
   - `https://www.googleapis.com/auth/youtube.force-ssl`
5. 테스트 사용자를 추가한다.
   - Testing 상태에서는 **테스트 사용자 최대 100명**까지만 로그인할 수 있다.
   - `<원본 계정 이메일>`, `<대상 계정 이메일>` 두 계정을 모두 추가한다.
6. 가능하면 **게시 상태를 In production으로 전환**한다.
   - Testing 상태에서는 **refresh token이 7일 후 만료**된다. 그때마다 `stk auth login`을 다시 실행해야 한다.
   - In production으로 전환하면 검증 절차가 필요할 수 있다(미검증 상태로도 동작하지만 Google 경고 화면이 나온다).

## 4. OAuth 데스크톱 클라이언트 만들기

1. **API 및 서비스 → 사용자 인증 정보 → 사용자 인증 정보 만들기 → OAuth 클라이언트 ID**를 선택한다.
2. 애플리케이션 유형은 **데스크톱 앱**을 선택한다.
3. 이름을 입력하고 **만들기**를 누른다.
4. 생성된 클라이언트의 JSON을 **JSON 다운로드**로 저장한다.

다운로드한 파일은 다음 위치에 둔다.

```text
~/.config/subscription-toolkit/client_secret.json
```

다른 경로를 쓰려면 `--client-secret <경로>`로 지정한다. 파일에는 `{"installed": {...}}` 형식의 데스크톱 클라이언트 정보가 있어야 한다. 웹 애플리케이션(`"web"`) 형식은 지원하지 않는다.

> 데스크톱 클라이언트의 `client_secret`은 비밀로 취급되지 않는 공개 정보지만, 저장소에 커밋하지 않는다. `.gitignore`에 `client_secret*.json`이 포함되어 있다.

## 5. 두 계정의 YouTube 채널 확인

API 쓰기는 로그인한 사용자의 **YouTube 채널** 기준으로 동작한다. 채널이 없으면 `subscriptions.insert` 등이 `subscriberNotFound`로 실패하고, `stk auth login`도 채널을 찾지 못해 중단한다.

1. `<원본 계정 이메일>`로 <https://www.youtube.com/>에 로그인해 채널이 있는지 확인한다. 없으면 **채널 만들기**로 생성한다.
2. `<대상 계정 이메일>`도 같은 방식으로 채널을 만든다.
3. 브랜드 계정이나 채널이 여러 개인 경우, OAuth 동의 화면에서 **이전에 사용할 채널을 직접 선택**해야 한다. 로그인 후 `stk auth status`로 채널 이름과 ID를 확인한다.

## 6. 로그인

```bash
export SOURCE_EMAIL='<원본 계정 이메일>'
export TARGET_EMAIL='<대상 계정 이메일>'

stk auth login --account source --login-hint "$SOURCE_EMAIL"
stk auth login --account target --login-hint "$TARGET_EMAIL"

stk auth status
```

- 브라우저가 자동으로 열리지 않으면 터미널에 출력된 주소를 직접 연다. `--no-browser`는 브라우저 실행을 생략한다.
- 로그인 중에는 `127.0.0.1`의 임시 포트에서 콜백을 기다린다(루프백 + PKCE). 방화벽이 로컬 루프백을 막으면 실패한다.
- 동의 화면에서 **모든 요청 권한을 허용**해야 한다. 거부하면 `access_denied`로 중단된다.
- 로그인에 성공하면 `토큰을 저장했습니다: .../tokens/<account>.json`이 stderr에 출력되고, 채널 이름과 ID가 stdout에 출력된다.

확인:

```bash
stk auth status --json
```

```json
{
  "source": {"logged_in": true, "channel_id": "UC...", "channel_title": "원본 채널"},
  "target": {"logged_in": true, "channel_id": "UC...", "channel_title": "대상 채널"}
}
```

## 7. 계획 확인 후 실행

```bash
stk plan                 # 차이와 예상 쿼터 확인(쓰기 없음)
stk push subscriptions --limit 2 --yes
stk push playlists --yes
stk push likes --yes
```

- `stk plan --json`은 스크립트에서 읽기 좋은 요약을 출력한다.
- `push`는 실행 직전에 대상 상태를 다시 읽으므로, 중간에 실패해도 같은 명령을 다시 실행하면 이어서 진행된다.
- 좋아요는 API 특성상 **최근 1,000개까지만** 조회되므로 그 이후 항목은 복사되지 않는다.

## 8. Takeout 시청기록 내보내기

시청기록은 YouTube API로 읽거나 쓸 수 없다. Google Takeout으로 내려받아 분석하고, 비공개 플레이리스트로 재현한다.

1. <https://takeout.google.com/>에 원본 계정으로 로그인한다.
2. **모두 선택 해제** 후 `YouTube 및 YouTube Music`만 선택한다.
3. **여러 형식**에서 `JSON`을 선택한다(HTML만 선택하면 `watch-history.json`이 없다).
4. 내보내기를 만들고 완료되면 zip을 내려받는다.
5. 다음 명령으로 가져온다.

```bash
stk history import --takeout ~/Downloads/takeout-20261029.zip
stk history report --top 20
stk history report --format json --out report.json
stk history playlist --dry-run
stk history playlist --yes
```

- `import`는 zip과 압축을 푼 디렉터리 모두 지원하며, 하위 6단계까지 `watch-history.json`을 찾는다.
- 후보가 없거나 2개 이상이면 오류가 나며, 오류 메시지에 경로가 표시된다.
- 시청기록 원본은 512MB를 넘으면 거부한다.

## 9. 데이터 삭제

```bash
stk purge --yes            # 스냅샷·계획·journal·쿼터 장부·시청기록 삭제
stk purge --tokens --yes   # 위 데이터 + 토큰 삭제
```

API 데이터는 최대 30일 보관이 원칙이며, `purge`로 즉시 삭제할 수 있다. 토큰을 삭제하면 다시 로그인해야 한다.

## 10. 문제 해결

| 증상 | 원인 | 해결 |
| --- | --- | --- |
| `redirect_uri_mismatch` | 웹 애플리케이션 클라이언트를 사용 | 데스크톱 앱 클라이언트 JSON으로 교체 |
| `동의가 취소되었습니다(access_denied)` | 동의 화면에서 권한 거부, 또는 테스트 사용자 미등록 | 테스트 사용자에 계정 추가 후 재로그인 |
| `invalid_grant`(갱신 실패) | Testing 상태의 7일 만료, 비밀번호 변경, 권한 철회 | `stk auth logout --account <계정>` 후 재로그인, 가능하면 In production 전환 |
| `YouTube 채널이 없습니다` | 해당 Google 계정에 채널이 없음 | youtube.com에서 채널 생성 후 재로그인 |
| `갱신 토큰을 받지 못했습니다` | 이전 동의가 남아 refresh token이 발급되지 않음 | <https://myaccount.google.com/permissions>에서 앱 접근 권한 제거 후 재로그인 |
| `OAuth 클라이언트 설정 파일을 찾을 수 없습니다` | JSON 경로가 다름 | `~/.config/subscription-toolkit/client_secret.json`에 두거나 `--client-secret`로 지정 |
| 종료 코드 4 | 하루 쿼터 예산 소진 | 태평양 시간 자정 이후 다시 실행하거나 `--budget`을 조정 |
| 종료 코드 5 + "구독 한도" | 대상 계정의 구독 한도 도달 | 대상 계정의 구독을 정리한 뒤 다시 실행 |
| 종료 코드 5 + "몇 시간 후" | 최근 구독 요청 과다 | 몇 시간 뒤 다시 실행 |
| 종료 코드 2 | 비TTY에서 확인 프롬프트 | `--yes`를 붙여 실행 |

로그를 더 보고 싶으면 `--verbose`, 경고만 보려면 `--quiet`를 붙인다. 토큰과 이메일은 로그에 남지 않는다.