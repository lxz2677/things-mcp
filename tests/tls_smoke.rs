//! Network smoke test for the TLS stack.
//!
//! Ignored by default (needs internet); run with:
//! `cargo test --test tls_smoke -- --ignored`

use things_mcp::things::{HttpTransport, RawRequest, Transport};

#[tokio::test]
#[ignore = "requires network access"]
async fn rustls_stack_reaches_https() {
    let transport = HttpTransport::new().expect("build transport");
    let reply = transport
        .execute(RawRequest {
            method: "GET".to_string(),
            url: "https://example.com/".to_string(),
            headers: vec![("accept".to_string(), "text/html".to_string())],
            body: None,
        })
        .await
        .expect("TLS handshake");

    assert!(
        (200..400).contains(&reply.status),
        "unexpected HTTP status {}",
        reply.status
    );
}
