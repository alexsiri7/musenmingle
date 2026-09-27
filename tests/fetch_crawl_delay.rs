//! The robots.txt fetch counts as a request, so the first page request after
//! it already waits the site's Crawl-delay.

use std::time::{Duration, Instant};

use musenmingle::config::RateLimitConfig;
use musenmingle::fetch::FetchContext;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn first_page_request_waits_the_crawl_delay_after_robots_txt() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/robots.txt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("User-agent: *\nCrawl-delay: 1\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/whats-on"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let ctx = FetchContext::new(RateLimitConfig::disabled()).unwrap();
    let start = Instant::now();
    let body = ctx
        .get_text(&Url::parse(&format!("{}/whats-on", server.uri())).unwrap())
        .await
        .unwrap();
    assert_eq!(body, "ok");
    assert!(
        start.elapsed() >= Duration::from_secs(1),
        "page fetched {:?} after robots.txt",
        start.elapsed()
    );
}
