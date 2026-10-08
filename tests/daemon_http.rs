use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use baleyg::{
    daemon::{
        BrowserProvisioner,
        registry::{BrowserOptions, CheckoutOptions, CheckoutRegistry},
    },
    store::topology::WorkspaceIdentity,
};
use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use tower::ServiceExt;

fn checkout(root: &Path) -> WorkspaceIdentity {
    fs::create_dir_all(root.join(".git")).unwrap();
    WorkspaceIdentity::discover(Some(root), root)
        .unwrap()
        .attach_marker()
        .unwrap()
}
fn options() -> CheckoutOptions {
    CheckoutOptions(serde_json::to_value(BrowserOptions::default()).unwrap())
}
async fn response(
    router: axum::Router,
    address: std::net::SocketAddr,
    route: &str,
    token: Option<&str>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .uri(route)
        .header("host", address.to_string());
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn socket_first_then_serve_and_serve_first_then_mcp_are_inert() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let a = checkout(&root.join("a"));
    let b = checkout(&root.join("b"));
    for socket_first in [true, false] {
        let registry = Arc::new(Mutex::new(CheckoutRegistry::new()));
        let mut browser = BrowserProvisioner::new();
        assert!(browser.address().is_none());
        let token_file = root.join(format!("token-{socket_first}"));
        assert!(!token_file.exists());
        if socket_first {
            registry.lock().await.attach_launch(1, &a).unwrap();
        }
        assert!(
            browser
                .register_serve(
                    &registry,
                    &a,
                    options(),
                    "127.0.0.1:0".parse().unwrap(),
                    &token_file
                )
                .await
                .is_ok()
        );
        let address = browser.address().unwrap();
        assert_ne!(address.port(), 0);
        if !socket_first {
            registry.lock().await.attach_launch(1, &a).unwrap();
        }
        let registered = registry.lock().await;
        assert_eq!(registered.active_count(), 1);
        assert!(registered.runtime(&a.root_key).is_none());
        drop(registered);
        let token = fs::read_to_string(&token_file).unwrap();
        let router = browser.router().unwrap();
        assert_eq!(
            response(router.clone(), address, "/api/checkouts", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            response(router.clone(), address, "/api/checkouts", Some(&token))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            response(router.clone(), address, "/api/daemon/status", Some(&token))
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            response(router.clone(), address, "/api/status", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            response(router.clone(), address, "/api/status", Some(&token))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            response(router.clone(), address, "/healthz", None)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            response(router.clone(), address, "/", None).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            response(router.clone(), address, "/app.js", None)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(registry.lock().await.active_count(), 1);
        // Exact repeat is idempotent, including requested port zero.
        assert_eq!(
            browser
                .register_serve(
                    &registry,
                    &a,
                    options(),
                    "127.0.0.1:0".parse().unwrap(),
                    &token_file
                )
                .await
                .unwrap(),
            address
        );
        assert!(
            browser
                .register_serve(
                    &registry,
                    &b,
                    options(),
                    "0.0.0.0:0".parse().unwrap(),
                    &token_file
                )
                .await
                .is_err()
        );
        assert!(
            browser
                .register_serve(
                    &registry,
                    &b,
                    options(),
                    "127.0.0.1:1".parse().unwrap(),
                    &token_file
                )
                .await
                .is_err()
        );
        assert!(registry.lock().await.registration(&b.root_key).is_none());
        let alternate = root.join(format!("alternate-{socket_first}"));
        assert!(
            browser
                .register_serve(
                    &registry,
                    &b,
                    options(),
                    "127.0.0.1:0".parse().unwrap(),
                    &alternate
                )
                .await
                .is_err()
        );
        assert!(!alternate.exists());
        fs::write(&token_file, "0".repeat(64)).unwrap();
        assert!(
            browser
                .register_serve(
                    &registry,
                    &b,
                    options(),
                    "127.0.0.1:0".parse().unwrap(),
                    &token_file
                )
                .await
                .is_err()
        );
        assert!(registry.lock().await.registration(&b.root_key).is_none());
        fs::write(&token_file, &token).unwrap();
        assert_eq!(
            browser
                .register_serve(
                    &registry,
                    &b,
                    options(),
                    "127.0.0.1:0".parse().unwrap(),
                    &token_file
                )
                .await
                .unwrap(),
            address
        );
        assert_eq!(registry.lock().await.active_count(), 1);
        registry.lock().await.disconnect(1);
        assert_eq!(registry.lock().await.active_count(), 1); // release has its own timer
    }
}

#[tokio::test]
async fn listener_and_control_do_not_postpone_idle_exit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let checkout = checkout(&root.join("checkout"));
    let registry = Arc::new(Mutex::new(CheckoutRegistry::starting_at(
        Instant::now() - Duration::from_secs(31 * 60),
    )));
    let mut browser = BrowserProvisioner::new();
    let address = browser
        .register_serve(
            &registry,
            &checkout,
            options(),
            "127.0.0.1:0".parse().unwrap(),
            &root.join("token"),
        )
        .await
        .unwrap();
    assert_eq!(registry.lock().await.active_count(), 0);
    let (_control, _daemon_control) = std::os::unix::net::UnixStream::pair().unwrap();
    browser.serve_until_idle(registry).await.unwrap();
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn conflicting_checkout_settings_and_token_inode_leave_other_entries_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let first = checkout(&root.join("first"));
    let second = checkout(&root.join("second"));
    let token_file = root.join("token");
    let registry = Arc::new(Mutex::new(CheckoutRegistry::new()));
    let mut browser = BrowserProvisioner::new();
    let original = options();
    browser
        .register_serve(
            &registry,
            &first,
            original.clone(),
            "127.0.0.1:0".parse().unwrap(),
            &token_file,
        )
        .await
        .unwrap();
    let address = browser.address();
    let different = CheckoutOptions(
        serde_json::to_value(BrowserOptions {
            max_file_bytes: 4096,
            ..BrowserOptions::default()
        })
        .unwrap(),
    );
    registry.lock().await.attach_launch(7, &first).unwrap();
    assert!(
        browser
            .register_serve(
                &registry,
                &first,
                different.clone(),
                "127.0.0.1:0".parse().unwrap(),
                &token_file
            )
            .await
            .is_err()
    );
    assert_eq!(
        registry.lock().await.registration(&first.root_key),
        Some(&original)
    );
    assert!(
        registry
            .lock()
            .await
            .registration(&second.root_key)
            .is_none()
    );
    assert_eq!(browser.address(), address);
    registry.lock().await.disconnect(7);
    assert!(registry.lock().await.release(&first.root_key).unwrap());
    assert!(
        browser
            .register_serve(
                &registry,
                &first,
                different.clone(),
                "127.0.0.1:0".parse().unwrap(),
                &token_file
            )
            .await
            .is_ok()
    );
    browser
        .register_serve(
            &registry,
            &second,
            original,
            "127.0.0.1:0".parse().unwrap(),
            &token_file,
        )
        .await
        .unwrap();
    let saved = registry
        .lock()
        .await
        .registration(&second.root_key)
        .cloned();
    fs::rename(&token_file, root.join("old-token")).unwrap();
    fs::write(&token_file, fs::read(root.join("old-token")).unwrap()).unwrap();
    assert!(
        browser
            .register_serve(
                &registry,
                &second,
                different,
                "127.0.0.1:0".parse().unwrap(),
                &token_file
            )
            .await
            .is_err()
    );
    assert_eq!(
        registry
            .lock()
            .await
            .registration(&second.root_key)
            .cloned(),
        saved
    );
    assert_eq!(browser.address(), address);
}

#[tokio::test]
async fn later_serve_compares_configuration_while_http_is_running() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let first = checkout(&root.join("first"));
    let second = checkout(&root.join("second"));
    let registry = Arc::new(Mutex::new(CheckoutRegistry::new()));
    registry.lock().await.attach_launch(1, &first).unwrap();
    let mut browser = BrowserProvisioner::new();
    let token_file = root.join("token");
    let address = browser
        .register_serve(
            &registry,
            &first,
            options(),
            "127.0.0.1:0".parse().unwrap(),
            &token_file,
        )
        .await
        .unwrap();
    let serving = browser.spawn_until_idle(registry.clone()).unwrap();
    assert_eq!(browser.address(), Some(address));
    assert_eq!(
        browser
            .register_serve(
                &registry,
                &second,
                options(),
                "127.0.0.1:0".parse().unwrap(),
                &token_file
            )
            .await
            .unwrap(),
        address
    );
    assert!(
        browser
            .register_serve(
                &registry,
                &second,
                options(),
                "127.0.0.1:1".parse().unwrap(),
                &token_file
            )
            .await
            .is_err()
    );
    assert_eq!(registry.lock().await.active_count(), 1);
    serving.abort();
    let _ = serving.await;
}
