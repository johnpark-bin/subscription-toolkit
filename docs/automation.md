# 무인 반복 실행

YouTube 이전은 하루 쿼터 예산 때문에 여러 날에 걸쳐 반복해야 한다. 이 문서는 매일 같은 명령을 자동으로 실행하는 방법을 정리한다. 서비스별 실행 전략은 [YouTube 운영 문서](services/youtube.md#3-실행-전략)을 따른다.

## 1. 전제

- [준비 절차](setup.md)대로 로그인을 마쳐 토큰 파일(`tokens/source.json`, `tokens/target.json`)이 있어야 한다.
- 매일 실행할 명령은 예산을 명시한다.

```bash
stk migrate --yes --budget 9500
```

구독만 먼저 옮기는 등 유형별로 나누려면 다음을 쓴다.

```bash
stk push subscriptions --yes --budget 9500
stk push playlists --yes --budget 9500
stk push likes --yes --budget 9500
```

- 비TTY에서 실행되므로 `--yes`는 필수다. 없으면 종료 코드 2로 멈춘다.
- 스케줄러 시각은 태평양 자정(쿼터 리셋) 직후로 잡는다. 아래 예시는 매일 07:10 UTC를 쓴다.
- 토큰이 Testing 상태 OAuth 클라이언트에서 발급되었다면 refresh token이 7일 후 만료된다. 그때는 스케줄러가 실패하므로 직접 `stk auth login --account <source|target>`으로 재로그인해야 한다. 가능하면 동의 화면을 In production으로 전환한다. 자세한 절차는 [준비 절차](setup.md#3-oauth-동의-화면-구성)를 따른다.

## 2. 실행 전 확인

스케줄러에 등록하기 전에 직접 한 번 실행해 예산과 단가를 맞춘다.

```bash
stk quota show
stk plan
stk migrate --yes --budget 9500
```

Google Cloud 콘솔의 "YouTube Data API v3 Queries per day" 실측이 장부와 어긋나면 교정한다. `--write-cost`로 고친 단가는 장부에 저장되어 다음 실행에도 유지된다.

```bash
stk quota sync --used <콘솔에_표시된_오늘_사용량> [--write-cost <단가>]
```

콘솔 사용량은 지연되어 표시된다. 실행 도중의 값으로 맞추면 실제보다 적게 잡혀 다음 실행이 `quotaExceeded`(종료 코드 4)로 끝날 수 있다. 실행이 끝나고 콘솔이 갱신된 뒤에 맞추는 것을 권장한다.

## 3. macOS `launchd` 예시

`~/Library/LaunchAgents/com.subscription-toolkit.stk.plist`로 저장한다. `stk`는 절대 경로로 지정하고, 설정·데이터 루트가 기본값이 아니면 `STK_HOME`을 지정한다.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.subscription-toolkit.stk</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/stk</string>
    <string>migrate</string>
    <string>--yes</string>
    <string>--budget</string>
    <string>9500</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>STK_HOME</key>
    <string>/Users/example/.config/subscription-toolkit</string>
  </dict>
  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>7</integer>
    <key>Minute</key>
    <integer>10</integer>
  </dict>
  <key>StandardOutPath</key>
  <string>/Users/example/Library/Logs/stk-migrate.log</string>
  <key>StandardErrorPath</key>
  <string>/Users/example/Library/Logs/stk-migrate-error.log</string>
</dict>
</plist>
```

등록과 확인:

```bash
launchctl load ~/Library/LaunchAgents/com.subscription-toolkit.stk.plist
launchctl list | grep stk
tail -f ~/Library/Logs/stk-migrate.log ~/Library/Logs/stk-migrate-error.log
```

## 4. Linux `cron` 예시

`flock`으로 중복 실행을 막고 매일 07:10 UTC에 실행한다.

```cron
10 7 * * * /usr/bin/flock -n /tmp/stk-migrate.lock /usr/local/bin/stk migrate --yes --budget 9500 >>/var/log/stk-migrate.log 2>&1
```

기본값이 아닌 설정·데이터 루트를 쓰면 `STK_HOME`을 앞에 둔다.

```cron
10 7 * * * STK_HOME=/home/example/.config/subscription-toolkit /usr/bin/flock -n /tmp/stk-migrate.lock /usr/local/bin/stk migrate --yes --budget 9500 >>/var/log/stk-migrate.log 2>&1
```

## 5. 안전장치

- 중복 실행 차단: `plan`, `push`, `migrate`, `history playlist`, `purge`는 `data/.lock`에 배타 잠금을 건다. 다른 `stk` 프로세스가 실행 중이면 "다른 stk 프로세스가 실행 중입니다" 오류로 멈추므로, 스케줄 간격이 겹쳐도 데이터가 깨지지 않는다. `cron` 예시의 `flock`은 잠금이 풀리기를 기다리지 않고 건너뛰는 추가 보호막이다.
- 재개: 성공·건너뜀은 `data/journal.jsonl`에 즉시 기록된다. 다음 날 같은 명령을 실행하면 완료분은 건너뛰고 이어서 진행한다. 실패한 액션만 재시도된다.
- 진행 확인: 실행 후 `stk status`로 완료·남은 수와 남은 예산을 확인한다.

```bash
stk status
stk status --json
stk quota show
```

- 대상 계정 변경 감지: 대상에서 직접 구독·플레이리스트·좋아요를 바꾸었다면 `stk plan --refresh`로 원본 스냅샷부터 다시 읽은 뒤 스케줄 실행을 계속한다.
- 실패 시 종료 코드:

| 코드 | 의미 | 대응 |
| --- | --- | --- |
| 0 | 성공(건너뜀 포함) | 다음 날 반복 또는 종료 |
| 1 | 일반 오류(네트워크, 실패 액션 등) | 로그 확인 후 같은 명령 재실행 |
| 2 | 사용법 오류(`--yes` 누락 등) | 스케줄러 명령 수정 |
| 3 | 인증·채널 문제 | `stk auth login --account <source\|target>`으로 재로그인 |
| 4 | 쿼터 예산 소진 | 다음 날 자동 실행을 기다림 |
| 5 | 부분 실패·재시도 필요 | 메시지 안내대로 구독 정리 또는 몇 시간 후 재실행 |

종료 코드의 근거와 오류별 처리는 [이전 요구사항](youtube-migration-requirements.md#6-오류-매핑)을 따른다.

## 참고 링크

- [YouTube Data API 할당량 계산](https://developers.google.com/youtube/v3/determine_quota_cost)
- [데스크톱 앱용 OAuth](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Google Takeout](https://takeout.google.com/)
