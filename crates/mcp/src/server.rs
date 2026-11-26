//! The MCP server: streamable HTTP at `/mcp`, eight tools, no Garmin credential.
//!
//! The pool is opened before the listener binds and the shipped migrations are asserted
//! present — never applied. This process runs as a role with no DDL rights, and a schema
//! it cannot create is a schema it must not pretend to have.
//!
//! Seven of the eight tools are reads. The eighth writes the two alias tables and
//! nothing else, which is a database fact here rather than a convention: the grant in
//! `0001_init.sql` is all the write authority `garmin_mcp` has.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use garmin_core::config::McpSettings;
use garmin_core::db::{DbError, assert_migrations, create_pool};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, ErrorData, Implementation, ServerCapabilities, ServerConfig,
};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use sqlx::postgres::PgPool;

use crate::tools::{self, ToolError};

/// What the server tells a client it is.
pub const INSTRUCTIONS: &str = "\
Garmin Connect data, from a permanent archive in PostgreSQL written by a separate ingester
that reads Garmin's own API. Nothing here reaches Garmin live — only what has already been
archived, and nothing here can change anything on the account.

Two kinds of data: activities (each workout, with its per-second samples parsed from the
original FIT file — heart rate, power, cadence, speed, altitude, GPS) and daily metrics
(sleep, HRV, stress, body battery, steps, respiration, SpO2, training readiness, VO2 max,
weight, and a daily summary). Intraday readings parsed from each day's watch files live in
`wellness_sample` (kind, ts, value): heart_rate about every minute, hrv every 5 minutes
overnight, stress and respiration about every minute — for every day, including the older
days where the daily JSON holds no intraday detail. `device` is the account's registered
devices (the exact model name); its `id` is the `device` on activities and the `device_id`
on wellness rows. Timestamps are ISO 8601; GPS is present.

`activities` lists workouts, `activity` shows one with a sample summary, `day` shows every
metric for a date, `metric` follows one daily kind over time, and `sql` runs a read-only
query (the per-second samples live in `activity_sample`). `status` says how much is
archived — a gap can mean \"not backfilled yet\" rather than \"never recorded\".
";

/// The name the server reports to clients, which MCPHub joins to each tool name with a
/// `-`: `garmin-activities`.
pub const SERVER_NAME: &str = "garmin";

#[derive(Clone)]
pub struct GarminMcp {
    pool: PgPool,
}

