use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Duration, Instant},
};
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
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM settings WHERE key = 'notify'")
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

// 共用一個帶逾時的 Client，而不是每次告警都 `Client::new()`：
// 一來省掉重複建立連線池的成本，二來確保單一告警最多卡 10 秒，
// 不會因為 Telegram/Webhook 對方沒回應就一直吊著。
static HTTP: Lazy<Client> = Lazy::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build reqwest client")
});

/// 冷卻 + 去重狀態：
/// - `COOLDOWN`：CPU / 記憶體 / 流量這類「持續超標」的告警，同一台機器
///   同一種類型在冷卻時間內只送一次，避免每次上報（預設 20 秒一次）都炸一則。
/// - `OFFLINE_ALERTED`：離線告警屬於「事件」而不是「持續超標」，用一個
///   集合記錄「目前正在離線中、已經通知過」的機器，等它恢復上報後才移出，
///   離線期間不會每次背景檢查（60 秒一次）都重複發。
static COOLDOWN_STATE: Lazy<Mutex<HashMap<(String, &'static str), Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static OFFLINE_ALERTED: Lazy<Mutex<HashSet<String>>> = Lazy::new(|| Mutex::new(HashSet::new()));

const ALERT_COOLDOWN: Duration = Duration::from_secs(30 * 60);

fn cooled_down(id: &str, kind: &'static str) -> bool {
    let mut m = COOLDOWN_STATE.lock().unwrap();
    let now = Instant::now();
    match m.get(&(id.to_string(), kind)) {
        Some(t) if now.duration_since(*t) < ALERT_COOLDOWN => false,
        _ => {
            m.insert((id.to_string(), kind), now);
            true
        }
    }
}

pub async fn send_alert(settings: &NotifySettings, alert: &AlertPayload) -> Result<()> {
    let mut attempted = false;
    let mut failures: Vec<String> = Vec::new();

    if settings.telegram_enabled
        && !settings.telegram_bot_token.is_empty()
        && !settings.telegram_chat_id.is_empty()
    {
        attempted = true;
        let text = format!("{}\n{}", alert.title, alert.message);
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            settings.telegram_bot_token
        );
        // 不用 parse_mode: Markdown —— 機器名稱、路徑常帶 `_`、`*`、`[` 等
        // Markdown 特殊字元，Telegram 遇到未跳脫的格式符號會直接回 400，
        // 導致告警整個送不出去且不易察覺。純文字最穩妥。
        let body = json!({
            "chat_id": settings.telegram_chat_id,
            "text": text,
        });
        match HTTP.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Telegram alert sent: {}", alert.title);
            }
            Ok(resp) => {
                let status = resp.status();
                warn!("Telegram failed: status {}", status);
                failures.push(format!("Telegram HTTP {status}"));
            }
            Err(e) => {
                // `.without_url()`：reqwest 的錯誤預設會夾帶完整請求 URL，
                // 這裡的 URL 含有 Bot Token，絕不能整包印進日誌。
                error!("Telegram error: {:?}", e.without_url());
                failures.push("Telegram request failed".into());
            }
        }
    }

    if settings.webhook_enabled && !settings.webhook_url.is_empty() {
        attempted = true;
        match HTTP.post(&settings.webhook_url).json(alert).send().await {
            Ok(resp) if resp.status().is_success() => {
                info!("Webhook alert sent: {}", alert.title);
            }
            Ok(resp) => {
                let status = resp.status();
                warn!("Webhook failed: status {}", status);
                failures.push(format!("Webhook HTTP {status}"));
            }
            Err(e) => {
                error!("Webhook error: {:?}", e.without_url());
                failures.push("Webhook request failed".into());
            }
        }
    }

    if !attempted {
        return Err(anyhow!("沒有任何通知管道啟用"));
    }
    if !failures.is_empty() {
        return Err(anyhow!(failures.join("; ")));
    }
    Ok(())
}

/// 在每次上報後檢查是否需要告警（CPU / 記憶體 / 流量）
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
    if settings.cpu_threshold > 0.0 && cpu >= settings.cpu_threshold && cooled_down(server_id, "cpu") {
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
        if mem_pct >= settings.mem_threshold && cooled_down(server_id, "memory") {
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
        if pct >= traffic_notify_percent && cooled_down(server_id, "traffic") {
            let alert = AlertPayload {
                title: "流量告警".into(),
                message: format!(
                    "{} 累計流量已達 {:.1}%（{:.2} GB / {:.0} GB）",
                    server_name, pct, total, traffic_limit
                ),
                server_id: server_id.into(),
                server_name: server_name.into(),
                r#type: "traffic".into(),
                level: if pct >= 100.0 { "critical".into() } else { "warning".into() },
                value: pct,
                limit: traffic_limit,
                timestamp: now,
            };
            let _ = send_alert(settings, &alert).await;
        }
    }
}

/// 定期檢查離線機器；每台機器離線期間只通知一次，恢復上報後補發一則恢復通知
pub async fn check_offline(pool: &SqlitePool, settings: &NotifySettings) {
    if settings.offline_minutes <= 0 {
        return;
    }

    let threshold = chrono::Utc::now() - chrono::Duration::minutes(settings.offline_minutes);

    let rows: Vec<(String, String, String)> =
        match sqlx::query_as("SELECT id, name, last_seen FROM servers")
            .fetch_all(pool)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                error!("offline check db error: {:?}", e);
                return;
            }
        };

    let mut still_offline = HashSet::new();

    for (id, name, last_seen_str) in rows {
        let is_offline = chrono::DateTime::parse_from_rfc3339(&last_seen_str)
            .map(|t| t.with_timezone(&chrono::Utc) < threshold)
            .unwrap_or(false);

        if is_offline {
            still_offline.insert(id.clone());
            let already_alerted = OFFLINE_ALERTED.lock().unwrap().contains(&id);
            if !already_alerted {
                OFFLINE_ALERTED.lock().unwrap().insert(id.clone());
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
        } else {
            let was_alerted = OFFLINE_ALERTED.lock().unwrap().remove(&id);
            if was_alerted {
                let alert = AlertPayload {
                    title: "恢復通知".into(),
                    message: format!("{} 已恢復上報", name),
                    server_id: id,
                    server_name: name,
                    r#type: "recovered".into(),
                    level: "info".into(),
                    value: 0.0,
                    limit: 0.0,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };
                let _ = send_alert(settings, &alert).await;
            }
        }
    }

    // 機器被刪除/資料庫清空後，清掉不再存在的 id，避免集合無限增長
    let mut alerted = OFFLINE_ALERTED.lock().unwrap();
    alerted.retain(|id| still_offline.contains(id));
}
