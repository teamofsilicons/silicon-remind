//! Worker liveness, schema readiness, and process metrics endpoints.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse as _, Response},
    routing::get,
};
use serde::Serialize;
use sqlx::PgPool;

use crate::{infrastructure::postgres, metrics::Metrics};

#[derive(Clone, Debug)]
struct OperationalState {
    pool: PgPool,
    metrics: Metrics,
}

/// Builds the worker's unauthenticated, operational-only HTTP surface.
pub(crate) fn router(pool: PgPool, metrics: Metrics) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics_response))
        .with_state(OperationalState { pool, metrics })
}

async fn live() -> Json<HealthResponse> {
    Json(HealthResponse::healthy())
}

async fn ready(State(state): State<OperationalState>) -> Response {
    match postgres::health_check(&state.pool).await {
        Ok(()) => live().await.into_response(),
        Err(error) => {
            tracing::warn!(error = %error, "worker readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(HealthResponse::unavailable()),
            )
                .into_response()
        }
    }
}

async fn metrics_response(State(state): State<OperationalState>) -> Response {
    match state.metrics.encode() {
        Ok(body) => {
            let mut response = (StatusCode::OK, body).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
            );
            response
        }
        Err(error) => {
            tracing::error!(error = %error, "worker metric encoding failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    service: &'static str,
    process: &'static str,
    status: &'static str,
    version: &'static str,
}

impl HealthResponse {
    const fn healthy() -> Self {
        Self {
            service: "silicon-remind",
            process: "worker",
            status: "ok",
            version: env!("CARGO_PKG_VERSION"),
        }
    }

    const fn unavailable() -> Self {
        Self {
            service: "silicon-remind",
            process: "worker",
            status: "unavailable",
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::postgres::PgPoolOptions;

    use crate::metrics::Metrics;

    use super::{HealthResponse, router};

    #[test]
    fn health_payload_identifies_worker_process() -> serde_json::Result<()> {
        let value = serde_json::to_value(HealthResponse::healthy())?;
        assert_eq!(value["service"], "silicon-remind");
        assert_eq!(value["process"], "worker");
        assert_eq!(value["status"], "ok");
        Ok(())
    }

    #[tokio::test]
    async fn operational_server_exposes_live_metrics_and_shuts_down() -> anyhow::Result<()> {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://remind:remind@127.0.0.1:9/remind")?;
        let metrics = Metrics::new();
        metrics.executions_materialized.inc();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(pool, metrics))
                .with_graceful_shutdown(async {
                    let _ = shutdown_receiver.await;
                })
                .await
        });

        let client = reqwest::Client::new();
        let live = client
            .get(format!("http://{address}/health/live"))
            .send()
            .await?;
        assert_eq!(live.status(), reqwest::StatusCode::OK);
        let payload = live.json::<serde_json::Value>().await?;
        assert_eq!(payload["process"], "worker");

        let response = client
            .get(format!("http://{address}/metrics"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; version=0.0.4; charset=utf-8")
        );
        assert!(
            response
                .text()
                .await?
                .contains("remind_executions_materialized_total 1")
        );

        let _ = shutdown_sender.send(());
        let joined = tokio::time::timeout(Duration::from_secs(2), server).await?;
        joined??;
        Ok(())
    }
}
