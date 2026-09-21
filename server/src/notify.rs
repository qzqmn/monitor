use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use tracing::{error, info, warn};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifySettings {
    pub telegram_enabled: bool,
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
    pub webhook_enabled: bool,
    pub webhook_url: String,
    pub offline_minutes: i64,
    pub cpu_threshold: f32,
    pub mem_threshold: f32,
}

impl NotifySettings {
    pub fn from_json(s: &str) -> Self {
        serde_json::from_str(s).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
}

pub async fn load_settings(pool: &SqlitePool) -> Result<NotifySettings> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT value FROM settings WHERE key = 'notify'"
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some((v,)) => NotifySettings::from_json(&v),
        None => NotifySettings {
            offline_minutes: 5,
            cpu_threshold: 90.0,
            mem_threshold: 90.0,
            ..Default::default()
        },
    })
}

pub async fn save_settings(pool: &SqlitePool, settings: &NotifySettings) -> Result<()> {
    let json = settings.to_json();
    sqlx::query(
        r#"
        INSERT INTO settings (key, value) VALUES ('notify', ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        "#,
    )
    .bind(json)
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct AlertPayload {
    pub title: String,
    pub message: String,
    pub server_id: String,
    pub server_name: String,
    pub r#type: String,
    pub level: String,
    pub value: f64,
    pub limit: f64,
    pub timestamp: String,
}

pub async fn send_alert(settings: &NotifySettings, alert: &AlertPayload) -> Result<()> {
    let client = Client::new();

    if settings.telegram_enabled
        && !settings.telegram_bot_token.is_empty()
        && !settings.telegram_chat_id.is_empty()
    {
        let text = format!("*{}*\n{}", alert.title, alert.message);
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            settings.telegram_bot_token
        );
        let body = json!({
            "chat_id": settings.telegram_chat_id,
            "text": text,
            "parse_mode": "Markdown"
        });
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Telegram alert sent: {}", alert.title);
            }
            Ok(resp) => {
                warn!("Telegram failed: status {}", resp.status());
            }
            Err(e) => error!("Telegram error: {:?}", e),
        }
    }

    if settings.webhook_enabled && !settings.webhook_url.is_empty() {
        match client.post(&settings.webhook_url).json(alert).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Webhook alert sent: {}", alert.title);
            }
            Ok(resp) => {
                warn!("Webhook failed: status {}", resp.status());
            }
            Err(e) => error!("Webhook error: {:?}", e),
        }
    }

    Ok(())
}

/// 在每次上報後檢查是否需要告警（流量 / CPU / 記憶體）
pub async fn check_report_alerts(
    _pool: &SqlitePool,
    settings: &NotifySettings,
    server_id: &str,
    server_name: &str,
    cpu: f32,
    mem_used: f64,
    mem_total: f64,
    cum_in: f64,
    cum_out: f64,
    traffic_limit: f64,
    traffic_notify_percent: f64,
) {
    let now = chrono::Utc::now().to_rfc3339();

    // CPU
    if settings.cpu_threshold > 0.0 && cpu >= settings.cpu_threshold {
        let alert = AlertPayload {
            title: "CPU 告警".into(),
            message: format!(
                "{} CPU 使用率 {:.1}%（閾值 {:.0}%）",
                server_name, cpu, settings.cpu_threshold
            ),
            server_id: server_id.into(),
            server_name: server_name.into(),
            r#type: "cpu".into(),
            level: "warning".into(),
            value: cpu as f64,
            limit: settings.cpu_threshold as f64,
            timestamp: now.clone(),
        };
        let _ = send_alert(settings, &alert).await;
    }

    // 記憶體
    if settings.mem_threshold > 0.0 && mem_total > 0.0 {
        let mem_pct = (mem_used / mem_total * 100.0) as f32;
        if mem_pct >= settings.mem_threshold {
            let alert = AlertPayload {
                title: "記憶體告警".into(),
                message: format!(
                    "{} 記憶體使用率 {:.1}%（閾值 {:.0}%）",
                    server_name, mem_pct, settings.mem_threshold
                ),
                server_id: server_id.into(),
                server_name: server_name.into(),
                r#type: "memory".into(),
                level: "warning".into(),
                value: mem_pct as f64,
                limit: settings.mem_threshold as f64,
                timestamp: now.clone(),
            };
            let _ = send_alert(settings, &alert).await;
        }
    }

    // 流量百分比
    if traffic_limit > 0.0 && traffic_notify_percent > 0.0 {
        let total = cum_in + cum_out;
        let pct = total / traffic_limit * 100.0;
        if pct >= traffic_notify_percent {
            let alert = AlertPayload {
                title: "流量告警".into(),
                message: format!(
                    "{} 累計流量已達 {:.1}%（{:.2} GB / {:.0} GB）",
                    server_name, pct, total, traffic_limit
                ),
                server_id: server_id.into(),
                server_name: server_name.into(),
                r#type: "traffic".into(),
                level: if pct >= 100.0 {
                    "critical".into()
                } else {
                    "warning".into()
                },
                value: pct,
                limit: traffic_limit,
                timestamp: now,
            };
            let _ = send_alert(settings, &alert).await;
        }
    }
}

/// 定期檢查離線機器
pub async fn check_offline(pool: &SqlitePool, settings: &NotifySettings) {
    if settings.offline_minutes <= 0 {
        return;
    }

    let threshold = chrono::Utc::now()
        - chrono::Duration::minutes(settings.offline_minutes);

    let rows: Vec<(String, String, String)> = match sqlx::query_as(
        "SELECT id, name, last_seen FROM servers"
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("offline check db error: {:?}", e);
            return;
        }
    };

    for (id, name, last_seen_str) in rows {
        if let Ok(last) = chrono::DateTime::parse_from_rfc3339(&last_seen_str) {
            if last.with_timezone(&chrono::Utc) < threshold {
                let alert = AlertPayload {
                    title: "離線告警".into(),
                    message: format!(
                        "{} 已超過 {} 分鐘沒有上報（最後：{}）",
                        name, settings.offline_minutes, last_seen_str
                    ),
                    server_id: id,
                    server_name: name,
                    r#type: "offline".into(),
                    level: "critical".into(),
                    value: settings.offline_minutes as f64,
                    limit: 0.0,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };
                let _ = send_alert(settings, &alert).await;
            }
        }
    }
}
