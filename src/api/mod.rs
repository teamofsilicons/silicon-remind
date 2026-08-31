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
use url::Url;

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
    pub(crate) schedules: ScheduleService,
    pub(crate) repository: PostgresRepository,
    pub(crate) iam: IamClient,
    pub(crate) iam_webhook: IamWebhookVerifier,
    pub(crate) internal_api_token: SecretString,
    pub(crate) encryption: SecretCipherKeyring,
    pub(crate) hook_base_url: Url,
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
        let iam = IamClient::new(
            settings.iam.introspection_url.clone(),
            &settings.iam.app_id,
            &settings.iam.app_secret,
            settings.iam.connect_timeout,
            settings.iam.request_timeout,
            settings.iam.max_response_bytes,
        )?;
        let encryption = SecretCipherKeyring::from_base64url(
            settings.encryption.current_version,
            &settings.encryption.keys,
        )?;
        let iam_webhook = IamWebhookVerifier::from_base64url(&settings.iam.webhook_keys)?;
        Ok(Self {
            schedules,
            repository,
            iam,
            iam_webhook,
            internal_api_token: settings.internal_api.bearer_token.clone(),
            encryption,
            hook_base_url: settings.hook.base_url.clone(),
            environment: settings.environment,
            metrics: Metrics::new(),
            request_timeout: settings.server.request_timeout,
        })
    }
}

/// Builds the complete public, internal, and operational router.
pub fn router(state: ApiState, settings: &Settings) -> Router {
    let public_api = Router::new()
        .route(
            "/schedules",
            get(handlers::schedules::list).post(handlers::schedules::create),
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
        .nest("/api/v1", public_api)
        .nest("/internal/v1", internal_api)
        .fallback(handlers::not_found)
        .method_not_allowed_fallback(handlers::method_not_allowed)
        .layer(axum_middleware::from_fn_with_state(
            state.clone(),
            middleware::observe,
        ))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
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
    let state = ApiState::new(&settings, pool.clone())?;
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
