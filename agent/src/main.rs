use anyhow::{Context, Result};
use serde::Serialize;
use std::{env, time::Duration};
use sysinfo::{Disks, Networks, System};
use tracing::{error, info};

#[derive(Serialize)]
struct Report {
    id: String,
    name: Option<String>,
    secret: String,
    country: Option<String>,
    city: Option<String>,
    cpu: f32,
    load: [f32; 3],
    mem_used: f64,
    mem_total: f64,
    disk_used: f64,
    disk_total: f64,
    net_rx: f64,
    net_tx: f64,
    traffic_in: f64,
    traffic_out: f64,
    uptime: i64,
    latency: Option<Latency>,
    loss: Option<f32>,
}

#[derive(Serialize)]
struct Latency {
    telecom: Option<i32>,
    unicom: Option<i32>,
    mobile: Option<i32>,
}

struct Config {
    server_url: String,
    agent_id: String,
    agent_name: String,
    secret: String,
    interval_secs: u64,
    net_ifaces: Option<Vec<String>>,
}

/// 預設要排除的虛擬/內部網卡：loopback、Docker 網橋與 veth、Tailscale、
/// WireGuard、libvirt、CNI 等。這些介面的流量要嘛是本機內部流量（不該算
/// 對外流量），要嘛會在容器重建時消失又出現，讓上報的計數器忽大忽小，
/// 誤觸「重開機」判斷把整個計數器灌進累計流量。真的要監控這些介面可以用
/// NET_IFACES 白名單覆蓋這個預設排除清單。
fn is_virtual_iface(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "lo", "docker", "br-", "veth", "virbr", "tailscale", "wg", "cni", "flannel", "cali",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

fn hostname() -> String {
    // 優先讀系統 hostname；失敗則用容器/環境常見後備
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| env::var("HOSTNAME").ok().filter(|s| !s.is_empty()))
        .or_else(|| env::var("HOST").ok().filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "unknown-host".to_string())
}

fn load_config() -> Result<Config> {
    let host = hostname();
    let agent_id = env::var("AGENT_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| host.clone());
    let agent_name = env::var("AGENT_NAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| agent_id.clone());

    Ok(Config {
        server_url: env::var("MONITOR_URL").context("MONITOR_URL required")?,
        agent_id,
        agent_name,
        secret: env::var("REPORT_SECRET").context("REPORT_SECRET required")?,
        interval_secs: env::var("INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(20),
        // 可選：手動指定要統計的網卡名稱（逗號分隔，例如 "eth0,ens18"）。
        // 沒設定或設成空字串就用 is_virtual_iface 的預設黑名單排除法——
        // 這裡一定要把空字串也當成「沒設定」，否則 docker-compose 用
        // `${NET_IFACES:-}` 帶出空字串時，會被解析成「白名單清單是空的」，
        // 導致所有網卡都被排除、流量直接變成 0。
        net_ifaces: env::var("NET_IFACES")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            }),
    })
}

async fn fetch_geo() -> (Option<String>, Option<String>) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok();
    let Some(client) = client else {
        return (None, None);
    };
    // ip-api.com 免費免 key
    match client
        .get("http://ip-api.com/json/?fields=status,countryCode,city")
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                if v.get("status").and_then(|s| s.as_str()) == Some("success") {
                    let country = v
                        .get("countryCode")
                        .and_then(|c| c.as_str())
                        .map(|s| s.to_string());
                    let city = v.get("city").and_then(|c| c.as_str()).map(|s| s.to_string());
                    return (country, city);
                }
            }
            (None, None)
        }
        _ => (None, None),
    }
}

