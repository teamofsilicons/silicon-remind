//! Durable scheduler, delivery, and retention worker loops.

use std::{future::IntoFuture as _, sync::Arc, time::Duration};

use anyhow::Context as _;
use tokio::time::{Instant, MissedTickBehavior};
use uuid::Uuid;

use crate::{
    application::ports::{Clock, SystemClock},
    config::Settings,
    infrastructure::{
        crypto::SecretCipherKeyring,
        hook::HookClient,
        postgres::{self, PostgresRepository},
    },
    metrics::Metrics,
};

pub mod delivery;
mod health;
pub mod retention;
pub mod scheduler;

struct WorkerRuntime {
    tests: Option<crate::infrastructure::testing::TestEnvironments>,
    repository: PostgresRepository,
    encryption: SecretCipherKeyring,
    delivery: delivery::DeliveryProcessor,
    worker_id: String,
    batch_size: u32,
    retention_batch_size: u32,
    poll_interval: Duration,
    retention_interval: Duration,
    clock: Arc<dyn Clock>,
    metrics: Metrics,
}

/// Runs durable scheduling, Hook delivery, and retention until shutdown.
///
/// # Errors
///
/// Returns startup/configuration errors. Transient loop failures are observed,
/// logged, and retried rather than terminating the process.
pub async fn run(settings: Settings) -> anyhow::Result<()> {
    let pool = postgres::connect(&settings.database).await?;
    let repository = PostgresRepository::new(pool.clone());
    let encryption = SecretCipherKeyring::from_base64url(
        settings.encryption.current_version,
        &settings.encryption.keys,
    )?;
    let hook_client = HookClient::new(
        settings.hook.connect_timeout,
        settings.hook.request_timeout,
        settings.hook.max_response_bytes,
    )?;
    let worker_id = format!("remind-worker-{}", Uuid::now_v7());
    let batch_size = u32::try_from(settings.worker.batch_size.get())?;
    let delivery_concurrency = u32::try_from(settings.worker.max_delivery_concurrency.get())?;
    let retention_batch_size = u32::try_from(settings.retention.batch_size.get())?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let metrics = Metrics::new();
    let delivery = delivery::DeliveryProcessor::new(
        repository.clone(),
        hook_client,
        encryption.clone(),
        delivery::DeliveryProcessorConfig {
            hook_base_url: settings.hook.base_url.clone(),
            worker_id: worker_id.clone(),
            lease_duration: settings.worker.lease_duration,
            max_concurrency: delivery_concurrency,
            retry: settings.retry.clone(),
        },
        clock.clone(),
        metrics.clone(),
    );
    let operational_listener =
        tokio::net::TcpListener::bind(settings.worker.operational_bind_addr).await?;
    let tests = match &settings.testing_database {
        Some(database) => Some(
            crate::infrastructure::testing::TestEnvironments::connect(
                database,
                &settings.iam,
                encryption.clone(),
            )
            .await?,
        ),
        None => None,
    };
    let runtime = WorkerRuntime {
        tests,
        repository,
        encryption,
        delivery,
        worker_id,
        batch_size,
        retention_batch_size,
        poll_interval: settings.worker.poll_interval,
        retention_interval: settings.retention.sweep_interval,
        clock,
        metrics,
    };
    let run_result = serve_worker(
        &runtime,
        operational_listener,
        settings.worker.operational_bind_addr,
        settings.server.shutdown_timeout,
    )
    .await;
    let close_result = tokio::time::timeout(settings.server.shutdown_timeout, pool.close()).await;
    run_result?;
    if close_result.is_err() {
        anyhow::bail!("worker database shutdown deadline exceeded");
    }
    Ok(())
}

fn operational_router(runtime: &WorkerRuntime) -> axum::Router {
    health::router(
        runtime.repository.pool().clone(),
        runtime.metrics.clone(),
        runtime.tests.clone(),
    )
}