/// Turn a tool's answer into a protocol response.
///
/// A usage error is the caller's and goes back as `invalid_params` with its own text. A
/// SQL error is ours: it is logged in full and the caller is told only that the read
/// failed, because a Postgres error string can carry the DSN and the data.
fn finish(result: Result<String, ToolError>) -> Result<CallToolResult, ErrorData> {
    match result {
        Ok(body) => Ok(CallToolResult::success(vec![ContentBlock::text(body)])),
        Err(ToolError::Usage(message)) => Err(ErrorData::invalid_params(message, None)),
        Err(ToolError::Sql(error)) => {
            tracing::error!(%error, "the archive could not be read");
            Err(ErrorData::internal_error(
                "the archive could not be read".to_string(),
                None,
            ))
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NoArgs {}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ActivitiesArgs {
    pub after: Option<String>,
    pub before: Option<String>,
    /// Filter by Garmin activity type key, e.g. running, cycling, strength_training.
    pub r#type: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ActivityArgs {
    pub id: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DayArgs {
    /// A date, YYYY-MM-DD.
    pub date: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MetricArgs {
    /// A daily kind: summary, sleep, hrv, heart_rate, stress, steps, body_battery,
    /// respiration, spo2, training_readiness, vo2max, weight, ...
    pub kind: String,
    pub after: Option<String>,
    pub before: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SqlArgs {
    /// A read-only SELECT over the archive. Runs in a READ ONLY transaction with a
    /// statement timeout and a row cap; writes and the session table are refused.
    pub query: String,
}

// Tool names are hyphenated and declared explicitly: MCPHub joins them to the server name
// with `-`, so these read `garmin-activities`. Every tool is read-only and says so.
#[tool_router]
impl GarminMcp {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[tool(
        name = "status",
        description = "Ingest health: what each loop last did, the backfill budget, and how many days \
                       of each daily metric are archived. Check here before concluding \"no data\" — a \
                       gap can mean \"not backfilled yet\".",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn status(&self, Parameters(_): Parameters<NoArgs>) -> Result<CallToolResult, ErrorData> {
        finish(tools::status(&self.pool).await)
    }

    #[tool(
        name = "activities",
        description = "List archived activities, newest first. `after`/`before` (ISO 8601 or \
                       YYYY-MM-DD) narrow the range; `type` filters by Garmin type key (running, \
                       cycling, strength_training, ...). Each row shows id, time, type, distance and \
                       whether it was manually started.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn activities(
        &self,
        Parameters(args): Parameters<ActivitiesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        finish(
            tools::activities_tool(
                &self.pool,
                args.after.as_deref(),
                args.before.as_deref(),
                args.r#type.as_deref(),
                args.limit,
            )
            .await,
        )
    }

    #[tool(
        name = "activity",
        description = "One activity's full summary plus a summary of its per-second samples (count, \
                       average and max heart rate, max altitude). The raw samples are not dumped; use \
                       `sql` over `activity_sample` for the time series.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn activity(
        &self,
        Parameters(args): Parameters<ActivityArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        finish(tools::activity_tool(&self.pool, args.id).await)
    }

    #[tool(
        name = "day",
        description = "Every daily metric archived for one date (YYYY-MM-DD): sleep, HRV, stress, \
                       body battery, steps, respiration, SpO2, training readiness, VO2 max, weight, \
                       and the daily summary.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn day(
        &self,
        Parameters(args): Parameters<DayArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        finish(tools::day_tool(&self.pool, &args.date).await)
    }

    #[tool(
        name = "metric",
        description = "One daily metric kind over a date range, one row per day. This is the tool for \
                       \"how has my sleep / HRV / VO2 max changed\". `kind` is one of the daily kinds \
                       `status` lists.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn metric(
        &self,
        Parameters(args): Parameters<MetricArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        finish(
            tools::metric_tool(
                &self.pool,
                &args.kind,
                args.after.as_deref(),
                args.before.as_deref(),
            )
            .await,
        )
    }

    #[tool(
        name = "sql",
        description = "Run a read-only SELECT over the archive when the other tools do not fit. \
                       Tables: activity, activity_sample (per-second), activity_file, daily_metric \
                       (kind,date,raw), wellness_sample (kind,ts,value,device_id: intraday heart_rate, \
                       hrv, stress, respiration), wellness_file, device (id = activity.device = \
                       wellness device_id; display_name), raw_response, sync_state. The query runs READ ONLY with a 10s \
                       timeout and a 500-row cap; writes and the session table are refused.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn sql(
        &self,
        Parameters(args): Parameters<SqlArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        finish(tools::sql_tool(&self.pool, &args.query).await)
    }
}

#[tool_handler]
impl ServerHandler for GarminMcp {
    /// Written out rather than left to `#[tool_handler(instructions = "…")]`: that
    /// argument takes a literal, and a literal folded to fit this file would drop the
    /// hard wrap the Python server sends. This is what keeps the two byte-identical.
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS.to_string())
    }
}

/// Own the pool and refuse an unmigrated database.
///
/// Separate from [`run`] so a test can call it without a listener.
pub async fn open_pool(settings: &McpSettings) -> Result<PgPool, DbError> {
    let pool = create_pool(settings.dsn.expose()).await?;
    if let Err(error) = assert_migrations(&pool).await {
        // This process runs as a role with no DDL rights. A schema it cannot create is
        // one it must not pretend to have: failing at startup names the problem, where
        // serving on regardless would fail one query at a time and blame the caller.
        pool.close().await;
        return Err(error);
    }
    Ok(pool)
}

/// The container healthcheck's endpoint: a live query, not a liveness constant.
///
/// The pool is opened before the listener binds, so by the time this can answer at all
/// there is a pool — the Python's "starting" case is a refused connection here, which
/// Docker reads as unhealthy just the same. What is left worth checking is the database,
/// because `compose.yaml` has `autoheal=true` on this container and a server that cannot
/// reach Postgres must not report itself well.
async fn healthz(pool: PgPool) -> Response {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&pool)
        .await
    {
        Ok(_) => "ok".into_response(),
        Err(error) => {
            tracing::error!(%error, "the healthcheck could not reach the database");
            (StatusCode::SERVICE_UNAVAILABLE, "database unreachable").into_response()
        }
    }
}

/// `/mcp` plus `/healthz`.
///
/// `/healthz` is a plain route on this router and only `/mcp` is nested onto the MCP
/// service, so the `Host` allowlist below does not apply to it and `--health-check`
/// needs no entry of its own.
/// `tests/archive/server.rs::healthz_answers_a_host_the_mcp_endpoint_rejects` holds
/// that: if it ever stops being true the healthcheck starts failing for a reason
/// nothing about the container would explain.
pub fn build_router(pool: PgPool, allowed_hosts: Vec<String>) -> Router {
    let probe = pool.clone();
    let service = StreamableHttpService::new(
        move || Ok(GarminMcp::new(pool.clone())),
        Arc::new(NeverSessionManager::default()),
        stateless_config(allowed_hosts),
    );
    Router::new()
        .route("/healthz", get(move || healthz(probe.clone())))
        .nest_service("/mcp", service)
}

