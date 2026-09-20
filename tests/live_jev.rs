//! Public API tests. No test invokes the production transport.
use baleyg::live_jev::LiveJev;
#[test]
fn public_budget_is_serializable_and_provider_is_send_sync() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<LiveJev>();
    let root = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let ledger = root.path().join("budget");
    let provider = LiveJev::open(
        &ledger,
        "synthetic-not-a-provider-key".into(),
        500,
        work.path(),
    )
    .unwrap();
    let status = serde_json::to_value(provider.budget().unwrap()).unwrap();
    assert_eq!(
        status,
        serde_json::json!({"capCents":500,"reservedCents":0,"remainingCents":500,"attempts":0})
    );
    assert!(LiveJev::open(&ledger, "synthetic".into(), 501, work.path()).is_err());
    assert!(LiveJev::open(&ledger, "synthetic".into(), 400, work.path()).is_err());
}
