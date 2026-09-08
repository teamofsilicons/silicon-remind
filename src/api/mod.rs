//! HTTP API composition and transport adapters.

use std::{future::IntoFuture as _, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::{HeaderName, header},
    middleware as axum_middleware,
    routing::{delete, get, post, put},
};
use secrecy::SecretString;
use sqlx::PgPool;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{
    catch_panic::CatchPanicLayer, sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::TraceLayer,
};

use crate::{
    application::{ports::SystemClock, schedules::ScheduleService},
    config::{RuntimeEnvironment, Settings},
    infrastructure::{
        crypto::SecretCipherKeyring,
        iam::IamClient,
        iam_webhook::IamWebhookVerifier,
        postgres::{self, PostgresRepository},
    },
    metrics::Metrics,
};

pub mod handlers;
pub mod middleware;
pub mod models;

/// Cloneable dependencies shared by HTTP handlers and middleware.
#[derive(Clone, Debug)]
pub struct ApiState {
    pub(crate) tests: Option<crate::infrastructure::testing::TestEnvironments>,
    pub(crate) idempotency_retention: Duration,
    pub(crate) schedules: ScheduleService,
    pub(crate) repository: PostgresRepository,
    pub(crate) iam: IamClient,
    pub(crate) iam_webhook: IamWebhookVerifier,
    pub(crate) internal_api_token: SecretString,
    pub(crate) encryption: SecretCipherKeyring,
    pub(crate) is_test: bool,
    pub(crate) environment: RuntimeEnvironment,
    pub(crate) metrics: Metrics,
    pub(crate) request_timeout: Duration,
}

impl ApiState {
    /// Composes API dependencies over an initialized PostgreSQL pool.
    ///
    /// # Errors
    ///
    /// Returns an error when an outbound client or encryption keyring cannot be
    /// constructed from validated configuration.
    pub fn new(settings: &Settings, pool: PgPool) -> anyhow::Result<Self> {
        let repository = PostgresRepository::new(pool);
        let schedules = ScheduleService::new(
            repository.clone(),
            Arc::new(SystemClock),
            settings.retention.idempotency_retention,
        );
        let iam = IamClient::new(&settings.iam)?;
        let encryption = SecretCipherKeyring::from_base64url(
            settings.encryption.current_version,
            &settings.encryption.keys,
        )?;
        let iam_webhook = IamWebhookVerifier::from_keys(&settings.iam.webhook_keys)?;
        Ok(Self {
            tests: None,
            idempotency_retention: settings.retention.idempotency_retention,
            schedules,
            repository,
            iam,
            iam_webhook,
            internal_api_token: settings.internal_api.bearer_token.clone(),
            encryption,
            is_test: false,
            environment: settings.environment,
            metrics: Metrics::new(),
            request_timeout: settings.server.request_timeout,
        })
    }
}

/// Builds the complete public, internal, and operational router.
#[allow(clippy::too_many_lines)]
pub fn router(state: ApiState, settings: &Settings) -> Router {
    let public_api = Router::new()
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
        .route("/silicons", get(handlers::destination::silicons))
        .route("/auth/me", get(handlers::auth::me))
        .merge(test_environment_routes())
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate,
        ));

    let internal_provisioning_api = Router::new()
        .route(
            "/hook-destinations",
            put(handlers::internal::upsert_hook_destination),
        )
        .route(
            "/hook-destinations/{org_id}/{silicon_id}",
            delete(handlers::internal::disable_hook_destination),
        )
        .route_layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::authenticate_internal,
        ));
    let internal_api = Router::new()
        .merge(internal_provisioning_api)
        .route("/iam/events", post(handlers::internal::accept_iam_event));

    Router::new()
        .route("/health/live", get(handlers::health::live))
        .route("/health/ready", get(handlers::health::ready))
        .route("/metrics", get(handlers::health::metrics))
        .route("/webhook/", post(handlers::internal::accept_iam_event))
        .route("/api/v1/auth/login", post(handlers::auth::login))
        .route("/api/v1/auth/refresh", post(handlers::auth::refresh))
        .route("/api/v1/auth/logout", post(handlers::auth::logout))
        .route(
            "/api/v1/testing-environment/iam",
            axum::routing::put(handlers::testing::test_only),
        )
        .route(
            "/api/v1/testing-environment",
            get(handlers::testing::test_only),
        )
        .route(
            "/api/v1/testing-environment/cleanings",
            post(handlers::testing::test_only),
        )
        .nest("/api/v1", public_api)
        .nest("/internal/v1", internal_api)
        .fallback(handlers::not_found)
        .method_not_allowed_fallback(handlers::method_not_allowed)
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::observe,
        ))
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            handlers::testing::select,
        ))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            HeaderName::from_static("x-remind-test-key"),
            HeaderName::from_static("x-hook-signature"),
            HeaderName::from_static("x-silicon-iam-signature"),
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
                &settings.iam,
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
    pub(crate) fn scoped(&self, extensions: &http::Extensions) -> Self {
        let mut state = self.clone();
        if let Some(context) = extensions.get::<handlers::testing::EnvironmentContext>() {
            state.is_test = true;
            state.repository = PostgresRepository::new(context.pool.clone());
            state.schedules = ScheduleService::new(
                state.repository.clone(),
                Arc::new(SystemClock),
                self.idempotency_retention,
            );
            state.iam = context.iam.clone();
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
