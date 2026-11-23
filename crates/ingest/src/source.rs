//! What the runner needs from Garmin, as a trait so a test can stand in a fake.

use garmin_client::{Client, ClientError, Session};
use serde_json::Value;

#[allow(async_fn_in_trait)]
pub trait Source {
    async fn refresh(&self, s: &Session) -> Result<Session, ClientError>;
    async fn activities(
        &self,
        s: &Session,
        start: i64,
        limit: i64,
    ) -> Result<Vec<Value>, ClientError>;
    async fn activity_details(&self, s: &Session, id: i64) -> Result<Value, ClientError>;
    async fn activity_file(&self, s: &Session, id: i64) -> Result<Vec<u8>, ClientError>;
    async fn daily(&self, s: &Session, kind: &str, date: &str) -> Result<Value, ClientError>;
}

impl Source for Client {
    async fn refresh(&self, s: &Session) -> Result<Session, ClientError> {
        Client::refresh(self, s).await
    }
    async fn activities(
        &self,
        s: &Session,
        start: i64,
        limit: i64,
    ) -> Result<Vec<Value>, ClientError> {
        Client::activities(self, s, start, limit).await
    }
    async fn activity_details(&self, s: &Session, id: i64) -> Result<Value, ClientError> {
        Client::activity_details(self, s, id).await
    }
    async fn activity_file(&self, s: &Session, id: i64) -> Result<Vec<u8>, ClientError> {
        Client::activity_file(self, s, id).await
    }
    async fn daily(&self, s: &Session, kind: &str, date: &str) -> Result<Value, ClientError> {
        Client::daily(self, s, kind, date).await
    }
}
