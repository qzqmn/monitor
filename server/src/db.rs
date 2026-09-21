use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqlitePoolOptions, FromRow, SqlitePool};
use anyhow::Result;

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
    pub admin_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminRename {
    pub id: String,
    pub name: String,
    pub admin_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminUpdateTrafficLimit {
    pub id: String,
    pub traffic_limit: f64,
    pub traffic_notify_percent: f64,
    pub admin_secret: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminNotifySettings {
    pub admin_secret: String,
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

pub async fn init_db(database_url: &str) -> Result<SqlitePool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(database_url)
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
    let name = payload.name.clone().unwrap_or_else(|| payload.id.clone());
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

            let mut delta_in = payload.traffic_in - old.last_raw_in;
            if delta_in < 0.0 {
                delta_in = payload.traffic_in;
            }
            new_cum_in += delta_in;

            let mut delta_out = payload.traffic_out - old.last_raw_out;
            if delta_out < 0.0 {
                delta_out = payload.traffic_out;
            }
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
            name = COALESCE(excluded.name, servers.name),
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
    .bind(&payload.country)
    .bind(&payload.city)
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
