mod api;
mod db;
mod notify;

use api::{
    admin_get_notify, admin_rename, admin_reset_traffic, admin_save_notify, admin_test_notify,
    admin_update_traffic_limit, list_servers, report, require_admin, AppState,
};
use axum::{
    http::{header, HeaderValue},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use axum::extract::Request;
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
    let report_secret = env::var("REPORT_SECRET").expect("REPORT_SECRET must be set");
    let admin_secret = env::var("ADMIN_SECRET").expect("ADMIN_SECRET must be set");

    std::fs::create_dir_all("data")?;

    let pool = db::init_db(&database_url).await?;

    let state = Arc::new(AppState {
        pool: pool.clone(),
        report_secret,
        admin_secret,
    });

    let pool_bg = pool.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Ok(settings) = notify::load_settings(&pool_bg).await {
                notify::check_offline(&pool_bg, &settings).await;
            }
        }
    });

    // /api/admin/* 全部掛在 require_admin 中介層後面，統一用
    // Authorization: Bearer <ADMIN_SECRET> 驗證，取代舊版「每支 handler
    // 各自檢查一次、GET 完全不檢查」的作法。
    let admin = Router::new()
        .route("/reset-traffic", post(admin_reset_traffic))
        .route("/rename", post(admin_rename))
        .route("/update-traffic-limit", post(admin_update_traffic_limit))
        .route("/notify", get(admin_get_notify).post(admin_save_notify))
        .route("/test-notify", post(admin_test_notify))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin,
        ));

    let app = Router::new()
        .route("/api/report", post(report))
        .route("/api/servers", get(list_servers))
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
