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

#[cfg(test)]
mod tests {
    // catalog: WEBUI-MOD-task-engine-discoverer-adaptive-rs
    // oracle: L1
    use super::*;

    #[test]
    fn retry_after_extraction_handles_ms_and_seconds_forms() {
        assert_eq!(
            extract_retry_after_ms_from_error("Retry-After-Ms=250"),
            Some(250)
        );
        assert_eq!(
            extract_retry_after_ms_from_error("failed: retry_after=2"),
            Some(2000)
        );
        assert_eq!(
            extract_retry_after_ms_from_error("retry-after-ms=0"),
            None,
            "zero must not be treated as a positive backoff"
        );
        assert_eq!(
            extract_retry_after_ms_from_error("retry_after_ms=abc"),
            None
        );
        assert_eq!(extract_retry_after_ms_from_error("no header here"), None);
    }

    #[test]
    fn pressure_error_detection_matches_documented_statuses() {
        for msg in [
            "request failed status=429",
            "Request Failed Status 429",
            "upstream timed out",
            "client timeout",
            "status=503",
            "status 502",
            "status=504",
        ] {
            assert!(is_pressure_error_message(msg), "msg={msg:?}");
        }
        assert!(!is_pressure_error_message("status=404"));
        assert!(!is_pressure_error_message("connection refused"));
    }

    #[test]
    fn pressure_grows_backoff_with_cap_and_retry_after_floor() {
        let control = AdaptiveRateControl::new(true, 250);
        // First pressure event floors at 100ms.
        control.on_pressure(None);
        assert_eq!(control.current_delay_ms.load(Ordering::Relaxed), 100);
        // Doubling: 100*2+50 = 250, exactly the cap.
        control.on_pressure(None);
        assert_eq!(control.current_delay_ms.load(Ordering::Relaxed), 250);
        // Further pressure stays clamped at max_delay_ms.
        control.on_pressure(None);
        assert_eq!(control.current_delay_ms.load(Ordering::Relaxed), 250);
        // An explicit retry-after cannot exceed the cap either.
        control.on_pressure(Some(240));
        assert_eq!(control.current_delay_ms.load(Ordering::Relaxed), 250);
        // With headroom, the explicit retry-after beats the doubling result.
        let control = AdaptiveRateControl::new(true, 1000);
        control.on_pressure(Some(300));
        assert_eq!(control.current_delay_ms.load(Ordering::Relaxed), 300);
    }

    #[test]
    fn success_streak_decays_backoff_only_after_twelve_wins() {
        let control = AdaptiveRateControl::new(true, 1000);
        control.on_pressure(None); // delay = 100
        for _ in 0..11 {
            control.on_success();
        }
        assert_eq!(
            control.current_delay_ms.load(Ordering::Relaxed),
            100,
            "streak below the threshold must not reduce the delay"
        );
        control.on_success(); // 12th success triggers the decay
        assert_eq!(
            control.current_delay_ms.load(Ordering::Relaxed),
            70,
            "decay is current * 0.8 - 10"
        );
    }

    #[test]
    fn disabled_control_is_a_no_op() {
        let control = AdaptiveRateControl::new(false, 1000);
        control.on_pressure(None);
        control.on_pressure(Some(500));
        control.on_success();
        control.on_error_message("status=429 timeout");
        assert_eq!(control.current_delay_ms.load(Ordering::Relaxed), 0);
        // wait_turn with the control disabled must resolve without delay.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_millis(50), control.wait_turn())
                    .await
                    .expect("disabled wait_turn must return immediately");
            });
    }
}
