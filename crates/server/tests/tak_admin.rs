//! Integration tests of TAK Server's administration pages against the real stack: Caddy
//! serves them on TAK_ADMIN_PORT to the platform's administrators only, through their
//! TeamTAK session, and reaches TAK Server with a certificate of its own.
//!
//! Like the other suites they need the stack in local mode (`./install.sh --local`) and are
//! ignored by a plain `cargo test`; run them with `scripts/test-integration.sh`.

mod common;

use common::*;
use serde_json::Value;

/// Address of the administration pages on this machine; the test is skipped when they are
/// turned off.
fn admin_pages() -> Option<String> {
    let port = env().get("TAK_ADMIN_PORT").map(String::as_str).unwrap_or("off");
    if port == "off" {
        eprintln!("TAK_ADMIN_PORT=off: nothing to test");
        return None;
    }
    Some(format!("http://127.0.0.1:{port}"))
}

#[tokio::test]
#[ignore = "needs the running stack"]
async fn only_administrators_reach_tak_servers_administration_pages() {
    let Some(pages) = admin_pages() else { return };
    let web = setting("PUBLIC_URL").trim_end_matches('/').to_string();
    let version = format!("{pages}/Marti/api/version");

    // Nobody signed in, and an account that is not an administrator: sent to the web UI,
    // nothing of TAK Server shown.
    let member = Session::new_account("takadmin").await;
    for (who, session) in [("nobody", Session::anonymous()), ("a member", member)] {
        let (status, location, body) = session.fetch(&version).await;
        assert_eq!((status, location.as_str()), (303, format!("{web}/").as_str()), "{who}: {body}");
        assert!(!body.contains("RELEASE"), "{who} saw TAK Server: {body}");
        assert_eq!(session.get("/me").await.body["tak_admin_url"], Value::Null, "{who} is not told where the pages are");
    }

    // An administrator: TAK Server answers, as to an administrator, and its redirects keep
    // the address the browser used.
    let admin = Session::admin().await;
    let (status, _, body) = admin.fetch(&version).await;
    assert_eq!(status, 200, "{body}");
    assert!(body.starts_with(setting("TAK_VERSION")), "{body}");
    let (status, _, body) = admin.fetch(&format!("{pages}/Marti/api/util/isAdmin")).await;
    assert_eq!((status, body.trim()), (200, "true"));
    let (status, location, _) = admin.fetch(&format!("{pages}/Marti/")).await;
    assert_eq!((status, location.as_str()), (302, format!("{pages}/index.html").as_str()));
    let link = admin.get("/me").await.ok()["tak_admin_url"].as_str().unwrap().to_string();
    let port = pages.rsplit(':').next().unwrap();
    assert!(link.ends_with(&format!(":{port}")), "{link}");

    // One entry in the audit log for all these requests.
    let log = admin.get(&format!("/admin/audit?action=tak_admin_opened&actor={}", admin.email)).await.ok();
    assert_eq!(log["entries"].as_array().unwrap().len(), 1, "{log}");
}
