//! The MCP server. Filled by the MCP task.
use garmin_core::config::McpSettings;

pub const SERVER_NAME: &str = "garmin";

pub async fn run(_settings: &McpSettings) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    Err("garmin-mcp server not yet implemented".into())
}

pub async fn health_check(_host: &str, _port: u16) -> Result<(), String> {
    Err("not yet implemented".to_owned())
}
