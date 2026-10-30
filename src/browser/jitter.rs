//! 요청 간격에 더할 지터(무작위 지연).
//!
//! 429(요청 제한)를 피하기 위해 고정 지연에 작은 무작위 폭을 더한다. 브라우저 지문을
//! 바꾸거나 자동화를 숨기는 목적이 아니라, 요청이 같은 간격으로 몰리지 않게 하는 페이싱이다.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// xorshift64* 기반 결정적 난수 생성기.
#[derive(Debug, Clone)]
pub struct Jitter {
    state: u64,
}

impl Jitter {
    /// 시스템 시각으로 시드를 만든다.
    pub fn from_clock() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        Self::with_seed(nanos)
    }

    /// 시드를 직접 지정한다(테스트용).
    pub fn with_seed(seed: u64) -> Self {
        Self { state: seed | 1 }
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.state;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.state = value;
        value.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// `0..=max` 밀리초 사이의 지연을 돌려준다. `max`가 0이면 0.
    pub fn duration(&mut self, max: Duration) -> Duration {
        let max_ms = max.as_millis() as u64;
        if max_ms == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(self.next_u64() % (max_ms + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_bounds() {
        let mut jitter = Jitter::with_seed(42);
        let max = Duration::from_millis(1_000);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let value = jitter.duration(max);
            assert!(value <= max, "지터가 상한을 넘었다: {value:?}");
            seen.insert(value.as_millis());
        }
        assert!(
            seen.len() > 50,
            "지터가 충분히 분산되지 않는다: {}",
            seen.len()
        );
    }

    #[test]
    fn zero_jitter_is_zero() {
        let mut jitter = Jitter::with_seed(7);
        assert_eq!(jitter.duration(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn same_seed_gives_same_sequence() {
        let mut left = Jitter::with_seed(1234);
        let mut right = Jitter::with_seed(1234);
        for _ in 0..10 {
            assert_eq!(
                left.duration(Duration::from_millis(500)),
                right.duration(Duration::from_millis(500))
            );
        }
    }
}
