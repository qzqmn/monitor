use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use crate::db::{
    get_all_servers, get_agent_token, rename_server, reset_traffic, update_traffic_limit,
    upsert_server, valid_id, AdminNotifySettings, AdminRename, AdminResetTraffic,
    AdminUpdateTrafficLimit, ReportPayload,
};
use crate::notify::{self, NotifySettings};
use serde::Serialize;
use sqlx::SqlitePool;
use std::sync::Arc;
use subtle::ConstantTimeEq;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    /// 登入 session 的 Cookie 要不要加 `Secure`（只能在 HTTPS 底下送出）。
    /// 這台預設是 Tailscale/內網 plain HTTP 存取，設 true 會導致 Cookie
    /// 整個送不出去、永遠登不進去，所以預設 false，架了 HTTPS 反代才開。
    pub cookie_secure: bool,
}

pub async fn report(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ReportPayload>,
) -> impl IntoResponse {
    if !valid_id(&payload.id) {
        return (
            StatusCode::BAD_REQUEST,
            "invalid id: only letters, digits, '.', '_', '-' allowed, max 64 chars",
        )
            .into_response();
    }

    // 認證改成「這個 id 有沒有在後台登記過、token 對不對得上」，取代舊版
    // 「只要密碼跟全域 REPORT_SECRET 一樣，隨便填什麼 id 都能自動建立
    // 新機器」。查無此 id 代表還沒在後台按「新增機器」，直接拒絕。
    match get_agent_token(&state.pool, &payload.id).await {
        Ok(Some(token)) => {
            let ok = bool::from(payload.secret.as_bytes().ct_eq(token.as_bytes()));
            if !ok {
                return (StatusCode::UNAUTHORIZED, "invalid secret").into_response();
            }
        }
        Ok(None) => {
            return (
                StatusCode::FORBIDDEN,
                "unknown agent id：請先在後台「新增機器」登記這個 id",
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("get_agent_token error: {:?}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    }

    match upsert_server(&state.pool, &payload).await {
        Ok(result) => {
            // 告警發送丟到背景 task，不擋住這次 /api/report 的回應：
            // Telegram/Webhook 若回應慢，Agent 端的上報與測速就不會被拖著等。
            let pool = state.pool.clone();
            tokio::spawn(async move {
                if let Ok(settings) = notify::load_settings(&pool).await {
                    notify::check_report_alerts(
                        &pool,
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
            });
            (StatusCode::OK, "ok").into_response()
        }
        Err(e) => {
            tracing::error!("upsert error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

/// 機器超過這麼久沒上報就視為離線（前端綠燈/灰燈用）。
/// 這跟通知設定裡「離線幾分鐘才告警」（offline_minutes）是兩件事：
/// 這裡只影響畫面顯示，用一個較短、跟 Agent 預設回報間隔（20 秒）匹配的
/// 固定值，讓卡片能較快反映真實狀態；告警的靜默期則由使用者在後台設定。
const ONLINE_STALE_SECS: i64 = 90;

#[derive(Serialize)]
struct ServerView {
    #[serde(flatten)]
    inner: crate::db::ServerStatus,
    online: bool,
}

pub async fn list_servers(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match get_all_servers(&state.pool).await {
        Ok(servers) => {
            let now = chrono::Utc::now();
            let views: Vec<ServerView> = servers
                .into_iter()
                .map(|s| {
                    let online = (now - s.last_seen).num_seconds() < ONLINE_STALE_SECS;
                    ServerView { inner: s, online }
                })
                .collect();
            Json(views).into_response()
        }
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
    match update_traffic_limit(
        &state.pool,
        &payload.id,
        payload.traffic_limit,
        payload.traffic_notify_percent,
        payload.traffic_reset_day,
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

pub async fn admin_get_notify(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
    // Token 欄位在 GET 時是遮罩過的（見 admin_get_notify）。管理頁「修改其他
    // 設定但沒有動 Token 欄位」時，表單會把遮罩值原樣送回來；這裡偵測到空值
    // 或遮罩格式就保留資料庫裡原本的 Token，否則每次存設定都會把 Token
    // 覆蓋壞掉，Telegram 通知就此失效且不會有任何錯誤提示。
    let old = notify::load_settings(&state.pool).await.unwrap_or_default();
    let token = if payload.telegram_bot_token.is_empty()
        || payload.telegram_bot_token.ends_with("****")
    {
        old.telegram_bot_token
    } else {
        payload.telegram_bot_token
    };

    let settings = NotifySettings {
        telegram_enabled: payload.telegram_enabled,
        telegram_bot_token: token,
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

pub async fn admin_test_notify(State(state): State<Arc<AppState>>) -> impl IntoResponse {
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
            // 這裡如實回傳 send_alert 的結果：以前不管 Telegram/Webhook 有沒有
            // 真的送出去都回 200「已送出」，管理員看不出設定其實是壞的。
            match notify::send_alert(&settings, &alert).await {
                Ok(_) => (StatusCode::OK, "test sent").into_response(),
                Err(e) => {
                    tracing::warn!("test notify failed: {}", e);
                    (StatusCode::BAD_GATEWAY, format!("送出失敗：{e}")).into_response()
                }
            }
        }
        Err(e) => {
            tracing::error!("load settings error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}
