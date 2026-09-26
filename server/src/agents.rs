//! 後台「機器管理」：新增/列出/重新產生/刪除每台機器各自的上報 token。
//! 取代舊版「全機共用一組 REPORT_SECRET、Agent 自己宣告 id 就自動建立」的
//! 模式——現在要先在這裡登記過，Agent 才報得進來（見 api::report）。

use crate::api::AppState;
use crate::db::{self, valid_id};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize)]
pub struct CreateAgent {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct AgentCredential {
    pub id: String,
    pub name: String,
    pub token: String,
}

pub async fn create_agent(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<CreateAgent>,
) -> impl IntoResponse {
    let id = payload.id.trim();
    if !valid_id(id) {
        return (
            StatusCode::BAD_REQUEST,
            "id 只能是英數字、'.'、'_'、'-'，最長 64 字元",
        )
            .into_response();
    }
    let name = if payload.name.trim().is_empty() {
        id.to_string()
    } else {
        payload.name
    };

    match db::get_agent_token(&state.pool, id).await {
        Ok(Some(_)) => {
            return (StatusCode::CONFLICT, "這個 id 已經存在了").into_response();
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!("check existing agent error: {:?}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    }

    match db::create_agent(&state.pool, id, &name).await {
        Ok(token) => Json(AgentCredential {
            id: id.to_string(),
            name,
            token,
        })
        .into_response(),
        Err(e) => {
            tracing::error!("create_agent error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

#[derive(Serialize)]
pub struct AgentListItem {
    pub id: String,
    pub name: String,
    pub token: String,
    pub last_seen: chrono::DateTime<chrono::Utc>,
    pub online: bool,
    pub traffic_limit: f64,
    pub traffic_notify_percent: f64,
    pub traffic_reset_day: i64,
}

/// 跟 api::ONLINE_STALE_SECS 用同一個門檻，判斷「上次上報是不是太久之前」。
const ONLINE_STALE_SECS: i64 = 90;

pub async fn list_agents(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match db::list_agents_admin(&state.pool).await {
        Ok(rows) => {
            let now = chrono::Utc::now();
            let items: Vec<AgentListItem> = rows
                .into_iter()
                .map(|r| AgentListItem {
                    id: r.id,
                    name: r.name,
                    token: r.token.unwrap_or_default(),
                    online: (now - r.last_seen).num_seconds() < ONLINE_STALE_SECS,
                    last_seen: r.last_seen,
                    traffic_limit: r.traffic_limit,
                    traffic_notify_percent: r.traffic_notify_percent,
                    traffic_reset_day: r.traffic_reset_day,
                })
                .collect();
            Json(items).into_response()
        }
        Err(e) => {
            tracing::error!("list_agents error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct AgentIdBody {
    pub id: String,
}

pub async fn rotate_agent_token(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AgentIdBody>,
) -> impl IntoResponse {
    match db::rotate_agent_token(&state.pool, &payload.id).await {
        Ok(token) => Json(serde_json::json!({ "id": payload.id, "token": token })).into_response(),
        Err(e) => {
            tracing::error!("rotate_agent_token error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

pub async fn delete_agent(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AgentIdBody>,
) -> impl IntoResponse {
    match db::delete_agent(&state.pool, &payload.id).await {
        Ok(_) => (StatusCode::OK, "deleted").into_response(),
        Err(e) => {
            tracing::error!("delete_agent error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}
