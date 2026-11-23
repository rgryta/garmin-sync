//! The ingest daemon: `garmin-ingest`.
//!
//! It never does the interactive SSO login. It refreshes the seeded session, backfills
//! the whole history under a daily budget, then polls. A rejected session is recorded and
//! waited on; the process does not exit.

use std::process::ExitCode;

use garmin_client::Client;
use garmin_core::db::{create_pool, ensure_schema};
use garmin_core::store::sync as sync_store;
use garmin_ingest::config::IngestSettings;
use garmin_ingest::runner::{BackfillStep, LOOP, RunError, Runner, load_session};

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "garmin_ingest=info,info".into()),
        )
        .init();

    let settings = match IngestSettings::from_env() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let pool = match create_pool(settings.dsn.expose()).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot connect to the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = ensure_schema(&pool).await {
        eprintln!("cannot apply the schema: {e}");
        return ExitCode::FAILURE;
    }

    loop {
        let session = match load_session(&pool, &settings.account, settings.session_from.as_deref())
            .await
        {
            Ok(Some(s)) => s,
            Ok(None) => {
                tracing::warn!("no session: seed one, then I will pick it up");
                let _ =
                    sync_store::record_auth_error(&pool, &settings.account, "no session seeded")
                        .await;
                tokio::time::sleep(settings.auth_retry).await;
                continue;
            }
            Err(e) => {
                tracing::error!(%e, "cannot load the session");
                tokio::time::sleep(settings.poll_interval).await;
                continue;
            }
        };
        let mut runner = Runner::new(
            pool.clone(),
            settings.account.clone(),
            settings.backfill_budget,
            Client::new(),
            session,
        );
        match run(&mut runner, &pool, &settings).await {
            RunError::Rejected => {
                tracing::error!("session rejected; waiting for a fresh seed");
                let _ = sync_store::record_auth_error(&pool, &settings.account, "session rejected")
                    .await;
                tokio::time::sleep(settings.auth_retry).await;
            }
            RunError::Throttled { retry_after } => {
                let wait = retry_after.unwrap_or(600);
                tracing::warn!(wait, "throttled; backing off");
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }
            other => {
                tracing::error!(%other, "pass failed; retrying after the poll interval");
                let _ = sync_store::record_error(&pool, LOOP, &other.to_string()).await;
                tokio::time::sleep(settings.poll_interval).await;
            }
        }
    }
}

async fn run(
    runner: &mut Runner<Client>,
    pool: &sqlx::postgres::PgPool,
    settings: &IngestSettings,
) -> RunError {
    loop {
        match runner.backfill_step().await {
            Ok(BackfillStep::Done) => {}
            Ok(BackfillStep::Progressed) => {
                // The daily budget is the throttle; keep going, gently spaced.
                let _ = sync_store::clear_auth_error(pool, &settings.account).await;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
            Ok(BackfillStep::BudgetSpent) => {
                let _ = sync_store::clear_auth_error(pool, &settings.account).await;
                tokio::time::sleep(settings.poll_interval).await;
                continue;
            }
            Err(e) => return e,
        }
        match runner.pass().await {
            Ok(report) => {
                let _ = sync_store::clear_auth_error(pool, &settings.account).await;
                tracing::info!(
                    activities = report.activities,
                    samples = report.samples,
                    daily = report.daily,
                    "pass complete"
                );
            }
            Err(e) => return e,
        }
        tokio::time::sleep(settings.poll_interval).await;
    }
}
