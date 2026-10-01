use chrono::{DateTime, Datelike, Utc};
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
    pub traffic_count_mode: String,
    pub traffic_reset_day: i64,
    #[serde(skip_serializing)]
    #[allow(dead_code)] // 只在 FromRow 讀取時用到，供未來除錯/顯示用途保留
    pub last_auto_reset_ym: Option<String>,
    pub uptime: i64,
    pub last_seen: DateTime<Utc>,
    /// Agent 上報認證用的專屬 token。故意 skip_serializing——這個欄位只能透過
    /// 已登入的管理端點手動取用（見 agents.rs），絕不能出現在 /api/servers
    /// 這種公開端點的 JSON 裡。
    #[serde(skip_serializing)]
    pub token: Option<String>,
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
    /// 每月流量自動歸零的日子（1-31）。0 或缺省 = 不自動歸零，
    /// 沿用舊行為（只能按「重設月流量統計」手動歸零）。
    #[serde(default)]
    pub traffic_reset_day: i64,
    /// 計費方向：total（預設，上下行合計）/ outbound（只算出站）/ inbound（只算入站）。
    #[serde(default = "default_count_mode")]
    pub traffic_count_mode: String,
}

fn default_count_mode() -> String {
    "total".to_string()
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
    pub traffic_count_mode: String,
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
            traffic_count_mode TEXT NOT NULL DEFAULT 'total',
            uptime INTEGER NOT NULL,
            last_seen TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // 輕量遷移：對已經在跑、資料庫裡已有 servers 表的舊安裝，補上這兩個新欄位。
    // SQLite 沒有 `ADD COLUMN IF NOT EXISTS`，欄位已存在時 ALTER 會回錯，
    // 直接忽略該錯誤即可（新建的資料庫這裡也會跑一次，欄位一樣會被補上）。
    let _ = sqlx::query(
        "ALTER TABLE servers ADD COLUMN traffic_reset_day INTEGER NOT NULL DEFAULT 0",
    )
    .execute(&pool)
    .await;
    let _ = sqlx::query("ALTER TABLE servers ADD COLUMN last_auto_reset_ym TEXT")
        .execute(&pool)
        .await;
    let _ = sqlx::query("ALTER TABLE servers ADD COLUMN token TEXT")
        .execute(&pool)
        .await;
    // 計費方向：total（上下行合計，舊行為/預設）、outbound（只算出站/上傳）、
    // inbound（只算入站/下載）。很多 VPS 商只對出站流量收費，所以開放選擇。
    let _ = sqlx::query(
        "ALTER TABLE servers ADD COLUMN traffic_count_mode TEXT NOT NULL DEFAULT 'total'",
    )
    .execute(&pool)
    .await;

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

    // 單一管理員帳號：用 CHECK(id = 1) 讓這張表天生只能有一列，
    // 不需要額外程式碼防止重複註冊出第二個帳號。
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS admin_account (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            username TEXT NOT NULL,
            password_hash TEXT NOT NULL,
            created_at TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            created_at TEXT NOT NULL,
            expires_at TEXT NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;

    // 歷史趨勢資料：背景任務每分鐘幫每台機器存一筆快照（見 main.rs 的
    // 60 秒迴圈），跟 Agent 實際上報間隔（可能設 5 秒也可能設 60 秒）脫鉤，
    // 不會因為有人把上報間隔設很短就爆量。舊資料由同一個迴圈定期清掉
    // （見 prune_history），保留 7 天，24 小時視圖用原始點、7 天視圖在查詢
    // 時聚合成小時級。
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS server_history (
            server_id TEXT NOT NULL,
            ts TEXT NOT NULL,
            cpu REAL NOT NULL,
            mem_used REAL NOT NULL,
            mem_total REAL NOT NULL,
            disk_used REAL NOT NULL,
            disk_total REAL NOT NULL,
            net_rx REAL NOT NULL,
            net_tx REAL NOT NULL,
            cum_in REAL NOT NULL,
            cum_out REAL NOT NULL
        )
        "#,
    )
    .execute(&pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_server_history_id_ts ON server_history(server_id, ts)")
        .execute(&pool)
        .await?;

    // 舊安裝升級：既有機器原本是靠全域 REPORT_SECRET 認證，沒有各自的 token。
    // 這裡幫每一筆還沒有 token 的既有機器補發一組，資料（累計流量、名稱、
    // 流量上限設定等）完全保留，管理員只需要把新 token 貼到該台機器的
    // Agent 設定裡即可，不用刪掉重建。
    let need_token: Vec<(String,)> =
        sqlx::query_as("SELECT id FROM servers WHERE token IS NULL OR token = ''")
            .fetch_all(&pool)
            .await?;
    for (id,) in need_token {
        let token = gen_random_token();
        sqlx::query("UPDATE servers SET token = ? WHERE id = ?")
            .bind(token)
            .bind(id)
            .execute(&pool)
            .await?;
    }

    Ok(pool)
}

/// 產生一組 32 bytes（64 個十六進位字元）的高強度隨機 token，
/// 用在 Agent 上報密鑰和登入 session 上。用 argon2 依賴帶進來的
/// OS 隨機數產生器，不用再多加一個 `rand` crate。
pub fn gen_random_token() -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub async fn upsert_server(pool: &SqlitePool, payload: &ReportPayload) -> Result<UpsertResult> {
    let name = clip(&payload.name.clone().unwrap_or_else(|| payload.id.clone()), 64);
    let country = payload.country.as_deref().map(|c| clip(c, 8));
    let city = payload.city.as_deref().map(|c| clip(c, 64));
    let now = Utc::now();

    let existing = sqlx::query_as::<_, ServerStatus>(
        "SELECT * FROM servers WHERE id = ?"
    )
    .bind(&payload.id)
    .fetch_optional(pool)
    .await?;

    // 告警/通知要用「後台登記或改過的名稱」，不是 Agent 自己上報的 AGENT_NAME：
    // 資料庫裡的 name 才是管理員說了算的（ON CONFLICT 也刻意不更新它）。
    let display_name = existing
        .as_ref()
        .map(|o| o.name.clone())
        .unwrap_or_else(|| name.clone());

    let (cum_in, cum_out, last_raw_in, last_raw_out, traffic_start, traffic_limit, traffic_notify_percent, traffic_count_mode) =
        if let Some(old) = existing.as_ref().filter(|o| o.traffic_start.is_none()) {
            // 後台「新增機器」建的占位資料列還沒有任何基準（traffic_start 是 NULL、
            // last_raw_* 是 0）。第一次真的上報時，Agent 帶來的是「開機以來」的
            // 原始計數器；如果照一般流程算「本次 − 上次(0)」，就會把整個開機以來的
            // 流量（可能好幾 TB）一次灌進本月累計，還會馬上誤觸流量告警。
            // 所以第一次只記基準，累計維持 0。
            (
                old.cum_in,
                old.cum_out,
                payload.traffic_in,
                payload.traffic_out,
                Some(now.to_rfc3339()),
                old.traffic_limit,
                old.traffic_notify_percent,
                old.traffic_count_mode.clone(),
            )
        } else if let Some(old) = existing {
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
                old.traffic_count_mode,
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
                "total".to_string(),
            )
        };

    sqlx::query(
        r#"
        INSERT INTO servers (
            id, name, country, city, cpu, load1, load5, load15,
            mem_used, mem_total, disk_used, disk_total,
            net_rx, net_tx, cum_in, cum_out, last_raw_in, last_raw_out,
            traffic_start, traffic_limit, traffic_notify_percent, traffic_count_mode,
            uptime, last_seen
        ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?,
            ?, ?
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
            traffic_start = COALESCE(servers.traffic_start, excluded.traffic_start),
            uptime = excluded.uptime,
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
    .bind(&traffic_count_mode)
    .bind(payload.uptime)
    .bind(now.to_rfc3339())
    .execute(pool)
    .await?;

    Ok(UpsertResult {
        id: payload.id.clone(),
        name: display_name,
        cpu: payload.cpu,
        mem_used: payload.mem_used,
        mem_total: payload.mem_total,
        cum_in,
        cum_out,
        traffic_limit,
        traffic_notify_percent,
        traffic_count_mode,
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

/// 建立一台新機器的登記資料：產生一組專屬 token，插入一筆「還沒有任何
/// 上報數據」的占位資料列。回傳的 token 只有這一刻的呼叫端看得到明碼，
/// 之後只能透過 list_agents_admin() 再查一次（存在資料庫裡，不是雜湊，
/// 因為 Agent 之後每次上報都要能拿它來比對，這跟使用者密碼不同，
/// 不能只存雜湊）。
pub async fn create_agent(pool: &SqlitePool, id: &str, name: &str) -> Result<String> {
    let token = gen_random_token();
    let name = clip(name, 64);
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO servers (
            id, name, cpu, load1, load5, load15, mem_used, mem_total,
            disk_used, disk_total, net_rx, net_tx, uptime, last_seen, token
        ) VALUES (?, ?, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, ?, ?)
        "#,
    )
    .bind(id)
    .bind(name)
    .bind(now)
    .bind(&token)
    .execute(pool)
    .await?;
    Ok(token)
}

/// 給後台「機器管理」列表用：跟 get_all_servers 一樣的資料，但額外把
/// token 一起帶出來（此函式只給已經過 require_admin 認證的端點呼叫）。
pub async fn list_agents_admin(pool: &SqlitePool) -> Result<Vec<ServerStatus>> {
    get_all_servers(pool).await
}

/// 查這個 id 目前登記的 token，report handler 拿它跟 Agent 上報帶的
/// secret 做常數時間比對。查無此 id 回 None，report handler 據此拒絕
/// 未登記過的機器上報——這是取代舊版「全域 REPORT_SECRET、隨便填 id
/// 都能自動建立新機器」的核心改動。
pub async fn get_agent_token(pool: &SqlitePool, id: &str) -> Result<Option<String>> {
    let row: Option<(Option<String>,)> = sqlx::query_as("SELECT token FROM servers WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await?;
    Ok(row.and_then(|(t,)| t))
}

/// 重新產生某台機器的 token（舊 token 立刻失效），用在懷疑外洩、
/// 或單純想換一組時，不影響其他機器。
pub async fn rotate_agent_token(pool: &SqlitePool, id: &str) -> Result<String> {
    let token = gen_random_token();
    sqlx::query("UPDATE servers SET token = ? WHERE id = ?")
        .bind(&token)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(token)
}

pub async fn delete_agent(pool: &SqlitePool, id: &str) -> Result<()> {
    sqlx::query("DELETE FROM servers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    // 機器刪掉了，歷史趨勢資料也一併清掉，不留孤兒資料。
    sqlx::query("DELETE FROM server_history WHERE server_id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------
// 歷史趨勢
// ---------------------------------------------------------------------

/// 機器超過這麼久沒上報就視為離線。api.rs（首頁卡片綠燈/灰燈）、
/// agents.rs（後台機器列表）、這裡（要不要繼續往歷史趨勢記快照）三處
/// 都用這一個門檻，之前是三個地方各自寫一份 `90`，改掉一處很容易忘記
/// 改到另一處，導致「線上/離線」判斷在不同頁面顯示不一致。
pub const ONLINE_STALE_SECS: i64 = 90;

/// 每分鐘幫「目前線上的每一台機器」存一筆快照，跟 Agent 上報頻率脫鉤。
/// 離線的機器不再繼續存快照（只是重複寫入它離線前最後一筆的舊數值），
/// 這樣歷史趨勢圖在離線期間會是真正的資料缺口，而不是一條看起來像
/// 「這段時間都正常運作」的假平線。
pub async fn record_history_snapshot(pool: &SqlitePool) -> Result<()> {
    let now = Utc::now();
    let online_cutoff = (now - chrono::Duration::seconds(ONLINE_STALE_SECS)).to_rfc3339();
    sqlx::query(
        r#"
        INSERT INTO server_history
            (server_id, ts, cpu, mem_used, mem_total, disk_used, disk_total, net_rx, net_tx, cum_in, cum_out)
        SELECT id, ?, cpu, mem_used, mem_total, disk_used, disk_total, net_rx, net_tx, cum_in, cum_out
        FROM servers
        WHERE uptime > 0 AND last_seen >= ?
        "#,
    )
    .bind(now.to_rfc3339())
    .bind(online_cutoff)
    .execute(pool)
    .await?;
    Ok(())
}

/// 只留最近 7 天，舊的定期清掉，避免資料庫無限長大。
pub async fn prune_history(pool: &SqlitePool) -> Result<()> {
    let cutoff = (Utc::now() - chrono::Duration::days(7)).to_rfc3339();
    sqlx::query("DELETE FROM server_history WHERE ts < ?")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct HistoryPoint {
    pub ts: String,
    pub cpu: f64,
    pub mem_used: f64,
    pub mem_total: f64,
    pub disk_used: f64,
    pub disk_total: f64,
    pub net_rx: f64,
    pub net_tx: f64,
    pub cum_in: f64,
    pub cum_out: f64,
}

/// range="7d" 回傳最近 7 天、聚合成小時級（避免一次丟上萬個原始點給前端畫圖）；
/// 其他值（含沒帶或帶錯值）一律當 24 小時，回傳原始點（背景任務每分鐘存一筆，
/// 24 小時大約 1440 點，前端畫線圖還算負擔得起）。
pub async fn get_history(pool: &SqlitePool, id: &str, range: &str) -> Result<Vec<HistoryPoint>> {
    let days = if range == "7d" { 7 } else { 1 };
    let since = (Utc::now() - chrono::Duration::days(days)).to_rfc3339();

    let rows: Vec<HistoryPoint> = sqlx::query_as(
        r#"
        SELECT ts, cpu, mem_used, mem_total, disk_used, disk_total, net_rx, net_tx, cum_in, cum_out
        FROM server_history
        WHERE server_id = ? AND ts >= ?
        ORDER BY ts ASC
        "#,
    )
    .bind(id)
    .bind(since)
    .fetch_all(pool)
    .await?;

    if range != "7d" {
        return Ok(rows);
    }

    // 7 天視圖：用時間戳前 13 碼（"YYYY-MM-DDTHH"，RFC3339 固定寬度，不用另外
    // parse）當小時桶的 key。瞬時指標（CPU/記憶體/硬碟/網速）取平均，累計流量
    // 取該小時內最大值——平均一個遞增的計數器沒有意義，會把當小時的真實用量
    // 打折扣，這裡改用它自然的意義：該小時「結束時」累計到多少。
    use std::collections::BTreeMap;
    #[derive(Default, Clone)]
    struct Acc {
        n: f64,
        cpu: f64,
        mem_used: f64,
        mem_total: f64,
        disk_used: f64,
        disk_total: f64,
        net_rx: f64,
        net_tx: f64,
        cum_in: f64,
        cum_out: f64,
    }
    let mut buckets: BTreeMap<String, Acc> = BTreeMap::new();
    for r in rows {
        let key: String = r.ts.chars().take(13).collect();
        let acc = buckets.entry(key).or_default();
        acc.n += 1.0;
        acc.cpu += r.cpu;
        acc.mem_used += r.mem_used;
        acc.mem_total += r.mem_total;
        acc.disk_used += r.disk_used;
        acc.disk_total += r.disk_total;
        acc.net_rx += r.net_rx;
        acc.net_tx += r.net_tx;
        acc.cum_in = acc.cum_in.max(r.cum_in);
        acc.cum_out = acc.cum_out.max(r.cum_out);
    }

    Ok(buckets
        .into_iter()
        .map(|(key, acc)| HistoryPoint {
            ts: format!("{}:00:00Z", key),
            cpu: acc.cpu / acc.n,
            mem_used: acc.mem_used / acc.n,
            mem_total: acc.mem_total / acc.n,
            disk_used: acc.disk_used / acc.n,
            disk_total: acc.disk_total / acc.n,
            net_rx: acc.net_rx / acc.n,
            net_tx: acc.net_tx / acc.n,
            cum_in: acc.cum_in,
            cum_out: acc.cum_out,
        })
        .collect())
}

// ---------------------------------------------------------------------
// 管理員帳號 / 登入 session
// ---------------------------------------------------------------------

pub async fn admin_exists(pool: &SqlitePool) -> Result<bool> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT id FROM admin_account WHERE id = 1")
        .fetch_optional(pool)
        .await?;
    Ok(row.is_some())
}

pub async fn create_admin(pool: &SqlitePool, username: &str, password_hash: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO admin_account (id, username, password_hash, created_at) VALUES (1, ?, ?, ?)",
    )
    .bind(username)
    .bind(password_hash)
    .bind(Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// 回傳 (username, password_hash)
pub async fn get_admin(pool: &SqlitePool) -> Result<Option<(String, String)>> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT username, password_hash FROM admin_account WHERE id = 1")
            .fetch_optional(pool)
            .await?;
    Ok(row)
}

pub async fn update_admin_password(pool: &SqlitePool, password_hash: &str) -> Result<()> {
    sqlx::query("UPDATE admin_account SET password_hash = ? WHERE id = 1")
        .bind(password_hash)
        .execute(pool)
        .await?;
    Ok(())
}

const SESSION_TTL_DAYS: i64 = 30;

pub async fn create_session(pool: &SqlitePool) -> Result<String> {
    let token = gen_random_token();
    let now = Utc::now();
    let expires = now + chrono::Duration::days(SESSION_TTL_DAYS);
    sqlx::query("INSERT INTO sessions (token, created_at, expires_at) VALUES (?, ?, ?)")
        .bind(&token)
        .bind(now.to_rfc3339())
        .bind(expires.to_rfc3339())
        .execute(pool)
        .await?;
    Ok(token)
}

/// session 是否有效（存在且未過期）。過期的session 這裡順手清掉，
/// 不用另外排一個清理排程。
pub async fn session_valid(pool: &SqlitePool, token: &str) -> Result<bool> {
    let row: Option<(String,)> = sqlx::query_as("SELECT expires_at FROM sessions WHERE token = ?")
        .bind(token)
        .fetch_optional(pool)
        .await?;
    match row {
        None => Ok(false),
        Some((expires_at,)) => {
            let expired = chrono::DateTime::parse_from_rfc3339(&expires_at)
                .map(|t| t.with_timezone(&Utc) < Utc::now())
                .unwrap_or(true);
            if expired {
                let _ = sqlx::query("DELETE FROM sessions WHERE token = ?")
                    .bind(token)
                    .execute(pool)
                    .await;
                Ok(false)
            } else {
                Ok(true)
            }
        }
    }
}

pub async fn delete_session(pool: &SqlitePool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn reset_traffic(pool: &SqlitePool, id: &str) -> Result<()> {
    let now = Utc::now();
    // 手動按「重設流量」也順便記一筆 last_auto_reset_ym，等於「這個月已經處理過了」，
    // 避免同一個月稍後又被自動歸零的排程再打一次（例如重置日設 15 號、
    // 但使用者在 10 號就手動按了重設，15 號那天就不需要再自動重設一次）。
    let ym = now.format("%Y-%m").to_string();
    sqlx::query(
        r#"
        UPDATE servers
        SET cum_in = 0,
            cum_out = 0,
            traffic_start = ?,
            last_auto_reset_ym = ?
        WHERE id = ?
        "#,
    )
    .bind(now.to_rfc3339())
    .bind(ym)
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
    reset_day: i64,
    count_mode: &str,
) -> Result<()> {
    // 1-31 才是合法的「每月幾號」；其他值（含 0）一律當成「關閉自動歸零」存成 0，
    // 不讓後台誤填的奇怪數字（例如 -5、99）進到自動歸零邏輯裡。
    let reset_day = if (1..=31).contains(&reset_day) { reset_day } else { 0 };
    let count_mode = match count_mode {
        "outbound" | "inbound" => count_mode,
        _ => "total",
    };
    sqlx::query(
        "UPDATE servers SET traffic_limit = ?, traffic_notify_percent = ?, traffic_reset_day = ?, traffic_count_mode = ? WHERE id = ?"
    )
    .bind(limit)
    .bind(percent)
    .bind(reset_day)
    .bind(count_mode)
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// 這個月的最後一天是幾號（處理 2 月、30 天月份等）。
fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 { (year + 1, 1) } else { (year, month + 1) };
    let first_of_next = chrono::NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid date");
    first_of_next.pred_opt().expect("valid date").day()
}

/// 檢查所有設定了「每月幾號自動歸零」的機器，該歸零的就歸零。
/// 用「今天幾號 >= 這個月的有效重置日，且這個月還沒重置過」判斷，
/// 而不是「今天剛好等於重置日」：這樣即使伺服器在重置日當天剛好離線、
/// 或重置日設 31 號但這個月只有 30 天，之後補跑一樣抓得到，不會整月漏掉。
/// 回傳這次有被歸零的 (id, name)，方便上層記日誌或發通知。
pub async fn auto_reset_due_traffic(pool: &SqlitePool) -> Result<Vec<(String, String)>> {
    let now = Utc::now();
    let this_ym = now.format("%Y-%m").to_string();
    let today = now.day();
    let last_day = last_day_of_month(now.year(), now.month());

    let candidates: Vec<(String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT id, name, traffic_reset_day, last_auto_reset_ym FROM servers WHERE traffic_reset_day > 0",
    )
    .fetch_all(pool)
    .await?;

    let mut reset_list = Vec::new();
    for (id, name, reset_day, last_ym) in candidates {
        if last_ym.as_deref() == Some(this_ym.as_str()) {
            continue; // 這個月已經重置過了
        }
        // 重置日設 31 號、但這個月只有 30 天（或 2 月）時，改用這個月的最後一天，
        // 不然永遠等不到「31 號」而整個月都不會自動歸零。
        let effective_day = (reset_day as u32).min(last_day);
        if today >= effective_day {
            sqlx::query(
                r#"
                UPDATE servers
                SET cum_in = 0, cum_out = 0, traffic_start = ?, last_auto_reset_ym = ?
                WHERE id = ?
                "#,
            )
            .bind(now.to_rfc3339())
            .bind(&this_ym)
            .bind(&id)
            .execute(pool)
            .await?;
            reset_list.push((id, name));
        }
    }
    Ok(reset_list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_day_of_month_handles_month_boundaries() {
        assert_eq!(last_day_of_month(2026, 2), 28); // 2026 不是閏年
        assert_eq!(last_day_of_month(2024, 2), 29); // 2024 是閏年
        assert_eq!(last_day_of_month(2026, 4), 30);
        assert_eq!(last_day_of_month(2026, 1), 31);
        assert_eq!(last_day_of_month(2026, 12), 31);
    }

    fn payload(id: &str, token: &str, traffic_in: f64, traffic_out: f64, uptime: i64) -> ReportPayload {
        ReportPayload {
            id: id.to_string(),
            name: None,
            secret: token.to_string(),
            country: None,
            city: None,
            cpu: 1.0,
            load: [0.0; 3],
            mem_used: 1.0,
            mem_total: 4.0,
            disk_used: 1.0,
            disk_total: 10.0,
            net_rx: 0.0,
            net_tx: 0.0,
            traffic_in,
            traffic_out,
            uptime,
        }
    }

    /// 用暫存檔案當資料庫（不用 :memory:，因為連線池有多條連線，
    /// 每條 :memory: 連線各自是一個空資料庫）。
    async fn temp_pool(tag: &str) -> SqlitePool {
        let path = std::env::temp_dir().join(format!(
            "vps-monitor-test-{}-{}-{}.db",
            tag,
            std::process::id(),
            gen_random_token()
        ));
        init_db(&format!("sqlite:{}", path.display())).await.unwrap()
    }

    /// 回歸測試：後台登記的機器第一次上報時，Agent 帶來的是「開機以來」的原始
    /// 計數器，只能當基準，不能被當成本月增量灌進累計流量。
    #[tokio::test]
    async fn first_report_after_registration_only_sets_baseline() {
        let pool = temp_pool("first-report").await;
        let token = create_agent(&pool, "vps-a", "A").await.unwrap();

        let r1 = upsert_server(&pool, &payload("vps-a", &token, 3000.0, 2000.0, 100_000)).await.unwrap();
        assert_eq!((r1.cum_in, r1.cum_out), (0.0, 0.0));
        assert_eq!(r1.name, "A", "告警用的名稱要是後台登記的，不是 Agent 自報的 id/AGENT_NAME");

        let row = get_all_servers(&pool).await.unwrap().remove(0);
        assert!(row.traffic_start.is_some(), "第一次上報後應該有記錄開始計數時間");

        // 之後的增量照常累計
        let r2 = upsert_server(&pool, &payload("vps-a", &token, 3001.0, 2003.0, 100_020)).await.unwrap();
        assert_eq!((r2.cum_in, r2.cum_out), (1.0, 3.0));
    }

    /// 還沒上報過的占位機器不該被寫進歷史趨勢（否則會累積全 0 的假資料點）。
    #[tokio::test]
    async fn history_snapshot_skips_never_reported_agents() {
        let pool = temp_pool("history").await;
        let token = create_agent(&pool, "vps-a", "A").await.unwrap();
        create_agent(&pool, "vps-b", "B").await.unwrap();
        upsert_server(&pool, &payload("vps-a", &token, 1.0, 1.0, 100)).await.unwrap();

        record_history_snapshot(&pool).await.unwrap();

        let a = get_history(&pool, "vps-a", "24h").await.unwrap();
        let b = get_history(&pool, "vps-b", "24h").await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 0);
    }

    /// 機器離線後不該繼續被寫進歷史（否則離線期間會被畫成一條假平線，
    /// 而不是真正的資料缺口）；同時間仍在線的機器要照常繼續記錄。
    #[tokio::test]
    async fn record_history_snapshot_skips_offline_servers() {
        let pool = temp_pool("offline-history").await;
        let token_a = create_agent(&pool, "vps-a", "A").await.unwrap();
        let token_b = create_agent(&pool, "vps-b", "B").await.unwrap();
        upsert_server(&pool, &payload("vps-a", &token_a, 1.0, 1.0, 100)).await.unwrap();
        upsert_server(&pool, &payload("vps-b", &token_b, 1.0, 1.0, 100)).await.unwrap();

        // 把 vps-b 的 last_seen 往回改到「早就超過線上判定門檻」，
        // 模擬它已經離線一段時間（uptime 欄位還留著離線前的最後數值，
        // 這正是舊版只看 uptime > 0 會誤判成「還在線」的情況）。
        let stale = (Utc::now() - chrono::Duration::seconds(ONLINE_STALE_SECS + 60)).to_rfc3339();
        sqlx::query("UPDATE servers SET last_seen = ? WHERE id = 'vps-b'")
            .bind(stale)
            .execute(&pool)
            .await
            .unwrap();

        record_history_snapshot(&pool).await.unwrap();

        let a = get_history(&pool, "vps-a", "24h").await.unwrap();
        let b = get_history(&pool, "vps-b", "24h").await.unwrap();
        assert_eq!(a.len(), 1, "在線機器應該照常被記錄");
        assert_eq!(b.len(), 0, "離線機器不該再被寫進歷史");
    }
}
