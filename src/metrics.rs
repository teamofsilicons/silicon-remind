//! Low-cardinality Prometheus metrics shared by API and worker processes.

use std::sync::Arc;

use prometheus_client::{
    encoding::{EncodeLabelSet, text::encode},
    metrics::{counter::Counter, family::Family},
    registry::Registry,
};

/// HTTP outcome labels which deliberately exclude tenant and resource IDs.
#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct HttpLabels {
    /// Matched route template, never the raw path.
    pub route: String,
    /// HTTP method.
    pub method: String,
    /// Coarse status class such as `2xx` or `5xx`.
    pub status_class: String,
}

/// Process metrics with cloned handles registered in one immutable registry.
#[derive(Clone, Debug)]
pub struct Metrics {
    registry: Arc<Registry>,
    /// Total HTTP responses by low-cardinality route/method/status labels.
    pub http_requests: Family<HttpLabels, Counter>,
    /// Durable occurrences created by the scheduler.
    pub executions_materialized: Counter,
    /// Hook events durably accepted.
    pub deliveries_succeeded: Counter,
    /// Hook attempts scheduled for retry.
    pub deliveries_retried: Counter,
    /// Hook executions reaching terminal failure.
    pub deliveries_failed: Counter,
    /// Worker iterations that failed before completing their stage.
    pub worker_errors: Counter,
}

impl Metrics {
    /// Creates and registers one independent process metric set.
    #[must_use]
    pub fn new() -> Self {
        let mut registry = Registry::default();
        let http_requests = Family::<HttpLabels, Counter>::default();
        let executions_materialized = Counter::default();
        let deliveries_succeeded = Counter::default();
        let deliveries_retried = Counter::default();
        let deliveries_failed = Counter::default();
        let worker_errors = Counter::default();

        registry.register(
            "remind_http_requests",
            "Total HTTP responses by route, method, and status class.",
            http_requests.clone(),
        );
        registry.register(
            "remind_executions_materialized",
            "Total durable schedule occurrences materialized.",
            executions_materialized.clone(),
        );
        registry.register(
            "remind_deliveries_succeeded",
            "Total reminders durably accepted by Silicon Hook.",
            deliveries_succeeded.clone(),
        );
        registry.register(
            "remind_deliveries_retried",
            "Total Hook delivery attempts scheduled for retry.",
            deliveries_retried.clone(),
        );
        registry.register(
            "remind_deliveries_failed",
            "Total Hook executions reaching terminal failure.",
            deliveries_failed.clone(),
        );
        registry.register(
            "remind_worker_errors",
            "Total worker stage iterations that failed.",
            worker_errors.clone(),
        );

        Self {
            registry: Arc::new(registry),
            http_requests,
            executions_materialized,
            deliveries_succeeded,
            deliveries_retried,
            deliveries_failed,
            worker_errors,
        }
    }

    /// Encodes the current metric snapshot in Prometheus text format.
    ///
    /// # Errors
    ///
    /// Returns an error only when the formatter rejects output.
    pub fn encode(&self) -> anyhow::Result<String> {
        let mut output = String::new();
        encode(&mut output, &self.registry)?;
        Ok(output)
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Metrics;

    #[test]
    fn encodes_registered_metrics() -> anyhow::Result<()> {
        let metrics = Metrics::new();
        metrics.executions_materialized.inc();
        let encoded = metrics.encode()?;
        assert!(encoded.contains("remind_executions_materialized_total 1"));
        Ok(())
    }
}
