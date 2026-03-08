use askama::Template;
use axum::{extract::Json, http::header, response::{Html, IntoResponse}};
use qrcode::{render::svg, QrCode};
use serde::Deserialize;

use crate::error::Error;

#[derive(Debug, Clone, Template)]
#[template(path = "qr.html")]
pub struct QrPage {
    pub version: &'static str,
}

pub async fn qr_page_route() -> Result<Html<String>, Error> {
    Ok(Html(QrPage { version: crate::VERSION }.render()?))
}

#[derive(Deserialize)]
pub struct QrParams {
    pub data: String,
}

pub async fn qr_route(Json(params): Json<QrParams>) -> Result<impl IntoResponse, Error> {
    let code = QrCode::new(params.data.as_bytes())
        .map_err(|source| Error::QrEncode { source })?;

    let svg = code
        .render::<svg::Color>()
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();

    Ok(([(header::CONTENT_TYPE, "image/svg+xml")], svg))
}
