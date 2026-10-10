//! HTTP API composition and transport adapters.

use std::{future::IntoFuture as _, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, header},
    middleware as axum_middleware,
    routing::{delete, get, post},
};
use secrecy::SecretString;
use sqlx::PgPool;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    catch_panic::CatchPanicLayer, sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::TraceLayer,
};

use crate::{
    application::{ports::SystemClock, schedules::ScheduleService, sharing::SharingService},
    config::{ProofIssuers, RuntimeEnvironment, Settings},
    infrastructure::{
        accounts::AccountsGateway,
        crypto::SecretCipherKeyring,
        identity::IdentityStore,
        postgres::{self, PostgresRepository},
    },
    metrics::Metrics,
};

mod budget;
pub mod contracts;
pub mod handlers;
pub mod middleware;
pub mod models;
#[cfg(test)]
mod tests;

/// Cloneable dependencies shared by HTTP handlers and middleware.
#[derive(Clone, Debug)]
pub struct ApiState {
    pub(crate) tests: Option<crate::infrastructure::testing::TestEnvironments>,
    pub(crate) idempotency_retention: Duration,
    pub(crate) schedules: ScheduleService,
    pub(crate) sharing: SharingService,
    pub(crate) repository: PostgresRepository,
    pub(crate) identity: IdentityStore,
    pub(crate) proof_issuers: ProofIssuers,
    pub(crate) webhook_secrets: Vec<SecretString>,
    pub(crate) encryption: SecretCipherKeyring,
    pub(crate) is_test: bool,
    pub(crate) reports_enabled: bool,
    pub(crate) telemetry: crate::telemetry::Recorder,
    pub(crate) environment: RuntimeEnvironment,
    pub(crate) metrics: Metrics,
    pub(crate) request_timeout: Duration,
    pub(crate) request_budget: budget::RequestBudget,
}

impl ApiState {
    /// Composes API dependencies over an initialized PostgreSQL pool.
    ///
    /// # Errors
    ///
    /// Returns an error when an outbound client or the encryption keyring cannot
    /// be constructed from validated configuration.
    pub fn new(settings: &Settings, pool: PgPool) -> anyhow::Result<Self> {
        let repository = PostgresRepository::new(pool.clone());
        let gateway = AccountsGateway::new(&settings.accounts)?;
        let identity = IdentityStore::new(pool, gateway, settings.accounts.lookup_ttl);
        let schedules = ScheduleService::new(
            repository.clone(),
            identity.clone(),
            Arc::new(SystemClock),
            settings.retention.idempotency_retention,
        );
        let encryption = SecretCipherKeyring::from_base64url(
            settings.encryption.current_version,
            &settings.encryption.keys,
        )?;
        Ok(Self {
            tests: None,
            idempotency_retention: settings.retention.idempotency_retention,
            schedules,
            sharing: SharingService::new(identity.clone()),
            repository,
            identity,
            proof_issuers: settings.accounts.proof_issuers.clone(),
            webhook_secrets: settings.accounts.webhook_secrets.clone(),
            encryption,
            is_test: false,
            telemetry: crate::telemetry::Recorder::new(settings),
            reports_enabled: settings.postmark_server_token.is_some(),
            environment: settings.environment,
            metrics: Metrics::new(),
            request_timeout: settings.server.request_timeout,
            request_budget: budget::RequestBudget::default(),
        })
    }
}