fn collect_metrics(
    sys: &mut System,
    prev_rx: &mut u64,
    prev_tx: &mut u64,
    country: &Option<String>,
    city: &Option<String>,
    cfg: &Config,
) -> Report {
    sys.refresh_cpu_all();
    sys.refresh_memory();

    let cpu = sys.global_cpu_usage();
    let load = System::load_average();
    let load_arr = [load.one as f32, load.five as f32, load.fifteen as f32];

    let mem_total = sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let mem_used = sys.used_memory() as f64 / 1024.0 / 1024.0 / 1024.0;

    let disks = Disks::new_with_refreshed_list();
    let mut disk_total_bytes = 0u64;
    let mut disk_used_bytes = 0u64;

    // 只匹配根目錄 / 所在的主硬碟，避免累加 Docker 虛擬分區
    for d in disks.list() {
        if d.mount_point() == std::path::Path::new("/") {
            disk_total_bytes = d.total_space();
            disk_used_bytes = d.total_space().saturating_sub(d.available_space());
            break;
        }
    }

    // 防護機制：如果沒匹配到 /，則取第一個實體分區
    if disk_total_bytes == 0 && !disks.list().is_empty() {
        if let Some(d) = disks.list().first() {
            disk_total_bytes = d.total_space();
            disk_used_bytes = d.total_space().saturating_sub(d.available_space());
        }
    }

    let disk_total_gb = disk_total_bytes as f64 / 1024.0 / 1024.0 / 1024.0;
    let disk_used_gb = disk_used_bytes as f64 / 1024.0 / 1024.0 / 1024.0;

    let networks = Networks::new_with_refreshed_list();
    let mut total_rx = 0u64;
    let mut total_tx = 0u64;
    for (name, data) in networks.list() {
        let counted = match &cfg.net_ifaces {
            Some(allowlist) => allowlist.iter().any(|a| a == name),
            None => !is_virtual_iface(name),
        };
        if counted {
            total_rx += data.total_received();
            total_tx += data.total_transmitted();
        }
    }

    // 速率 KB/s（與上次差值 / 間隔近似，首次為 0）
    let interval = cfg.interval_secs.max(1) as f64;
    let net_rx = if *prev_rx > 0 && total_rx >= *prev_rx {
        (total_rx - *prev_rx) as f64 / 1024.0 / interval
    } else {
        0.0
    };
    let net_tx = if *prev_tx > 0 && total_tx >= *prev_tx {
        (total_tx - *prev_tx) as f64 / 1024.0 / interval
    } else {
        0.0
    };
    *prev_rx = total_rx;
    *prev_tx = total_tx;

    // 累計流量以 GB 上報（原始系統計數器）
    let traffic_in = total_rx as f64 / 1024.0 / 1024.0 / 1024.0;
    let traffic_out = total_tx as f64 / 1024.0 / 1024.0 / 1024.0;

    let uptime = System::uptime() as i64;

    Report {
        id: cfg.agent_id.clone(),
        name: Some(cfg.agent_name.clone()),
        secret: cfg.secret.clone(),
        country: country.clone(),
        city: city.clone(),
        cpu,
        load: load_arr,
        mem_used,
        mem_total,
        disk_used: disk_used_gb,
        disk_total: disk_total_gb,
        net_rx,
        net_tx,
        traffic_in,
        traffic_out,
        uptime,
        latency: None,
        loss: None,
    }
}

async fn send_report(client: &reqwest::Client, url: &str, report: &Report) -> Result<()> {
    let resp = client
        .post(url)
        .json(report)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .context("send failed")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("server returned {}: {}", status, body);
    }
    Ok(())
}

// 監聽 SIGTERM / Ctrl+C，等收到就完成呢個 future。
// 用喺 tokio::select! 入面同「等落一次上報」做競賽，邊個先到就邊個贏，
// 咁樣 docker stop 送 SIGTERM 落嚟就即刻退出 loop，唔使等成個 interval 完。
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = load_config()?;
    let report_url = format!(
        "{}/api/report",
        cfg.server_url.trim_end_matches('/')
    );
    info!(
        "agent starting id={} name={} url={} interval={}s",
        cfg.agent_id, cfg.agent_name, report_url, cfg.interval_secs
    );

    let (country, city) = fetch_geo().await;
    info!("geo: country={:?} city={:?}", country, city);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;

    let mut sys = System::new_all();
    let mut prev_rx = 0u64;
    let mut prev_tx = 0u64;

    // 第一次先刷新一次，讓速率有基準
    let _ = collect_metrics(&mut sys, &mut prev_rx, &mut prev_tx, &country, &city, &cfg);
    tokio::time::sleep(Duration::from_secs(2)).await;

    loop {
        let report = collect_metrics(&mut sys, &mut prev_rx, &mut prev_tx, &country, &city, &cfg);
        match send_report(&client, &report_url, &report).await {
            Ok(_) => info!(
                "reported cpu={:.1}% mem={:.1}/{:.1}GB",
                report.cpu, report.mem_used, report.mem_total
            ),
            Err(e) => error!("report failed: {:?}", e),
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(cfg.interval_secs)) => {}
            _ = shutdown_signal() => {
                info!("shutdown signal received, exiting");
                break;
            }
        }
    }

    Ok(())
}
