//! Client tests against a local server: refresh rotates both tokens, the allow-list
//! refuses an unknown service, a 401 is a rejection, activities page, and no credential
//! reaches Debug.

use std::net::SocketAddr;

use axum::{
    Router,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::any,
};
use garmin_client::{Client, ClientError, Session};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn session() -> Session {
    Session {
        refresh_token: "SECRET-REFRESH".into(),
        client_id: "GARMIN_CONNECT_MOBILE_ANDROID_DI_2025Q2".into(),
        access_token: "SECRET-ACCESS".into(),
        access_expires_at: Some(chrono::Utc::now() + chrono::TimeDelta::hours(1)),
        oauth1_token: Some("SECRET-OAUTH1".into()),
        oauth1_secret: Some("SECRET-OAUTH1SEC".into()),
        garmin_user_id: Some("u1".into()),
    }
}

async fn serve() -> (String, String) {
    async fn handle(req: Request) -> axum::response::Response {
        let path = req.uri().path().to_owned();
        if path.contains("di-oauth2-service/oauth/token") {
            return Json(json!({"access_token": "NEW-ACCESS", "refresh_token": "NEW-REFRESH", "expires_in": 3600, "token_type": "bearer"})).into_response();
        }
        if path.contains("activitylist-service/activities/search/activities") {
            return Json(json!([{"activityId": 1}, {"activityId": 2}])).into_response();
        }
        if path.contains("hrv-service") {
            // A day with no HRV: Garmin answers 204 with an empty body.
            return StatusCode::NO_CONTENT.into_response();
        }
        if path.contains("userprofile-service") {
            // A rejected session: 401.
            return (StatusCode::UNAUTHORIZED, "no").into_response();
        }
        Json(json!({"ok": true})).into_response()
    }
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().fallback(any(handle));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{addr}/");
    (base.clone(), base)
}

#[tokio::test]
async fn refresh_rotates_both_tokens() {
    let (api, diauth) = serve().await;
    let client = Client::with_bases(api, diauth);
    let rotated = client.refresh(&session()).await.unwrap();
    assert_eq!(rotated.access_token, "NEW-ACCESS");
    assert_eq!(rotated.refresh_token, "NEW-REFRESH");
    assert!(rotated.access_expires_at.unwrap() > chrono::Utc::now());
    // The public client id and the oauth1 fallback carry over.
    assert_eq!(rotated.client_id, "GARMIN_CONNECT_MOBILE_ANDROID_DI_2025Q2");
    assert_eq!(rotated.oauth1_token.as_deref(), Some("SECRET-OAUTH1"));
}

#[tokio::test]
async fn the_allow_list_refuses_an_unknown_service() {
    let client = Client::with_bases("http://127.0.0.1:1/", "http://127.0.0.1:1/");
    let err = client
        .get_json(&session(), "social-service/followers")
        .await
        .unwrap_err();
    assert!(matches!(err, ClientError::NotAllowed(_)), "{err:?}");
}

#[tokio::test]
async fn a_401_is_a_rejection_not_http() {
    let (api, diauth) = serve().await;
    let client = Client::with_bases(api, diauth);
    let err = client.profile(&session()).await.unwrap_err();
    assert!(matches!(err, ClientError::Rejected), "{err:?}");
}

#[tokio::test]
async fn activities_page() {
    let (api, diauth) = serve().await;
    let client = Client::with_bases(api, diauth);
    let acts = client.activities(&session(), 0, 20).await.unwrap();
    assert_eq!(acts.len(), 2);
    assert_eq!(acts[0].get("activityId").and_then(Value::as_i64), Some(1));
}

#[tokio::test]
async fn a_204_day_is_null_not_a_shape_error() {
    let (api, diauth) = serve().await;
    let client = Client::with_bases(api, diauth);
    let day = client.daily(&session(), "hrv", "2026-09-22").await.unwrap();
    assert!(day.is_null(), "{day:?}");
}

#[tokio::test]
async fn needs_refresh_when_expired() {
    let mut s = session();
    assert!(!s.needs_refresh(chrono::Utc::now()));
    s.access_expires_at = Some(chrono::Utc::now() - chrono::TimeDelta::minutes(1));
    assert!(s.needs_refresh(chrono::Utc::now()));
    s.access_token = String::new();
    assert!(s.needs_refresh(chrono::Utc::now()));
}

#[tokio::test]
async fn no_credential_reaches_debug() {
    let shown = format!("{:?}", session());
    for secret in ["SECRET-REFRESH", "SECRET-ACCESS", "SECRET-OAUTH1"] {
        assert!(!shown.contains(secret), "{shown}");
    }
    assert!(shown.contains("**********"));
}