async fn serve_worker(
    runtime: &WorkerRuntime,
    operational_listener: tokio::net::TcpListener,
    operational_bind_addr: std::net::SocketAddr,
    shutdown_timeout: Duration,
) -> anyhow::Result<()> {
    let mut work_interval = tokio::time::interval(runtime.poll_interval);
    work_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut retention = tokio::time::interval_at(
        Instant::now() + runtime.retention_interval,
        runtime.retention_interval,
    );
    retention.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let shutdown = crate::shutdown::signal();
    tokio::pin!(shutdown);

    let operational_app = operational_router(runtime);
    let (operational_shutdown_sender, operational_shutdown_receiver) =
        tokio::sync::oneshot::channel::<()>();
    let mut operational_server = tokio::spawn(
        axum::serve(operational_listener, operational_app)
            .with_graceful_shutdown(async {
                let _ = operational_shutdown_receiver.await;
            })
            .into_future(),
    );

    tracing::info!(
        worker.id = %runtime.worker_id,
        operational.address = %operational_bind_addr,
        delivery.max_concurrency = runtime.delivery.max_concurrency(),
        "Silicon Remind worker started"
    );
    let mut operational_failure = None;
    let mut test_cursor = None;
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            result = &mut operational_server => {
                operational_failure = Some(match result {
                    Ok(Ok(())) => anyhow::anyhow!(
                        "worker operational HTTP server stopped unexpectedly"
                    ),
                    Ok(Err(error)) => anyhow::Error::new(error)
                        .context("worker operational HTTP server failed"),
                    Err(error) => anyhow::Error::new(error)
                        .context("worker operational HTTP task failed"),
                });
                break;
            }
            _ = work_interval.tick() => {
                run_work_cycle(
                    &runtime.repository,
                    &runtime.delivery,
                    &runtime.worker_id,
                    runtime.batch_size,
                    runtime.clock.as_ref(),
                    &runtime.metrics,
                ).await;
                work_test_environments(runtime, &mut test_cursor).await;
            }
            _ = retention.tick() => {
                sweep_test_environments(runtime).await;
                match retention::sweep(
                    &runtime.repository,
                    &runtime.encryption,
                    &runtime.worker_id,
                    runtime.clock.now(),
                    runtime.retention_batch_size,
                ).await {
                    Ok(result) if result != retention::RetentionResult::default() => {
                        tracing::info!(
                            schedules = result.schedules,
                            deleted_reminders_logged = result.deleted_reminders_logged,
                            deleted_reminders_trimmed = result.deleted_reminders_trimmed,
                            idempotency_records = result.idempotency_records,
                            hook_destinations = result.hook_destinations,
                            destinations_rewrapped = result.destinations_rewrapped,
                            "retention sweep processed expired records"
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        runtime.metrics.worker_errors.inc();
                        tracing::error!(error = %error, "retention sweep failed");
                    }
                }
            }
        }
    }

    if operational_failure.is_none() {
        let _ = operational_shutdown_sender.send(());
        let server_result = tokio::time::timeout(shutdown_timeout, &mut operational_server)
            .await
            .map_err(|_| anyhow::anyhow!("worker HTTP graceful shutdown deadline exceeded"))?
            .map_err(|error| {
                anyhow::Error::new(error).context("worker operational HTTP task failed")
            })?;
        server_result.context("worker operational HTTP server failed")?;
    }
    match operational_failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn run_work_cycle(
    repository: &PostgresRepository,
    delivery: &delivery::DeliveryProcessor,
    worker_id: &str,
    batch_size: u32,
    clock: &dyn Clock,
    metrics: &Metrics,
) {
    match repository
        .cleanup_revoked_resources(clock.now(), batch_size, worker_id)
        .await
    {
        Ok(result) if result != postgres::RevokedResourceCleanup::default() => {
            tracing::info!(
                schedules_deleted = result.schedules_deleted,
                executions_failed = result.executions_failed,
                destinations_disabled = result.destinations_disabled,
                "cleaned resources blocked by IAM lifecycle tombstones"
            );
        }
        Ok(_) => {}
        Err(error) => {
            metrics.worker_errors.inc();
            tracing::error!(error = %error, "revoked-resource cleanup failed");
        }
    }

    match scheduler::materialize_due(repository, worker_id, clock.now(), batch_size, metrics).await
    {
        Ok(count) if count > 0 => {
            tracing::debug!(count, "materialized due reminder executions");
        }
        Ok(_) => {}
        Err(error) => {
            metrics.worker_errors.inc();
            tracing::error!(error = %error, "schedule materialization failed");
        }
    }

    match delivery.run_once().await {
        Ok(count) if count > 0 => tracing::debug!(count, "processed Hook delivery batch"),
        Ok(_) => {}
        Err(error) => {
            metrics.worker_errors.inc();
            tracing::error!(error = %error, "Hook delivery batch had failures");
        }
    }
}

async fn run_test_cycles(
    runtime: &WorkerRuntime,
    tests: &crate::infrastructure::testing::TestEnvironments,
    cursor: &mut Option<Uuid>,
) -> Result<(), crate::error::AppError> {
    let ids = tests.active_ids(*cursor).await?;
    if ids.is_empty() {
        *cursor = None;
        return Ok(());
    }
    // Rotate through bounded pages, keeping production work between pages.
    for id in ids.into_iter().take(8) {
        *cursor = Some(id);
        if let Some(lease) = tests.enter_worker(id).await? {
            let repository = PostgresRepository::new(lease.pool.clone());
            let delivery = runtime.delivery.with_repository(repository.clone());
            run_work_cycle(
                &repository,
                &delivery,
                &runtime.worker_id,
                runtime.batch_size,
                runtime.clock.as_ref(),
                &runtime.metrics,
            )
            .await;
            retention::sweep(
                &repository,
                &runtime.encryption,
                &runtime.worker_id,
                runtime.clock.now(),
                runtime.retention_batch_size,
            )
            .await
            .map_err(|e| crate::error::AppError::internal("test_retention", e))?;
            lease.finish(false).await?;
        }
    }
    Ok(())
}

async fn sweep_test_environments(runtime: &WorkerRuntime) {
    if let Some(tests) = &runtime.tests
        && let Err(error) = tests.sweep().await
    {
        runtime.metrics.worker_errors.inc();
        tracing::error!(error = %error, "test environment lifecycle sweep failed");
    }
}

async fn work_test_environments(runtime: &WorkerRuntime, test_cursor: &mut Option<Uuid>) {
    if let Some(tests) = &runtime.tests
        && let Err(error) = run_test_cycles(runtime, tests, test_cursor).await
    {
        runtime.metrics.worker_errors.inc();
        tracing::error!(error = %error, "test environment work cycle failed");
    }
}
