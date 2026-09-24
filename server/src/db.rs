use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    FromRow, SqlitePool,
};
use anyhow::Result;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ServerStatus {
    pub id: String,
    pub name: String,
    pub country: Option<String>,
    pub city: Option<String>,
    pub cpu: f32,
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
    pub mem_used: f64,
    pub mem_total: f64,
    pub disk_used: f64,
    pub disk_total: f64,
    pub net_rx: f64,
    pub net_tx: f64,
    pub cum_in: f64,
    pub cum_out: f64,
    pub last_raw_in: f64,
    pub last_raw_out: f64,
    pub traffic_start: Option<String>,
    pub traffic_limit: f64,
    pub traffic_notify_percent: f64,
    pub uptime: i64,
    pub latency_telecom: Option<i32>,
    pub latency_unicom: Option<i32>,
    pub latency_mobile: Option<i32>,
    pub packet_loss: Option<f32>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ReportPayload {
    pub id: String,
    pub name: Option<String>,
    pub secret: String,
    pub country: Option<String>,
    pub city: Option<String>,
    pub cpu: f32,
    pub load: [f32; 3],
    pub mem_used: f64,
    pub mem_total: f64,
    pub disk_used: f64,
    pub disk_total: f64,
    pub net_rx: f64,
    pub net_tx: f64,
    pub traffic_in: f64,
    pub traffic_out: f64,
    pub uptime: i64,
    pub latency: Option<Latency>,
    pub loss: Option<f32>,
}

#[derive(Debug, Deserialize)]
pub struct Latency {
    pub telecom: Option<i32>,
    pub unicom: Option<i32>,
    pub mobile: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct AdminResetTraffic {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminRename {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminUpdateTrafficLimit {
    pub id: String,
    pub traffic_limit: f64,
    pub traffic_notify_percent: f64,
}

#[derive(Debug, Deserialize)]
pub struct AdminNotifySettings {
    pub telegram_enabled: bool,
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
    pub webhook_enabled: bool,
    pub webhook_url: String,
    pub offline_minutes: i64,
    pub cpu_threshold: f32,
    pub mem_threshold: f32,
}

#[derive(Debug)]
pub struct UpsertResult {
    pub id: String,
    pub name: String,
    pub cpu: f32,
    pub mem_used: f64,
    pub mem_total: f64,
    pub cum_in: f64,
    pub cum_out: f64,
    pub traffic_limit: f64,
    pub traffic_notify_percent: f64,
}

/// 機器 id 會被拿去組資料庫 primary key、也會被前端當成 HTML 屬性值使用
/// （例如 `id="name-${s.id}"`），限制字元集可以同時防止奇怪的 id 造成
/// 前端渲染出錯或被拿來做 XSS/HTML 注入。
pub fn valid_id(id: &str) -> bool {
    static RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"^[A-Za-z0-9._-]{1,64}$").unwrap());
    RE.is_match(id)
}

/// 顯示名稱 / 國家 / 城市這類自由文字欄位不限制字元（才能顯示中文等），
/// 但截斷長度，避免有人塞超長字串撐爆卡片版面或洗版資料庫。
pub fn clip(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

pub async fn init_db(database_url: &str) -> Result<SqlitePool> {
    // `create_if_missing` 讓伺服器在 DATABASE_URL 沒帶 `?mode=rwc`（例如映像檔的預設值）
    // 且資料庫檔案第一次不存在時，仍能自動建立，而不是直接崩潰退出。
    let opts = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(opts)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            country TEXT,
            city TEXT,
            cpu REAL NOT NULL,
            load1 REAL NOT NULL,
            load5 REAL NOT NULL,
            load15 REAL NOT NULL,
            mem_used REAL NOT NULL,
            mem_total REAL NOT NULL,
            disk_used REAL NOT NULL,
            disk_total REAL NOT NULL,
            net_rx REAL NOT NULL,
            net_tx REAL NOT NULL,
            cum_in REAL NOT NULL DEFAULT 0,
            cum_out REAL NOT NULL DEFAULT 0,
            last_raw_in REAL NOT NULL DEFAULT 0,
            last_raw_out REAL NOT NULL DEFAULT 0,
            traffic_start TEXT,
            traffic_limit REAL NOT NULL DEFAULT 0,
            traffic_notify_percent REAL NOT NULL DEFAULT 80,
            uptime INTEGER NOT NULL,
            latency_telecom INTEGER,
            latency_unicom INTEGER,
            latency_mobile INTEGER,
            packet_loss REAL,
            last_seen TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

pub async fn upsert_server(pool: &SqlitePool, payload: &ReportPayload) -> Result<UpsertResult> {
    let name = clip(&payload.name.clone().unwrap_or_else(|| payload.id.clone()), 64);
    let country = payload.country.as_deref().map(|c| clip(c, 8));
    let city = payload.city.as_deref().map(|c| clip(c, 64));
    let now = Utc::now();

    let (lt, lu, lm) = match &payload.latency {
        Some(l) => (l.telecom, l.unicom, l.mobile),
        None => (None, None, None),
    };

    let existing = sqlx::query_as::<_, ServerStatus>(
        "SELECT * FROM servers WHERE id = ?"
    )
    .bind(&payload.id)
    .fetch_optional(pool)
    .await?;

    let (cum_in, cum_out, last_raw_in, last_raw_out, traffic_start, traffic_limit, traffic_notify_percent) =
        if let Some(old) = existing {
            let mut new_cum_in = old.cum_in;
            let mut new_cum_out = old.cum_out;

            // 機器真的重開機時 uptime 會歸零/變小；只有這種情況才能確定網卡計數器
            // 也被系統重置了，這時候才把「這次上報的原始值」整個當作增量加回去。
            // 如果 uptime 沒有變小但計數器卻變小了（常見於 docker/veth 介面消失重建、
            // NIC 驅動重置等），那只是雜訊，不是真流量，增量記 0，避免把整個計數器
            // 的值誤當成新增流量灌進累計裡。
            let rebooted = payload.uptime < old.uptime;

            let delta_in = if payload.traffic_in >= old.last_raw_in {
                payload.traffic_in - old.last_raw_in
            } else if rebooted {
                payload.traffic_in
            } else {
                0.0
            };
            new_cum_in += delta_in;

            let delta_out = if payload.traffic_out >= old.last_raw_out {
                payload.traffic_out - old.last_raw_out
            } else if rebooted {
                payload.traffic_out
            } else {
                0.0
            };
            new_cum_out += delta_out;

            (
                new_cum_in,
                new_cum_out,
                payload.traffic_in,
                payload.traffic_out,
                old.traffic_start,
                old.traffic_limit,
                old.traffic_notify_percent,
            )
        } else {
            (
                0.0,
                0.0,
                payload.traffic_in,
                payload.traffic_out,
                Some(now.to_rfc3339()),
                0.0,
                80.0,
            )
        };

    sqlx::query(
        r#"
        INSERT INTO servers (
            id, name, country, city, cpu, load1, load5, load15,
            mem_used, mem_total, disk_used, disk_total,
            net_rx, net_tx, cum_in, cum_out, last_raw_in, last_raw_out,
            traffic_start, traffic_limit, traffic_notify_percent, uptime,
            latency_telecom, latency_unicom, latency_mobile, packet_loss, last_seen
        ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?, ?, ?
        )
        ON CONFLICT(id) DO UPDATE SET
            -- 注意：這裡刻意不更新 name。name 只在第一次 INSERT 時取用 Agent
            -- 上報的名稱；之後一律由管理後台的「修改名稱」決定，
            -- 否則 Agent 每次上報都會把管理員剛改好的顯示名稱蓋回去。
            country = COALESCE(excluded.country, servers.country),
            city = COALESCE(excluded.city, servers.city),
            cpu = excluded.cpu,
            load1 = excluded.load1,
            load5 = excluded.load5,
            load15 = excluded.load15,
            mem_used = excluded.mem_used,
            mem_total = excluded.mem_total,
            disk_used = excluded.disk_used,
            disk_total = excluded.disk_total,
            net_rx = excluded.net_rx,
            net_tx = excluded.net_tx,
            cum_in = excluded.cum_in,
            cum_out = excluded.cum_out,
            last_raw_in = excluded.last_raw_in,
            last_raw_out = excluded.last_raw_out,
            uptime = excluded.uptime,
            latency_telecom = excluded.latency_telecom,
            latency_unicom = excluded.latency_unicom,
            latency_mobile = excluded.latency_mobile,
            packet_loss = excluded.packet_loss,
            last_seen = excluded.last_seen
        "#,
    )
    .bind(&payload.id)
    .bind(&name)
    .bind(&country)
    .bind(&city)
    .bind(payload.cpu)
    .bind(payload.load[0])
    .bind(payload.load[1])
    .bind(payload.load[2])
    .bind(payload.mem_used)
    .bind(payload.mem_total)
    .bind(payload.disk_used)
    .bind(payload.disk_total)
    .bind(payload.net_rx)
    .bind(payload.net_tx)
    .bind(cum_in)
    .bind(cum_out)
    .bind(last_raw_in)
    .bind(last_raw_out)
    .bind(&traffic_start)
    .bind(traffic_limit)
    .bind(traffic_notify_percent)
    .bind(payload.uptime)
    .bind(lt)
    .bind(lu)
    .bind(lm)
    .bind(payload.loss)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;

    Ok(UpsertResult {
        id: payload.id.clone(),
        name,
        cpu: payload.cpu,
        mem_used: payload.mem_used,
        mem_total: payload.mem_total,
        cum_in,
        cum_out,
        traffic_limit,
        traffic_notify_percent,
    })
}

pub async fn get_all_servers(pool: &SqlitePool) -> Result<Vec<ServerStatus>> {
    let rows = sqlx::query_as::<_, ServerStatus>(
        "SELECT * FROM servers ORDER BY name"
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn reset_traffic(pool: &SqlitePool, id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        r#"
        UPDATE servers
        SET cum_in = 0,
            cum_out = 0,
            traffic_start = ?
        WHERE id = ?
        "#,
    )
    .bind(now)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn rename_server(pool: &SqlitePool, id: &str, name: &str) -> Result<()> {
    let name = clip(name, 64);
    sqlx::query("UPDATE servers SET name = ? WHERE id = ?")
        .bind(name)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_traffic_limit(
    pool: &SqlitePool,
    id: &str,
    limit: f64,
    percent: f64,
) -> Result<()> {
    sqlx::query(
        "UPDATE servers SET traffic_limit = ?, traffic_notify_percent = ? WHERE id = ?"
    )
    .bind(limit)
    .bind(percent)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}
