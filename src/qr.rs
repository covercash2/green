use askama::Template;
use axum::{
    extract::{Json, State},
    http::header,
    response::{Html, IntoResponse},
};
use qrcode::{QrCode, render::svg};
use serde::Deserialize;

use crate::{ServerState, auth::{AuthUserInfo, MaybeAuthUser}, error::Error};

#[derive(Debug, Clone, Template)]
#[template(path = "qr.html")]
pub struct QrPage {
    pub version: &'static str,
    pub auth_user: Option<AuthUserInfo>,
}

pub async fn qr_page_route(
    MaybeAuthUser(auth_user): MaybeAuthUser,
    State(_): State<ServerState>,
) -> Result<Html<String>, Error> {
    Ok(Html(
        QrPage {
            version: crate::VERSION,
            auth_user,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct QrParams {
    pub data: String,
}

pub async fn qr_route(Json(params): Json<QrParams>) -> Result<impl IntoResponse, Error> {
    let code = QrCode::new(params.data.as_bytes()).map_err(|source| Error::QrEncode { source })?;

    let svg = code
        .render::<svg::Color>()
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();

    Ok(([(header::CONTENT_TYPE, "image/svg+xml")], svg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt;

    /// Minimal `ServerState` for handler tests that require state but don't use it.
    async fn minimal_state() -> ServerState {
        use crate::{
            breaker,
            breaker_detail::{BreakerData, BreakerStore},
            index::Index,
            route::Routes,
        };
        use std::{collections::HashMap, sync::Arc};

        let store = Arc::new(
            BreakerStore::from_data(BreakerData {
                todos: vec![],
                slots: HashMap::new(),
                couples: vec![],
            })
            .unwrap(),
        );
        let breaker_content = Arc::new(breaker::BreakerContent::new(store.as_ref()));

        ServerState {
            certificate: Arc::from(""),
            breaker_content,
            breaker_detail_store: store,
            index: Index::new(Routes::default(), false, false, false).await.unwrap(),
            tailscale_socket: Arc::from(std::path::Path::new(
                "/run/tailscale/tailscaled.sock",
            )),
            notes_store: None,
            auth_state: None,
            mqtt_state: None,
        }
    }

    fn json_body(v: serde_json::Value) -> Body {
        Body::from(serde_json::to_vec(&v).unwrap())
    }

    // ── qr_route ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn qr_route_returns_svg_content_type() {
        let app = Router::new().route("/qr", post(qr_route));
        let req = Request::builder()
            .method("POST")
            .uri("/qr")
            .header("content-type", "application/json")
            .body(json_body(serde_json::json!({"data": "https://example.com"})))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get("content-type").unwrap(), "image/svg+xml");
    }

    #[tokio::test]
    async fn qr_route_body_is_valid_svg() {
        let app = Router::new().route("/qr", post(qr_route));
        let req = Request::builder()
            .method("POST")
            .uri("/qr")
            .header("content-type", "application/json")
            .body(json_body(serde_json::json!({"data": "hello"})))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let svg = std::str::from_utf8(&bytes).unwrap();
        assert!(svg.contains("<svg"), "response should be an SVG document");
        assert!(svg.contains("</svg>"));
    }

    // ── qr_page_route ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn qr_page_route_returns_html() {
        let state = minimal_state().await;
        let app = Router::new()
            .route("/qr", get(qr_page_route))
            .with_state(state);
        let req = Request::builder()
            .method("GET")
            .uri("/qr")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let html = std::str::from_utf8(&bytes).unwrap();
        assert!(html.contains("<!DOCTYPE html") || html.contains("<html"));
    }
}
