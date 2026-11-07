//! #401: a failed fetch logs its status, content type and the start of its
//! body, so the next flap shows what the host answered; never a credential.
//!
//! Its own test binary: `tracing` caches each callsite's interest globally,
//! so lib tests hitting the same callsite on other threads would race the
//! capturing subscriber.

use std::sync::{Arc, Mutex};

use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use url::Url;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

async fn serving(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn failed_fetches_are_logged_with_status_content_type_and_body() {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let ctx = FetchContext::new_allowing_loopback(RateLimitConfig::disabled()).unwrap();
    for template in [
        ResponseTemplate::new(202)
            .set_body_raw("<p>Checking your browser. key=SECRETKEY1</p>", "text/html"),
        ResponseTemplate::new(200)
            .set_body_raw("<!DOCTYPE html><title>Just a moment</title>", "text/html"),
    ] {
        let server = serving(template).await;
        let url = Url::parse(&format!("{}/api?key=SECRETKEY1&page=1", server.uri())).unwrap();
        ctx.get_json::<serde_json::Value>(&url).await.unwrap_err();
    }

    let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("fetch failed")).collect();
    assert_eq!(lines.len(), 2, "{log}");
    for (line, status, body) in [
        (
            lines[0],
            "status=202 Accepted",
            "body=<p>Checking your browser. key=[redacted]</p>",
        ),
        (
            lines[1],
            "status=200 OK",
            "body=<!DOCTYPE html><title>Just a moment</title>",
        ),
    ] {
        assert!(line.contains(status), "{line}");
        assert!(line.contains(r#"content_type="text/html""#), "{line}");
        assert!(line.contains(body), "{line}");
        assert!(!line.contains("SECRETKEY1"), "{line}");
    }
}
