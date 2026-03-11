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
