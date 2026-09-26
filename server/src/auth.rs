//! 帳密登入取代舊版單一 ADMIN_SECRET：
//! - 第一次開啟後台會走「建立管理員帳號」，之後永久鎖住不能再註冊第二個。
//! - 登入用 argon2 驗證密碼雜湊，成功後發一個存在資料庫裡的 session，
//!   用 HttpOnly + SameSite=Strict 的 Cookie 帶著跑，不再是每次 API
//!   呼叫都手動帶 Authorization header。
//! - 因為現在只有單一帳號，攻擊者不用像多用戶系統一樣先找帳號、只需要
//!   暴力猜密碼，所以加一個簡單的失敗次數鎖定，擋掉線上暴力破解。

use crate::api::AppState;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use time::Duration as CookieDuration;

pub const SESSION_COOKIE: &str = "session";

fn hash_password(password: &str) -> Result<String, ()> {
    let salt = SaltString::generate(&mut rand_core::OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| ())
}

fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

// ---------------------------------------------------------------------
// 登入失敗鎖定：只有單一帳號，簡單用一個全域計數器就夠，重啟服務會重置
// 也沒關係（不是防長期針對性攻擊，只是擋掉最基本的線上暴力猜密碼）。
// ---------------------------------------------------------------------
struct LoginGuardState {
    failures: u32,
    locked_until: Option<Instant>,
}
static LOGIN_GUARD: OnceLock<Mutex<LoginGuardState>> = OnceLock::new();
const MAX_FAILURES: u32 = 5;
const LOCKOUT: Duration = Duration::from_secs(5 * 60);

fn login_guard() -> &'static Mutex<LoginGuardState> {
    LOGIN_GUARD.get_or_init(|| {
        Mutex::new(LoginGuardState {
            failures: 0,
            locked_until: None,
        })
    })
}

/// 還在鎖定中就回傳剩餘秒數
fn check_locked() -> Option<u64> {
    let g = login_guard().lock().unwrap();
    match g.locked_until {
        Some(t) if t > Instant::now() => Some((t - Instant::now()).as_secs()),
        _ => None,
    }
}

fn record_failure() {
    let mut g = login_guard().lock().unwrap();
    g.failures += 1;
    if g.failures >= MAX_FAILURES {
        g.locked_until = Some(Instant::now() + LOCKOUT);
        g.failures = 0;
    }
}

fn record_success() {
    let mut g = login_guard().lock().unwrap();
    g.failures = 0;
    g.locked_until = None;
}

