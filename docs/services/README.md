# 서비스별 이전 문서

이 디렉터리는 `stk`가 지원하는 서비스별 운영 문서를 모은다. `stk`는 서비스별 어댑터 구조로 확장되는 것을 전제로 하며, 현재 구현된 서비스는 YouTube뿐이다. 공통 규칙(브랜치, 커밋, 테스트, 보안)은 [AGENTS.md](../../AGENTS.md)를 따른다.

## 지원 현황

| 서비스 | 이전 가능 범위 | 필수 전처리 | 문서 |
| --- | --- | --- | --- |
| YouTube | 구독·플레이리스트·좋아요 복사, 시청기록 리포트+비공개 플레이리스트 재현 | Google Cloud 프로젝트, YouTube Data API v3, OAuth 데스크톱 클라이언트, 양쪽 계정 채널 | [youtube.md](youtube.md) |

새 서비스가 추가되면 이 표에 행을 추가하고, 아래 체크리스트대로 문서를 만든다.

## 새 서비스 문서 체크리스트

파일 이름 규칙: `docs/services/<service>.md` (소문자 영어, 예: `spotify.md`).

새 문서에 반드시 포함할 6개 절:

1. 전처리 — 계정·앱 등록·API 사용 설정 등 실행 전 준비 절차
2. 한도 — 서비스가 제공하는 이전 가능/부분/불가 범위와 이유
3. 인증 — `stk`에서 쓰는 인증 방식과 토큰 보관 위치
4. 쿼터 — 일일 한도, 요청당 비용, 예산 초과 시 동작
5. 오류 — 대표 오류와 재시도·중단·건너뜀 정책, 종료 코드
6. 수용 기준 — 이전이 완료되었다고 판단하는 조건과 확인 방법

## 공통 문서

| 문서 | 내용 |
| --- | --- |
| [준비 절차](../setup.md) | Google Cloud와 OAuth, Takeout 준비 (YouTube 기준) |
| [이전 요구사항](../youtube-migration-requirements.md) | 이전 범위, 쿼터 계산, 보관·오류·재개 정책 (YouTube 기준) |
| [무인 반복 실행](../automation.md) | 하루 예산으로 여러 날 실행하는 스케줄러 설정 |
| [README](../../README.md) | 설치, 명령 요약, 종료 코드, 데이터 파일 위치 |

## 참고 링크

- [YouTube Data API 할당량 계산](https://developers.google.com/youtube/v3/determine_quota_cost)
- [데스크톱 앱용 OAuth](https://developers.google.com/identity/protocols/oauth2/native-app)
- [Google Takeout](https://takeout.google.com/)