/// Builds the complete public and operational router.
#[allow(
    clippy::too_many_lines,
    reason = "The route table reads best as one declaration"
)]
pub fn router(state: ApiState, settings: &Settings) -> Router {
    let public_api = Router::new()
        .route("/telemetry/events", post(handlers::telemetry::record))
        .route("/reports", post(handlers::reports::create))
        .route("/reports/{id}", get(handlers::reports::get))
        .route(
            "/schedules",
            get(handlers::schedules::list)
                .post(handlers::schedules::create)
                .patch(handlers::schedules::update_statuses),
        )
        .route(
            "/schedules/{schedule_id}",
            get(handlers::schedules::get)
                .patch(handlers::schedules::patch)
                .delete(handlers::schedules::delete),
        )
        .route(
            "/schedules/{schedule_id}/executions",
            get(handlers::schedules::list_executions),
        )
        .route(
            "/webhook",
            get(handlers::destination::get)
                .put(handlers::destination::set)
                .delete(handlers::destination::disable),
        )
        .route(
            "/webhooks",
            get(handlers::destination::list).post(handlers::destination::subscribe),
        )
        .route(
            "/webhooks/{subscription_id}",
            delete(handlers::destination::unsubscribe),
        )
        .route("/silicons", get(handlers::accounts::silicons))
        .route("/auth/me", get(handlers::accounts::me))
        .route(
            "/viewers",
            get(handlers::sharing::list_viewers).post(handlers::sharing::grant_viewer),
        )
        .route(
            "/viewers/{viewer}",
            delete(handlers::sharing::revoke_viewer),
        )
        .route(
            "/allowed-accounts",
            get(handlers::sharing::list_allowed).post(handlers::sharing::allow_account),
        )
        .route(
            "/allowed-accounts/{account}",
            delete(handlers::sharing::disallow_account),
        )
        .merge(test_environment_routes())
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate,
        ));

    Router::new()
        .route("/api/versions", get(contracts::versions))
        .route("/health/live", get(handlers::health::live))
        .route("/health/ready", get(handlers::health::ready))
        .route("/metrics", get(handlers::health::metrics))
        .route("/webhook", post(handlers::accounts_webhook::receive))
        .route("/webhook/", post(handlers::accounts_webhook::receive))
        .route(
            "/api/v2/testing-environment",
            get(handlers::testing::test_only),
        )
        .route(
            "/api/v2/testing-environment/cleanings",
            post(handlers::testing::test_only),
        )
        .nest("/api/v2", public_api)
        .fallback(handlers::not_found)
        .method_not_allowed_fallback(handlers::method_not_allowed)
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::observe,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            contracts::negotiate,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            handlers::testing::select,
        ))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            HeaderName::from_static("x-remind-test-key"),
            HeaderName::from_static("x-hook-signature"),
            HeaderName::from_static("x-accounts-signature"),
        ]))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::enforce_timeout,
        ))
        .layer(ConcurrencyLimitLayer::new(
            settings.server.max_concurrent_requests.get(),
        ))
        .layer(CatchPanicLayer::custom(middleware::panic_response))
        .layer(TraceLayer::new_for_http())
        .layer(DefaultBodyLimit::max(settings.server.max_body_bytes))
        .layer(axum_middleware::from_fn(middleware::request_context))
        .with_state(state)
}

/// Connects dependencies and serves HTTP until graceful shutdown completes.
///
/// # Errors
///
/// Returns startup, listener, server, or shutdown-deadline errors.
pub async fn serve(settings: Settings) -> anyhow::Result<()> {
    let pool = postgres::connect(&settings.database).await?;
    let mut state = ApiState::new(&settings, pool.clone())?;
    if let Some(database) = &settings.testing_database {
        state.tests = Some(
            crate::infrastructure::testing::TestEnvironments::connect(
                database,
                state.encryption.clone(),
            )
            .await?,
        );
    }
    let app = router(state, &settings);
    let listener = tokio::net::TcpListener::bind(settings.server.bind_addr).await?;
    tracing::info!(address = %settings.server.bind_addr, "Silicon Remind API listening");

    let (shutdown_sender, shutdown_receiver) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown_receiver.await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => result?,
        () = crate::shutdown::signal() => {
            let _ = shutdown_sender.send(());
            tokio::time::timeout(settings.server.shutdown_timeout, &mut server)
                .await
                .map_err(|_| anyhow::anyhow!("HTTP graceful shutdown deadline exceeded"))??;
        }
    }
    pool.close().await;
    Ok(())
}

/// Dependencies selected from the authenticated test environment boundary.
pub struct ScopedState(pub ApiState);

impl axum::extract::FromRequestParts<ApiState> for ScopedState {
    type Rejection = std::convert::Infallible;
    fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &ApiState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        std::future::ready(Ok(Self(state.scoped(&parts.extensions))))
    }
}

impl ApiState {
    /// Points data access at the selected test environment, if any. Identity
    /// stays in the production database: Silicon Accounts has no test copies.
    pub(crate) fn scoped(&self, extensions: &http::Extensions) -> Self {
        let mut state = self.clone();
        if let Some(context) = extensions.get::<handlers::testing::EnvironmentContext>() {
            state.is_test = true;
            state.repository = PostgresRepository::new(context.pool.clone());
            state.schedules = ScheduleService::new(
                state.repository.clone(),
                self.identity.clone(),
                Arc::new(SystemClock),
                self.idempotency_retention,
            );
        }
        state
    }
}

fn test_environment_routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/test-environments",
            get(handlers::testing::list).post(handlers::testing::create),
        )
        .route(
            "/test-environments/{id}",
            get(handlers::testing::get).delete(handlers::testing::delete),
        )
        .route("/test-environments/{id}/key", get(handlers::testing::key))
        .route(
            "/test-environments/{id}/key-rotations",
            post(handlers::testing::rotate),
        )
        .route(
            "/test-environments/{id}/restorations",
            post(handlers::testing::restore),
        )
}