// ---------------------------------------------------------------------
// require_admin 中介層：改成看 Cookie 裡的 session，不再比對 Bearer header
// ---------------------------------------------------------------------
pub async fn require_admin(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(cookie) = jar.get(SESSION_COOKIE) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    match crate::db::session_valid(&state.pool, cookie.value()).await {
        Ok(true) => Ok(next.run(req).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

fn session_cookie(token: String, secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(secure)
        .path("/")
        .max_age(CookieDuration::days(30))
        .build()
}

#[derive(Serialize)]
pub struct SetupStatus {
    needs_setup: bool,
}

pub async fn setup_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match crate::db::admin_exists(&state.pool).await {
        Ok(exists) => Json(SetupStatus {
            needs_setup: !exists,
        })
        .into_response(),
        Err(e) => {
            tracing::error!("setup_status db error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}

#[derive(Deserialize)]
pub struct Credentials {
    username: String,
    password: String,
}

/// 首次設定管理員帳號。一旦 admin_account 已經有資料就永遠拒絕，
/// 避免有心人在你設定好之後又跑一次把帳號改成他的。
pub async fn setup(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Json(payload): Json<Credentials>,
) -> impl IntoResponse {
    match crate::db::admin_exists(&state.pool).await {
        Ok(true) => return (StatusCode::FORBIDDEN, "管理員帳號已經建立過了").into_response(),
        Err(e) => {
            tracing::error!("setup check error: {:?}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
        Ok(false) => {}
    }

    let username = payload.username.trim();
    if username.is_empty() || payload.password.len() < 8 {
        return (
            StatusCode::BAD_REQUEST,
            "帳號不可為空，密碼至少 8 個字元",
        )
            .into_response();
    }

    let Ok(hash) = hash_password(&payload.password) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "hash error").into_response();
    };

    if let Err(e) = crate::db::create_admin(&state.pool, username, &hash).await {
        tracing::error!("create_admin error: {:?}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
    }

    match crate::db::create_session(&state.pool).await {
        Ok(token) => {
            let jar = jar.add(session_cookie(token, state.cookie_secure));
            (jar, (StatusCode::OK, "帳號建立成功")).into_response()
        }
        Err(e) => {
            tracing::error!("create_session error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response()
        }
    }
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Json(payload): Json<Credentials>,
) -> impl IntoResponse {
    if let Some(secs) = check_locked() {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            format!("登入失敗次數過多，請 {secs} 秒後再試"),
        )
            .into_response();
    }

    let admin = match crate::db::get_admin(&state.pool).await {
        Ok(Some(a)) => a,
        Ok(None) => return (StatusCode::FORBIDDEN, "尚未建立管理員帳號").into_response(),
        Err(e) => {
            tracing::error!("get_admin error: {:?}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    };

    let (username, password_hash) = admin;
    // 用常數時間比較帳號名稱，避免帳號名稱本身也被拿來做時間側錄猜測
    // （雖然只有一個帳號、意義有限，但反正成本是 0，順手做好）。
    use subtle::ConstantTimeEq;
    let username_ok = bool::from(payload.username.as_bytes().ct_eq(username.as_bytes()));

    if !username_ok || !verify_password(&payload.password, &password_hash) {
        record_failure();
        return (StatusCode::UNAUTHORIZED, "帳號或密碼錯誤").into_response();
    }

    record_success();
    match crate::db::create_session(&state.pool).await {
        Ok(token) => {
            let jar = jar.add(session_cookie(token, state.cookie_secure));
            (jar, (StatusCode::OK, "登入成功")).into_response()
        }
        Err(e) => {
            tracing::error!("create_session error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "session error").into_response()
        }
    }
}

pub async fn logout(State(state): State<Arc<AppState>>, jar: CookieJar) -> impl IntoResponse {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        let _ = crate::db::delete_session(&state.pool, cookie.value()).await;
    }
    let jar = jar.remove(Cookie::from(SESSION_COOKIE));
    (jar, (StatusCode::OK, "已登出"))
}

#[derive(Deserialize)]
pub struct ChangePassword {
    current_password: String,
    new_password: String,
}

/// 改密碼要求重新輸入目前密碼，不能只靠「已經登入」這件事就放行——
/// 萬一瀏覽器沒關就被別人碰到，至少不能直接改密碼把你踢出去。
pub async fn change_password(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ChangePassword>,
) -> impl IntoResponse {
    let admin = match crate::db::get_admin(&state.pool).await {
        Ok(Some(a)) => a,
        Ok(None) => return (StatusCode::FORBIDDEN, "尚未建立管理員帳號").into_response(),
        Err(e) => {
            tracing::error!("get_admin error: {:?}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response();
        }
    };

    if !verify_password(&payload.current_password, &admin.1) {
        return (StatusCode::UNAUTHORIZED, "目前密碼不正確").into_response();
    }
    if payload.new_password.len() < 8 {
        return (StatusCode::BAD_REQUEST, "新密碼至少 8 個字元").into_response();
    }

    let Ok(hash) = hash_password(&payload.new_password) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "hash error").into_response();
    };
    match crate::db::update_admin_password(&state.pool, &hash).await {
        Ok(_) => (StatusCode::OK, "密碼已更新").into_response(),
        Err(e) => {
            tracing::error!("update_admin_password error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "db error").into_response()
        }
    }
}
