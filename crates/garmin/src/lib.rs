//! The read-only Garmin Connect private-API client. Fetches and parses JSON; no database,
//! no FIT parsing (that lives in the ingester, next to the sample model it builds).

pub mod api;
pub mod error;
pub mod session;

pub use api::{ALLOWED_SERVICES, Client, DAILY_PATHS};
pub use error::ClientError;
pub use session::Session;
