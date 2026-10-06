//! Setup uses the same host configuration/validation contract as native clients.
use crate::server::Server;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use regain_hub::{client::ClientError, ipc::Command};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn routes() -> Router<Arc<Server>> {
    Router::new()
        .route(
            "/setup/hub",
            get(|| async { Html(include_str!("../web/hub.html")) }),
        )
        .route("/setup/v1/{kind}/{slot}/setup", get(device_page))
        .route(
            "/hub.mjs",
            get(|| async {
                (
                    [("Content-Type", "application/javascript")],
                    include_str!("../web/hub.mjs"),
                )
            }),
        )
        .route(
            "/hub-config.mjs",
            get(|| async {
                (
                    [("Content-Type", "application/javascript")],
                    include_str!("../web/hub-config.mjs"),
                )
            }),
        )
        .route(
            "/hub-form.mjs",
            get(|| async {
                (
                    [("Content-Type", "application/javascript")],
                    include_str!("../web/hub-form.mjs"),
                )
            }),
        )
        .route(
            "/hub.css",
            get(|| async {
                (
                    [("Content-Type", "text/css")],
                    include_str!("../web/hub.css"),
                )
            }),
        )
        // POST-only and JSON-only, including reads. No CORS grant. The same-origin
        // check precedes dispatch so a cross-site request cannot inspect hardware.
        .route(
            "/setup/api/hub",
            post(request).layer(DefaultBodyLimit::max(regain_hub::ipc::MAX_FRAME_BYTES)),
        )
}
async fn device_page(
    State(server): State<Arc<Server>>,
    Path((kind, slot)): Path<(String, u32)>,
) -> Response {
    if !matches!(
        kind.as_str(),
        "switch" | "safetymonitor" | "observingconditions"
    ) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(hub) = &server.hub else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match hub.devices().await {
        Ok(devices)
            if devices.iter().any(|device| {
                device.number == slot
                    && crate::hub_output::class_name(device.device_type).to_lowercase() == kind
            }) =>
        {
            Html(include_str!("../web/hub.html")).into_response()
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
async fn request(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !crate::server::setup_allowed(&headers)
        || !headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
            })
        || headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "cross-site")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(hub) = &server.hub else {
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":{"code":"notConfigured","message":"Start this server with --hub-config ABSOLUTE_PATH to manage a hub."}}),
        );
    };
    let command: Command = match serde_json::from_slice(&body) {
        Ok(command) => command,
        Err(_) => {
            return response(
                StatusCode::BAD_REQUEST,
                json!({"error":{"code":"invalidRequest","message":"Invalid hub setup request"}}),
            );
        }
    };
    match hub.setup(command).await {
        Ok(result) => response(StatusCode::OK, json!({"result":result})),
        Err(ClientError::Remote(remote)) => response(
            StatusCode::BAD_REQUEST,
            json!({"error":{"code":remote.code,"message":remote.message,"fields":remote.fields,"retryAfterSeconds":remote.retry_after_seconds}}),
        ),
        Err(error) => {
            let code = match &error {
                ClientError::Uncertain => "uncertain",
                ClientError::Busy => "busy",
                ClientError::InvalidRequest => "invalidRequest",
                _ => "disconnected",
            };
            response(
                StatusCode::BAD_REQUEST,
                json!({"error":{"code":code,"message":error.to_string()}}),
            )
        }
    }
}
fn response(status: StatusCode, body: Value) -> Response {
    (status, [("Cache-Control", "no-store")], Json(body)).into_response()
}
