mod api;
mod db;
mod notify;

use api::{
    admin_get_notify, admin_rename, admin_reset_traffic, admin_save_notify, admin_test_notify,
    admin_update_traffic_limit, list_servers, report, AppState,
};
use axum::{
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

    let app = Router::new()
        .route("/api/report", post(report))
        .route("/api/servers", get(list_servers))
        .route("/api/admin/reset-traffic", post(admin_reset_traffic))
        .route("/api/admin/rename", post(admin_rename))
        .route("/api/admin/update-traffic-limit", post(admin_update_traffic_limit))
        .route("/api/admin/notify", get(admin_get_notify).post(admin_save_notify))
        .route("/api/admin/test-notify", post(admin_test_notify))
        .nest_service("/", ServeDir::new("static"))
        .with_state(state);

    let addr = "0.0.0.0:8080";
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
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
