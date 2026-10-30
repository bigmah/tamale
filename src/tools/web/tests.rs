use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[test]
fn duckduckgo_results_decode_redirects_entities_and_skip_invalid_links() {
    let html = r#"<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2F%3Fa%3D1%26b%3D2">Rust &amp; tools</a><div class="result__snippet">Some <b>useful</b> text.</div></div>
        <div class="result"><a class="result__a" href="https://rust-lang.org/">Rust</a></div>
        <div class="result"><a class="result__a" href="javascript:alert(1)">Bad</a></div>"#;
    let results = parse_duckduckgo(html).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].url, "https://example.com/?a=1&b=2");
    assert_eq!(results[0].title, "Rust & tools");
    assert!(results[0].description.contains("useful"));
}

#[test]
fn duckduckgo_distinguishes_no_results_from_blocked_or_changed_pages() {
    assert!(
        parse_duckduckgo("<div class='no-results'>Nothing</div>")
            .unwrap()
            .is_empty()
    );
    assert!(parse_duckduckgo("<form id='challenge-form'>Solve captcha</form>").is_err());
    assert!(parse_duckduckgo("<html>Unexpected response</html>").is_err());
}

#[test]
fn brave_parses_results_and_missing_web_results() {
    let results = parse_brave(r#"{"web":{"results":[{"title":"Rust","url":"https://rust-lang.org/","description":"<b>Rust</b> tools"}]}}"#).unwrap();
    assert_eq!(results[0].url, "https://rust-lang.org/");
    assert_eq!(html_text(&results[0].description), "Rust tools");
    assert!(parse_brave("{}").unwrap().is_empty());
    assert!(parse_brave("invalid json").is_err());
}

#[test]
fn page_extracts_main_text_code_and_links_without_script_or_hidden_text() {
    let (title, text) = page_text(
        r#"<title>Guide &amp; API</title><body><nav>Navigation</nav><main>
        <h1>Guide</h1><script>bad script</script><style>bad style</style>
        <p hidden>hidden text</p><p aria-hidden="true">also hidden</p>
        <p>Inline <strong>bold</strong> text.</p><pre><span>let</span> x = 1;
  print(x);</pre><a href="/next">Next page</a><a href="javascript:x">Invalid</a>
        </main></body>"#,
        &Url::parse("https://example.com/docs/start").unwrap(),
    );
    assert_eq!(title, "Guide & API");
    for excluded in [
        "Navigation",
        "bad script",
        "bad style",
        "hidden text",
        "also hidden",
    ] {
        assert!(!text.contains(excluded), "{text}");
    }
    assert!(
        text.contains("let x = 1;\n  print(x);") && text.contains("https://example.com/next"),
        "{text}"
    );
    assert!(!text.contains("javascript:"));
    assert!(text.contains("Inline bold text."), "{text}");
}

#[test]
fn page_paginates_and_handles_out_of_range_and_long_unicode_lines() {
    let url = Url::parse("https://example.com").unwrap();
    let out = format_page(&url, "Title", "first\nsecond\nthird", Some(2), Some(1));
    assert!(
        out.contains("     2\tsecond") && out.contains("offset 3"),
        "{out}"
    );
    assert!(!out.contains("\tfirst"));
    assert!(format_page(&url, "", "one", Some(10), None).contains("past the end"));
    let out = format_page(&url, "", &"é".repeat(MAX_OUTPUT_CHARS), None, None);
    assert!(out.contains("bytes) omitted; full output: "), "{out}");
}

#[tokio::test]
async fn validates_queries_and_urls_before_network_requests() {
    assert!(search(json!({"query": "  "})).await.is_err());
    assert!(search(json!({"query": "a".repeat(601)})).await.is_err());
    for url in [
        "file:///etc/passwd",
        "ftp://example.com",
        "https://user:secret@example.com",
        "not a url",
    ] {
        assert!(fetch(json!({"url": url})).await.is_err());
    }
}

struct Server {
    base: String,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0; 4096];
                let size = stream.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..size]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let response = match path {
                    "/redirect" => "HTTP/1.1 302 Found\r\nLocation: /page\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    "/page" => "HTTP/1.0 200 OK\r\nContent-Type: text/html\r\n\r\n<title>Test page</title><main><p>Readable text</p></main>".into(),
                    "/plain" => "HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nfirst\nsecond\nthird".into(),
                    "/pdf" => "HTTP/1.0 200 OK\r\nContent-Type: application/pdf\r\n\r\n%PDF".into(),
                    "/large" => format!("HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1),
                    "/unbounded" => format!("HTTP/1.0 200 OK\r\n\r\n{}", "a".repeat(MAX_BODY_BYTES + 1)),
                    _ => "HTTP/1.0 404 Not Found\r\n\r\nMissing".into(),
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self { base, task }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn fetch_follows_redirects_reads_text_and_reports_http_and_size_errors() {
    let server = Server::new().await;
    let out = crate::tools::execute(
        "web_fetch",
        json!({"url": format!("{}/redirect", server.base), "description": "Read test page"}),
    )
    .await
    .unwrap();
    assert!(
        out.contains("Readable text") && out.contains("Title: Test page"),
        "{out}"
    );
    assert!(out.contains(&format!("URL: {}/page", server.base)));
    let out = fetch(json!({"url": format!("{}/plain", server.base), "offset": 2, "limit": 1}))
        .await
        .unwrap();
    assert!(out.contains("     2\tsecond") && out.contains("offset 3"));
    for (path, expected) in [
        ("missing", "404"),
        ("pdf", "unsupported content type"),
        ("large", "2 MiB"),
        ("unbounded", "2 MiB"),
    ] {
        let error = fetch(json!({"url": format!("{}/{path}", server.base)}))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(expected), "{path}: {error:#}");
    }
}

#[tokio::test]
#[ignore = "contacts DuckDuckGo; run explicitly for a live provider smoke test"]
async fn live_duckduckgo_search() {
    let response = client(0)
        .unwrap()
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", "rust programming language official")])
        .send()
        .await
        .unwrap();
    let results = parse_duckduckgo(&body(response).await.unwrap()).unwrap();
    assert!(!results.is_empty());
}

#[tokio::test]
#[ignore = "contacts example.com; run explicitly for a live fetch smoke test"]
async fn live_fetch() {
    let out = fetch(json!({"url": "https://example.com"})).await.unwrap();
    assert!(out.contains("Example Domain"), "{out}");
}
