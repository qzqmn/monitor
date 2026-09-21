use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use crate::db::{
    get_all_servers, rename_server, reset_traffic, update_traffic_limit, upsert_server,
    AdminNotifySettings, AdminRename, AdminResetTraffic, AdminUpdateTrafficLimit, ReportPayload,
};
use crate::notify::{self, NotifySettings};
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub report_secret: String,
    pub admin_secret: String,
}

pub async fn report(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ReportPayload>,
) -> impl IntoResponse {
    if payload.secret != state.report_secret {
        return (StatusCode::UNAUTHORIZED, "invalid secret").into_response();
    }

    match upsert_server(&state.pool, &payload).await {
        Ok(result) => {
            if let Ok(settings) = notify::load_settings(&state.pool).await {
                notify::check_report_alerts(
                    &state.pool,
                    &settings,
                    &result.id,
                    &result.name,
                    result.cpu,
                    result.mem_used,
                    result.mem_total,
                    result.cum_in,
                    result.cum_out,
                    result.traffic_limit,
                    result.traffic_notify_percent,
                )
                .await;
            }
            (StatusCode::OK, "ok").into_response()
        }
        Err(e) => {
            tracing::error!("upsert error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn list_servers(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match get_all_servers(&state.pool).await {
        Ok(servers) => Json(servers).into_response(),
        Err(e) => {
            tracing::error!("list error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn admin_reset_traffic(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AdminResetTraffic>,
) -> impl IntoResponse {
    if payload.admin_secret != state.admin_secret {
        return (StatusCode::UNAUTHORIZED, "invalid admin secret").into_response();
    }
    match reset_traffic(&state.pool, &payload.id).await {
        Ok(_) => (StatusCode::OK, "reset ok").into_response(),
        Err(e) => {
            tracing::error!("reset error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn admin_rename(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AdminRename>,
) -> impl IntoResponse {
    if payload.admin_secret != state.admin_secret {
        return (StatusCode::UNAUTHORIZED, "invalid admin secret").into_response();
    }
    match rename_server(&state.pool, &payload.id, &payload.name).await {
        Ok(_) => (StatusCode::OK, "rename ok").into_response(),
        Err(e) => {
            tracing::error!("rename error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn admin_update_traffic_limit(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AdminUpdateTrafficLimit>,
) -> impl IntoResponse {
    if payload.admin_secret != state.admin_secret {
        return (StatusCode::UNAUTHORIZED, "invalid admin secret").into_response();
    }
    match update_traffic_limit(
        &state.pool,
        &payload.id,
        payload.traffic_limit,
        payload.traffic_notify_percent,
    )
    .await
    {
        Ok(_) => (StatusCode::OK, "update ok").into_response(),
        Err(e) => {
            tracing::error!("update traffic limit error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn admin_get_notify(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match notify::load_settings(&state.pool).await {
        Ok(s) => {
            let mut safe = s.clone();
            if safe.telegram_bot_token.len() > 8 {
                safe.telegram_bot_token = format!("{}****", &safe.telegram_bot_token[..4]);
            }
            Json(safe).into_response()
        }
        Err(e) => {
            tracing::error!("get notify error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn admin_save_notify(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AdminNotifySettings>,
) -> impl IntoResponse {
    if payload.admin_secret != state.admin_secret {
        return (StatusCode::UNAUTHORIZED, "invalid admin secret").into_response();
    }
    let settings = NotifySettings {
        telegram_enabled: payload.telegram_enabled,
        telegram_bot_token: payload.telegram_bot_token,
        telegram_chat_id: payload.telegram_chat_id,
        webhook_enabled: payload.webhook_enabled,
        webhook_url: payload.webhook_url,
        offline_minutes: payload.offline_minutes,
        cpu_threshold: payload.cpu_threshold,
        mem_threshold: payload.mem_threshold,
    };
    match notify::save_settings(&state.pool, &settings).await {
        Ok(_) => (StatusCode::OK, "saved").into_response(),
        Err(e) => {
            tracing::error!("save notify error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn admin_test_notify(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let secret = payload.get("admin_secret").and_then(|v| v.as_str()).unwrap_or("");
    if secret != state.admin_secret {
        return (StatusCode::UNAUTHORIZED, "invalid admin secret").into_response();
    }
    match notify::load_settings(&state.pool).await {
        Ok(settings) => {
            let alert = notify::AlertPayload {
                title: "測試通知".into(),
                message: "這是一條來自 VPS Monitor 的測試訊息。".into(),
                server_id: "test".into(),
                server_name: "Test".into(),
                r#type: "test".into(),
                level: "info".into(),
                value: 0.0,
                limit: 0.0,
                timestamp: chrono::Utc::now().to_rfc3339(),
            };
            match notify::send_alert(&settings, &alert).await {
                Ok(_) => (StatusCode::OK, "test sent").into_response(),
                Err(e) => {
                    tracing::error!("test notify error: {:?}", e);
                    (StatusCode::INTERNAL_SERVER_ERROR, "send failed").into_response()
                }
            }
        }
        Err(e) => {
            tracing::error!("load settings error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}
