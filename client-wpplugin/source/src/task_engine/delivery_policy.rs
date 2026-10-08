//! A failed WordPress delivery retries the saved result.
//! It does not send the unit back to the provider.

pub(crate) fn status_after_callback_failure(retry_count: i64, max_retries: i64) -> &'static str {
    if max_retries > 0 && retry_count >= max_retries {
        "failed"
    } else {
        "translated"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_result_stays_saved_when_wordpress_rejects_delivery() {
        assert_eq!(status_after_callback_failure(1, 5), "translated");
        assert_eq!(status_after_callback_failure(5, 5), "failed");
        assert_ne!(status_after_callback_failure(0, 0), "pending");
    }
}
