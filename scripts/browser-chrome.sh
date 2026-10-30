#!/usr/bin/env bash
# 대상 계정 로그인용 Chrome을 별도 프로필 + DevTools 포트로 실행한다.
#
# 예) PORT=9222 scripts/browser-chrome.sh
set -euo pipefail

PORT="${PORT:-9222}"
PROFILE="${PROFILE:-$HOME/.config/subscription-toolkit/chrome-target}"

if [[ -z "${CHROME:-}" ]]; then
    for candidate in \
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
        "/Applications/Chromium.app/Contents/MacOS/Chromium" \
        "$(command -v google-chrome || true)" \
        "$(command -v chromium || true)" \
        "$(command -v chromium-browser || true)"; do
        if [[ -n "$candidate" && -x "$candidate" ]]; then
            CHROME="$candidate"
            break
        fi
    done
fi

if [[ -z "${CHROME:-}" ]]; then
    echo "오류: Chrome/Chromium 실행 파일을 찾을 수 없습니다. CHROME 환경 변수로 지정하세요." >&2
    exit 1
fi

mkdir -p "$PROFILE"
echo "Chrome: $CHROME"
echo "DevTools 포트: $PORT"
echo "프로필: $PROFILE"
echo
echo "1) 열린 Chrome에서 대상 계정으로 로그인하세요."
echo "2) 로그인 후 다른 터미널에서: stk browser check --port $PORT"
echo "3) 계획이 있으면: stk browser sync playlists --port $PORT --dry-run"
echo

exec "$CHROME" \
    --remote-debugging-port="$PORT" \
    --user-data-dir="$PROFILE" \
    --no-first-run \
    --no-default-browser-check \
    https://www.youtube.com/
