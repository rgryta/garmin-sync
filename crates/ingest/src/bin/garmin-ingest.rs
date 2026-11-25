//! The ingest daemon: `garmin-ingest`.
//!
//! It never does the interactive SSO login. It refreshes the seeded session, polls on
//! every poll interval, and between polls backfills the whole history under a daily
//! budget, spread over the day. A rejected session is recorded and waited on; the process
//! does not exit.

use std::process::ExitCode;
use std::time::{Duration, Instant};

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
        // The runner outlives a failed pass, so its daily-poll clock does too; only a
        // rejected session builds a new one from a fresh seed.
        loop {
            match run(&mut runner, &pool, &settings).await {
                RunError::Rejected => {
                    tracing::error!("session rejected; waiting for a fresh seed");
                    let _ =
                        sync_store::record_auth_error(&pool, &settings.account, "session rejected")
                            .await;
                    tokio::time::sleep(settings.auth_retry).await;
                    break;
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
}

async fn run(
    runner: &mut Runner<Client>,
    pool: &sqlx::postgres::PgPool,
    settings: &IngestSettings,
) -> RunError {
    // Backfill requests are spread over the day instead of spent in a burst at midnight.
    let pace = Duration::from_secs((86_400 / settings.backfill_budget.max(1)).max(1) as u64);
    let mut next_pass = Instant::now();
    loop {
        if Instant::now() >= next_pass {
            match runner.pass().await {
                Ok(report) => tracing::info!(
                    activities = report.activities,
                    samples = report.samples,
                    daily = report.daily,
                    "pass complete"
                ),
                Err(e) => return e,
            }
            next_pass = Instant::now() + settings.poll_interval;
        }
        let wait = match runner.backfill_step().await {
            Ok(BackfillStep::Progressed) => pace,
            Ok(BackfillStep::Done | BackfillStep::BudgetSpent) => {
                next_pass.saturating_duration_since(Instant::now())
            }
            Err(e) => return e,
        };
        let _ = sync_store::clear_auth_error(pool, &settings.account).await;
        tokio::time::sleep(wait).await;
    }
}
