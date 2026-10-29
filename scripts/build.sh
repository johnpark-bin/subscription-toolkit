#!/usr/bin/env bash
# subscription-toolkit 빌드 스크립트.
#
# 예) scripts/build.sh --check --install ~/.local/bin
set -euo pipefail

usage() {
    cat <<'EOF'
사용법: scripts/build.sh [옵션]

옵션:
  --check            빌드 전에 fmt/clippy/test 검증을 실행합니다.
  --install [DIR]    빌드한 stk를 DIR에 설치합니다(기본: ~/.local/bin).
  --debug            release 대신 debug 프로필로 빌드합니다.
  --clean            빌드 전에 target 디렉터리를 정리합니다.
  -h, --help         이 도움말을 출력합니다.

예:
  scripts/build.sh
  scripts/build.sh --check
  scripts/build.sh --check --install ~/.local/bin
EOF
}

# 스크립트 위치를 기준으로 저장소 루트에서 실행한다.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run_check=false
install_dir=""
profile="release"
clean=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --check)
            run_check=true
            shift
            ;;
        --install)
            if [[ $# -ge 2 && "$2" != --* ]]; then
                install_dir="$2"
                shift 2
            else
                install_dir="$HOME/.local/bin"
                shift
            fi
            ;;
        --debug)
            profile="debug"
            shift
            ;;
        --clean)
            clean=true
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "오류: 알 수 없는 옵션입니다: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if ! command -v cargo >/dev/null 2>&1; then
    echo "오류: cargo를 찾을 수 없습니다. https://rustup.rs 에서 설치하세요." >&2
    exit 1
fi

if $clean; then
    echo "==> cargo clean"
    cargo clean
fi

if $run_check; then
    echo "==> cargo fmt --all -- --check"
    cargo fmt --all -- --check
    echo "==> cargo clippy --all-targets --all-features -- -D warnings"
    cargo clippy --all-targets --all-features -- -D warnings
    echo "==> cargo test --all-features"
    cargo test --all-features
fi

if [[ "$profile" == "release" ]]; then
    echo "==> cargo build --release"
    cargo build --release
    binary="target/release/stk"
else
    echo "==> cargo build"
    cargo build
    binary="target/debug/stk"
fi

version="$("$binary" --version)"
size="$(wc -c <"$binary" | tr -d ' ')"
echo "빌드 완료: $ROOT/$binary"
echo "  버전: $version"
echo "  크기: ${size} bytes"

if [[ -n "$install_dir" ]]; then
    mkdir -p "$install_dir"
    install -m 0755 "$binary" "$install_dir/stk"
    echo "설치 완료: $install_dir/stk"
    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *)
            echo "안내: PATH에 $install_dir 가 없습니다. 셸 설정에 다음을 추가하세요:"
            echo "  export PATH=\"$install_dir:\$PATH\""
            ;;
    esac
    echo "스케줄러(launchd/cron)에 넣을 절대 경로: $install_dir/stk"
fi