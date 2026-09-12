//! 管理员凭据的 **env 分支** 回归测试。
//!
//! 单独一个测试文件 = 单独一个进程：这里会设置进程级环境变量，跟别的测试跑在
//! 同一进程里会互相干扰（`set_var` 是全局的），所以刻意隔离。
//!
//! 背景：曾有一个只在「设了 ADMIN_PASSWORD」时才复现的登录 bug ——
//! `admin_credentials` 把 env 明文密码塞进了「哈希」槽位，而校验方以「非空即
//! 哈希」去解析，必然失败。测试环境不设 env，所以当时全绿、生产恒 401。

use axum::body::Body;
use http::{header, Method, Request, StatusCode};

const USER: &str = "env-admin-user";
const PASS: &str = "env-admin-pass-!@#";

async fn login_status(app: axum::Router, user: &str, pass: &str) -> StatusCode {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({ "username": user, "password": pass }).to_string(),
        ))
        .unwrap();
    llmux_server::test_request(app, req).await.status()
}

#[tokio::test]
async fn env_credentials_take_effect_and_override_the_default() {
    std::env::set_var("ADMIN_USERNAME", USER);
    std::env::set_var("ADMIN_PASSWORD", PASS);

    let state = llmux_server::test_state().await;
    let app = llmux_server::app(state.clone());

    // env 里的凭据必须能登录 —— 这正是生产用 .env 注入密码的路径。
    assert_eq!(
        login_status(app.clone(), USER, PASS).await,
        StatusCode::OK,
        "env 中配置的凭据应能登录（生产用的就是这条路径）"
    );

    // env 生效时默认值必须失效，否则等于留了个后门。
    assert_eq!(
        login_status(app.clone(), "admin", "admin").await,
        StatusCode::UNAUTHORIZED,
        "设了 env 时 admin/admin 不应可用"
    );

    // 用户名对、密码错 → 拒绝
    assert_eq!(
        login_status(app.clone(), USER, "wrong").await,
        StatusCode::UNAUTHORIZED
    );
    // 密码对、用户名错 → 拒绝
    assert_eq!(
        login_status(app.clone(), "someone-else", PASS).await,
        StatusCode::UNAUTHORIZED
    );

    std::env::remove_var("ADMIN_USERNAME");
    std::env::remove_var("ADMIN_PASSWORD");
}