fn stateless_config(allowed_hosts: Vec<String>) -> StreamableHttpServerConfig {
    let mut config = StreamableHttpServerConfig::default();
    // There is no per-client state to keep, and statelessness takes session affinity off
    // the table behind MCPHub — what `stateless_http=True` bought the Python server.
    config.legacy_session_mode = false;
    // rmcp's guard against DNS rebinding. An empty list would mean "answer any `Host`",
    // so the list handed in here is never empty — see `DEFAULT_ALLOWED_HOSTS`. MCPHub
    // addresses this server by container name, so production sets `garmin-mcp`.
    config.allowed_hosts = allowed_hosts;
    config
}

pub async fn run(settings: &McpSettings) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let pool = open_pool(settings).await?;
    let address: SocketAddr = format!("{}:{}", settings.host, settings.port).parse()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    // Logged because the failure mode is a server that boots clean and then refuses
    // every tool call with a 403: one line here turns a baffling outage into an obvious
    // one. `/healthz` is outside this list, so a container can look healthy while every
    // call is rejected.
    tracing::info!(
        allowed_hosts = %settings.allowed_hosts.join(", "),
        "answering /mcp only for these Host headers"
    );
    tracing::info!(%address, "serving the garmin archive over MCP");
    axum::serve(listener, build_router(pool, settings.allowed_hosts.clone()))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Probe our own `/healthz`, for the container healthcheck.
///
/// The image is `scratch`: no shell, no curl, no interpreter, so the healthcheck in
/// `deploy/compose.yaml` runs this binary with `--health-check` instead.
///
/// Hand-rolled over TCP rather than through an HTTP client crate: it reads one status
/// line from a loopback socket, and a dependency for that is a dependency to keep
/// patched for no gain. It is also the one thing in this binary that opens a socket at
/// all, which is why it speaks to loopback and nothing else.
pub async fn health_check(host: &str, port: u16) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // The bind address may be the wildcard, which is not a destination.
    let host = match host {
        "0.0.0.0" | "::" | "" => "127.0.0.1",
        host => host,
    };
    let target = format!("{host}:{port}");
    let mut stream = tokio::net::TcpStream::connect(&target)
        .await
        .map_err(|error| format!("could not connect to {target}: {error}"))?;
    stream
        .write_all(
            format!("GET /healthz HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .map_err(|error| format!("could not send the probe: {error}"))?;

    let mut status = String::new();
    BufReader::new(stream)
        .read_line(&mut status)
        .await
        .map_err(|error| format!("no answer from {target}: {error}"))?;
    if status.contains(" 200 ") {
        return Ok(());
    }
    Err(format!("/healthz answered {}", status.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_names() -> Vec<String> {
        GarminMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    #[test]
    fn every_tool_is_registered() {
        let mut names = tool_names();
        names.sort();
        assert_eq!(
            names,
            vec!["activities", "activity", "day", "metric", "sql", "status"]
        );
    }

    #[test]
    fn names_are_hyphenated_not_mixed() {
        // MCPHub joins server and tool with `-`; an underscore would read as
        // `garmin-set_alias`, mixing both separators in one identifier.
        for name in tool_names() {
            assert!(!name.contains('_'), "{name}");
        }
    }

    #[test]
    fn tools_describe_themselves() {
        // MCPHub shows these descriptions; an unlabelled tool is an unusable one.
        for tool in GarminMcp::tool_router().list_all() {
            assert!(tool.description.is_some(), "{}", tool.name);
        }
    }

    #[test]
    fn every_tool_only_reads() {
        // The whole archive is read-only toward Garmin; no tool may advertise a write.
        for tool in GarminMcp::tool_router().list_all() {
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{} has no annotations", tool.name));
            assert_eq!(annotations.read_only_hint, Some(true), "{}", tool.name);
        }
    }

    #[test]
    fn the_instructions_say_what_the_archive_is_and_is_not() {
        assert!(INSTRUCTIONS.contains("Garmin Connect data"));
        assert!(INSTRUCTIONS.contains("Nothing here reaches Garmin live"));
        assert!(INSTRUCTIONS.contains("per-second samples"));
    }

    #[tokio::test]
    async fn the_health_check_target_is_never_the_wildcard_address() {
        // Connecting to 0.0.0.0 is not a thing the healthcheck can do; port 1 is closed,
        // so the error names the address it actually tried.
        let error = health_check("0.0.0.0", 1).await.unwrap_err();
        assert!(error.contains("127.0.0.1:1"), "{error}");
    }
}
