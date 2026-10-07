use super::*;

pub(super) const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Copy)]
pub(super) enum HttpResponseKind {
    Json,
    Binary,
    UploadAck,
}

#[derive(Clone, Copy)]
pub(super) struct HttpBudget<'a> {
    deadline: tokio::time::Instant,
    pub(super) response_bytes: u64,
    component: &'a str,
}

pub(crate) fn validate(limits: Option<&ComponentHttpLimits>) -> anyhow::Result<()> {
    if let Some(limits) = limits {
        anyhow::ensure!(
            limits
                .max_response_bytes
                .is_none_or(|n| (1..=MAX_RESPONSE_BYTES).contains(&n)),
            crate::component_rt::loader::RuntimeConfigurationFault::error(
                "http_limits.max_response_bytes"
            )
        );
        anyhow::ensure!(
            limits
                .timeout_ms
                .is_none_or(|n| (1..=MAX_TIMEOUT_MS).contains(&n)),
            crate::component_rt::loader::RuntimeConfigurationFault::error("http_limits.timeout_ms")
        );
    }
    Ok(())
}

pub(super) fn validate_http_limits(template: &ComponentTemplate) -> anyhow::Result<()> {
    validate(template.request.http_limits.as_ref())?;
    if let Some(prepare) = &template.prepare {
        validate(prepare.request.http_limits.as_ref())?;
    }
    if let Some(poll) = &template.async_poll {
        validate(poll.request.http_limits.as_ref())?;
        if let Some(request) = &poll.result_request {
            validate(request.http_limits.as_ref())?;
        }
        if let Some(download) = &poll.result_download {
            validate(download.http_limits.as_ref())?;
        }
        if let Some(reconcile) = &poll.reconcile {
            validate(reconcile.request.http_limits.as_ref())?;
        }
    }
    if let Some(upload) = &template.source_upload {
        validate(upload.http_limits.as_ref())?;
    }
    Ok(())
}

impl<'a> HttpBudget<'a> {
    pub(super) fn new(
        limits: Option<&ComponentHttpLimits>,
        kind: HttpResponseKind,
        component: &'a str,
    ) -> anyhow::Result<Self> {
        validate(limits)?;
        let (bytes, millis) = match kind {
            HttpResponseKind::Json => (16 * 1024 * 1024, 60_000),
            HttpResponseKind::Binary => (512 * 1024 * 1024, 900_000),
            HttpResponseKind::UploadAck => (1024 * 1024, 600_000),
        };
        let millis = limits.and_then(|l| l.timeout_ms).unwrap_or(millis);
        let deadline = tokio::time::Instant::now()
            .checked_add(Duration::from_millis(millis))
            .ok_or_else(|| {
                crate::component_rt::loader::RuntimeConfigurationFault::error(
                    "http_limits.timeout_ms",
                )
            })?;
        Ok(Self {
            deadline,
            response_bytes: limits.and_then(|l| l.max_response_bytes).unwrap_or(bytes),
            component,
        })
    }

    fn expired(&self) -> anyhow::Error {
        anyhow!(
            "component HTTP phase timed out (component={})",
            self.component
        )
    }

    pub(super) fn relabel<'b>(&self, component: &'b str) -> HttpBudget<'b> {
        HttpBudget {
            deadline: self.deadline,
            response_bytes: self.response_bytes,
            component,
        }
    }

    pub(super) fn response_ceiling(mut self, ceiling: u64) -> Self {
        self.response_bytes = self.response_bytes.min(ceiling);
        self
    }

    pub(super) async fn wait<T>(
        &self,
        future: impl std::future::Future<Output = T>,
    ) -> anyhow::Result<T> {
        anyhow::ensure!(tokio::time::Instant::now() < self.deadline, self.expired());
        let result = tokio::time::timeout_at(self.deadline, future)
            .await
            .map_err(|_| self.expired())?;
        anyhow::ensure!(tokio::time::Instant::now() < self.deadline, self.expired());
        Ok(result)
    }

    pub(super) async fn read(&self, mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
        let limit_error = || {
            anyhow!(
                "component response byte limit exceeded (component={})",
                self.component
            )
        };
        if response
            .content_length()
            .is_some_and(|length| length > self.response_bytes)
        {
            return Err(limit_error());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = self.wait(response.chunk()).await?.map_err(|error| {
            component_transport_error("component response read", self.component, error)
        })? {
            let length = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(limit_error)?;
            anyhow::ensure!(u64::try_from(length)? <= self.response_bytes, limit_error());
            bytes.try_reserve_exact(chunk.len()).map_err(|_| {
                anyhow!(
                    "component response allocation failed (component={})",
                    self.component
                )
            })?;
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}
