use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

pub(super) fn extract_retry_after_ms_from_error(err_text: &str) -> Option<u64> {
    let lower = err_text.to_lowercase();
    for key in ["retry_after_ms=", "retry-after-ms="] {
        if let Some(pos) = lower.find(key) {
            let rest = &lower[pos + key.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<u64>() {
                if v > 0 {
                    return Some(v);
                }
            }
        }
    }
    for key in ["retry_after=", "retry-after="] {
        if let Some(pos) = lower.find(key) {
            let rest = &lower[pos + key.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<u64>() {
                if v > 0 {
                    return Some(v.saturating_mul(1000));
                }
            }
        }
    }
    None
}

pub(super) fn is_pressure_error_message(err_text: &str) -> bool {
    let msg = err_text.to_lowercase();
    msg.contains("status=429")
        || msg.contains("status 429")
        || msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("status=503")
        || msg.contains("status 503")
        || msg.contains("status=502")
        || msg.contains("status 502")
        || msg.contains("status=504")
        || msg.contains("status 504")
}

#[derive(Clone)]
pub(super) struct AdaptiveRateControl {
    enabled: bool,
    max_delay_ms: u64,
    current_delay_ms: Arc<AtomicU64>,
    success_streak: Arc<AtomicU32>,
}

impl AdaptiveRateControl {
    pub(super) fn new(enabled: bool, max_delay_ms: u64) -> Self {
        Self {
            enabled,
            max_delay_ms: max_delay_ms.max(200),
            current_delay_ms: Arc::new(AtomicU64::new(0)),
            success_streak: Arc::new(AtomicU32::new(0)),
        }
    }

    pub(super) async fn wait_turn(&self) {
        if !self.enabled {
            return;
        }
        let delay = self.current_delay_ms.load(Ordering::Relaxed);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
    }

    pub(super) fn on_success(&self) {
        if !self.enabled {
            return;
        }
        let streak = self.success_streak.fetch_add(1, Ordering::Relaxed) + 1;
        if streak < 12 {
            return;
        }
        self.success_streak.store(0, Ordering::Relaxed);
        let current = self.current_delay_ms.load(Ordering::Relaxed);
        if current == 0 {
            return;
        }
        let reduced = current
            .saturating_mul(8)
            .saturating_div(10)
            .saturating_sub(10);
        self.current_delay_ms.store(reduced, Ordering::Relaxed);
    }

    pub(super) fn on_pressure(&self, retry_after_ms: Option<u64>) {
        if !self.enabled {
            return;
        }
        self.success_streak.store(0, Ordering::Relaxed);
        let current = self.current_delay_ms.load(Ordering::Relaxed);
        let mut next = current.saturating_mul(2).saturating_add(50).max(100);
        if let Some(v) = retry_after_ms {
            next = next.max(v);
        }
        next = next.min(self.max_delay_ms);
        self.current_delay_ms.store(next, Ordering::Relaxed);
    }

    pub(super) fn on_error_message(&self, err_text: &str) {
        if !is_pressure_error_message(err_text) {
            return;
        }
        self.on_pressure(extract_retry_after_ms_from_error(err_text));
    }
}
