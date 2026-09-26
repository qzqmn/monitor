mod agents;
mod api;
mod auth;
mod db;
mod notify;

use agents::{create_agent, delete_agent, list_agents, rotate_agent_token};
use api::{
    admin_get_notify, admin_rename, admin_reset_traffic, admin_save_notify, admin_test_notify,
    admin_update_traffic_limit, list_servers, report, AppState,
};
use auth::{change_password, login, logout, require_admin, setup, setup_status};
use axum::{
    extract::Request,
    http::{header, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use std::{env, sync::Arc, time::Duration};
use tower_http::services::ServeDir;
use tracing_subscriber;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite:data/monitor.db".to_string());
    // 帳密＋每台機器專屬 token 都改存在資料庫裡，首次開後台網頁自己設定，
    // 伺服器啟動不再需要 REPORT_SECRET / ADMIN_SECRET 這兩個環境變數了。
    // COOKIE_SECURE 預設關閉，因為預設是走 Tailscale/內網 plain HTTP，
    // 開了瀏覽器會直接不送這顆 Cookie、永遠登不進去；架了 HTTPS 反代才開。
    let cookie_secure = env::var("COOKIE_SECURE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    std::fs::create_dir_all("data")?;

    let pool = db::init_db(&database_url).await?;

    let state = Arc::new(AppState {
        pool: pool.clone(),
        cookie_secure,
    });

    let pool_bg = pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Ok(settings) = notify::load_settings(&pool_bg).await {
                notify::check_offline(&pool_bg, &settings).await;
            }
            // 每月自動歸零流量：用「今天 >= 這個月的重置日」判斷，60 秒跑一次
            // 成本可以忽略，但能保證重置日當天離線也會在恢復後很快補跑。
            match db::auto_reset_due_traffic(&pool_bg).await {
                Ok(reset) => {
                    for (id, name) in reset {
                        tracing::info!("流量已自動歸零: {} ({})", name, id);
                    }
                }
                Err(e) => tracing::error!("auto reset traffic error: {:?}", e),
            }
        }
    });

    // /api/admin/* 全部掛在 require_admin 中介層後面：改成看登入時發的
    // session Cookie，不再是舊版「Authorization: Bearer <ADMIN_SECRET>」。
    let admin = Router::new()
        .route("/reset-traffic", post(admin_reset_traffic))
        .route("/rename", post(admin_rename))
        .route("/update-traffic-limit", post(admin_update_traffic_limit))
        .route("/notify", get(admin_get_notify).post(admin_save_notify))
        .route("/test-notify", post(admin_test_notify))
        .route("/change-password", post(change_password))
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/rotate", post(rotate_agent_token))
        .route("/agents/delete", post(delete_agent))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin,
        ));

    let app = Router::new()
        .route("/api/report", post(report))
        .route("/api/servers", get(list_servers))
        .route("/api/setup-status", get(setup_status))
        .route("/api/setup", post(setup))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .nest("/api/admin", admin)
        .nest_service("/", ServeDir::new("static"))
        .layer(middleware::from_fn(security_headers))
        .with_state(state);

    let addr = "0.0.0.0:8080";
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

// 基本安全 header，屬於多層防護：就算前端某處漏轉義，這些也能擋掉一部分
// 攻擊面（禁止被嵌 iframe、禁止瀏覽器自作聰明猜 MIME type、限制腳本來源）。
async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline' https://cdn.tailwindcss.com; \
             style-src 'self' 'unsafe-inline' https://cdnjs.cloudflare.com; \
             font-src https://cdnjs.cloudflare.com; img-src 'self' data:; connect-src 'self'",
        ),
    );
    res
}

// 監聽 SIGTERM / Ctrl+C，收到就完成呢個 future，等 axum 做優雅關閉：
// 停止接受新連線、等緊做嘅 request 做完先退出，唔使等 docker 嘅 10 秒 grace period 完先俾 SIGKILL 強殺。
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
