use super::*;

#[test]
fn only_the_hubs_page_may_frame_a_session() {
    let hub = "https://hub.example";
    assert_eq!(
        framed_by_hub("default-src 'self'; frame-ancestors 'none'", hub),
        "default-src 'self'; frame-ancestors https://hub.example"
    );
    assert_eq!(
        framed_by_hub("sandbox allow-scripts; frame-ancestors 'self'", hub),
        "sandbox allow-scripts; frame-ancestors 'self' https://hub.example"
    );
    assert_eq!(
        framed_by_hub("default-src 'self'", hub),
        "default-src 'self'"
    );
}

#[test]
fn public_urls_need_https_unless_insecure() {
    let p = Public::parse("https://Tamale-Hub.com/", false).unwrap();
    assert_eq!(
        (p.origin().as_str(), p.secure),
        ("https://tamale-hub.com", true)
    );
    assert_eq!(p.session_origin("ab"), "https://ab.tamale-hub.com");
    assert!(Public::parse("http://hub.localhost:8787", false).is_err());
    assert!(
        !Public::parse("http://hub.localhost:8787", true)
            .unwrap()
            .secure
    );
    assert!(Public::parse("https://x.com/path", false).is_err());
    assert_eq!(p.cookie_name("th"), "__Host-th");
    assert!(
        p.set_cookie("th", "v", 5)
            .contains("HttpOnly; SameSite=Strict; Path=/; Max-Age=5; Secure")
    );
}

#[test]
fn session_cookies_are_bound_to_their_sid_and_secret() {
    let sid = "a".repeat(32);
    let view = "f".repeat(64);
    let value = SessionCookie::make(b"secret", &sid, &view);
    assert_eq!(
        SessionCookie::check(b"secret", &sid, &value),
        Some(&view[..16])
    );
    assert_eq!(SessionCookie::check(b"other", &sid, &value), None);
    assert_eq!(
        SessionCookie::check(b"secret", &"b".repeat(32), &value),
        None
    );
    let forged = value.replace(&view[..16], &"0".repeat(16));
    assert_eq!(SessionCookie::check(b"secret", &sid, &forged), None);
}

#[test]
fn forwarded_for_counts_only_behind_a_local_proxy() {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", "9.9.9.9, 1.2.3.4".parse().unwrap());
    let local: SocketAddr = "127.0.0.1:5".parse().unwrap();
    let remote: SocketAddr = "5.6.7.8:5".parse().unwrap();
    assert_eq!(
        client_ip(local, &headers),
        "1.2.3.4".parse::<IpAddr>().unwrap()
    );
    assert_eq!(
        client_ip(remote, &headers),
        "5.6.7.8".parse::<IpAddr>().unwrap()
    );
}

#[test]
fn rate_limits_go_by_the_network_not_the_address() {
    let ip = |text: &str| text.parse::<IpAddr>().unwrap();
    // Anywhere in a /64 is one customer; the next /64 is another.
    assert_eq!(
        limit_bucket(ip("2001:db8:1:2:aaaa::1")),
        ip("2001:db8:1:2::")
    );
    assert_eq!(
        limit_bucket(ip("2001:db8:1:2:ffff:ffff:ffff:ffff")),
        ip("2001:db8:1:2::")
    );
    assert_ne!(limit_bucket(ip("2001:db8:1:3::1")), ip("2001:db8:1:2::"));
    // IPv4 is itself, however it arrives.
    assert_eq!(limit_bucket(ip("192.0.2.7")), ip("192.0.2.7"));
    assert_eq!(limit_bucket(ip("::ffff:192.0.2.7")), ip("192.0.2.7"));
}

#[test]
fn a_request_too_long_to_rewrite_isnt_routed_by_its_own_path() {
    let public = Public::parse("https://hub.example", false).unwrap();
    // The longest URI there is, whose rewritten form won't parse.
    let query = "a".repeat(65534 - "/s/x/ws?".len());
    let request = Request::builder()
        .uri(format!("/s/x/ws?{query}"))
        .header(header::HOST, "hub.example")
        .body(axum::body::Body::empty())
        .unwrap();
    assert_eq!(rewrite(&public, request).uri().path(), "/x/none");

    let request = Request::builder()
        .uri("/s/x/ws?a=1")
        .header(header::HOST, "hub.example")
        .body(axum::body::Body::empty())
        .unwrap();
    let rewritten = rewrite(&public, request);
    assert_eq!(rewritten.uri().path(), "/d/s/x/ws");
    assert_eq!(rewritten.uri().query(), Some("a=1"));
}

#[test]
fn only_https_hubs_ask_browsers_to_stay_on_https() {
    let header = |secure| {
        https_only(secure, StatusCode::OK.into_response())
            .headers()
            .get(header::STRICT_TRANSPORT_SECURITY)
            .cloned()
    };
    assert_eq!(header(false), None);
    assert_eq!(header(true).unwrap(), "max-age=31536000");
}

#[test]
fn a_backlog_refuses_what_wont_fit_and_gives_it_all_back() {
    let total = Arc::new(AtomicUsize::new(0));
    let one = Backlog::new(total.clone(), 100);
    let two = Backlog::new(total.clone(), 100);
    assert!(one.add(100));
    assert!(!one.add(1), "over this socket's limit");
    assert_eq!(
        total.load(Ordering::Relaxed),
        100,
        "what didn't fit isn't counted"
    );
    one.sent(50);
    assert!(one.add(50));
    assert!(two.add(100));

    // The hub's limit applies however many sockets it's spread over.
    let others: Vec<_> = (0..2).map(|_| Backlog::new(total.clone(), 100)).collect();
    assert!(others[0].add(100) && others[1].add(100));
    assert_eq!(total.load(Ordering::Relaxed), 100 * QUEUED_SOCKETS);
    let last = Backlog::new(total.clone(), 100);
    assert!(!last.add(1), "over the hub's limit");

    drop((one, two, others, last));
    assert_eq!(
        total.load(Ordering::Relaxed),
        0,
        "what was waiting goes with its socket"
    );
}

mod end_to_end {
    use super::super::*;
    use crate::state::{Key, Store, new_key};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use futures::{SinkExt, StreamExt};
    use serde_json::Value;
    use tamale_hub_protocol::ToHub;
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

    fn new_auth() -> String {
        let key = new_key().unwrap();
        protocol::auth_value(&protocol::decode_key(&key).unwrap())
    }

    struct Setup {
        port: u16,
        /// The auth value of the key `mine`, and of `other`.
        auth: String,
        other_auth: String,
        store: Store,
        hub: Arc<Hub>,
    }

    async fn start() -> Setup {
        start_with(MAX_QUEUED).await
    }

    /// With `max_queued` bytes allowed to wait for each socket.
    async fn start_with(max_queued: usize) -> Setup {
        let dir = std::env::temp_dir().join(format!(
            "tamale-hub-e2e-{}",
            state::hex(&state::random::<8>().unwrap())
        ));
        let store = Store::open(Some(dir)).unwrap();
        let (auth, other_auth) = (new_auth(), new_auth());
        store
            .edit(|s| {
                s.keys.push(Key::new("mine", &auth, false));
                s.keys.push(Key::new("other", &other_auth, false));
                Ok(())
            })
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let public = Public::parse(&format!("http://hub.localhost:{port}"), true).unwrap();
        let mut hub = Hub::new(
            public,
            Store {
                dir: store.dir.clone(),
            },
            Signup {
                open: true,
                max_keys: 4,
            },
        )
        .unwrap();
        Arc::get_mut(&mut hub).unwrap().max_queued = max_queued;
        tokio::spawn(run(hub.clone(), listener));
        Setup {
            port,
            auth,
            other_auth,
            store,
            hub,
        }
    }

    type Ws = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn ws(
        port: u16,
        path: &str,
        headers: &[(&str, String)],
    ) -> Result<Ws, tungstenite::Error> {
        let mut request = format!("ws://127.0.0.1:{port}{path}")
            .into_client_request()
            .unwrap();
        for (k, v) in headers {
            request.headers_mut().insert(
                tungstenite::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(ws, _)| ws)
    }

    async fn next_text(ws: &mut Ws) -> String {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
            {
                tungstenite::Message::Text(t) => return t.to_string(),
                tungstenite::Message::Close(f) => panic!("closed: {f:?}"),
                _ => {}
            }
        }
    }

    async fn close_code(ws: &mut Ws) -> u16 {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .unwrap()
            {
                Some(Ok(tungstenite::Message::Close(Some(f)))) => return f.code.into(),
                Some(Ok(_)) => {}
                other => panic!("no close frame: {other:?}"),
            }
        }
    }

    async fn agent_hello(port: u16, token: &str, sid: &str) -> Ws {
        agent_hello_as(port, token, sid, false).await
    }

    async fn agent_hello_as(port: u16, token: &str, sid: &str, chef: bool) -> Ws {
        let mut agent = ws(
            port,
            "/agent",
            &[
                ("host", format!("hub.localhost:{port}")),
                ("authorization", format!("Bearer {token}")),
            ],
        )
        .await
        .unwrap();
        let hello = ToHub::Hello {
            proto: protocol::PROTO,
            sid: sid.into(),
            label: Label {
                host: "vps-b".into(),
                cwd: "~/api".into(),
                model: "m".into(),
                ..Default::default()
            },
            mid: None,
            e2e: false,
            sealed: None,
            chef,
        };
        agent
            .send(tungstenite::Message::Text(
                serde_json::to_string(&hello).unwrap().into(),
            ))
            .await
            .unwrap();
        agent
    }

    /// Compares `value` with the iOS app's checked-in sample (see `apple/ios.md`), or
    /// rewrites it with `UPDATE_FIXTURES` set.
    fn check_fixture(name: &str, value: &Value) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../apple/Fixtures")
            .join(name);
        let text = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
        if std::env::var_os("UPDATE_FIXTURES").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &text).unwrap();
            return;
        }
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            old == text,
            "apple/Fixtures/{name} is stale; run `UPDATE_FIXTURES=1 cargo test -p tamale-hub` and check it in"
        );
    }

    fn set_cookie(response: &reqwest::Response) -> String {
        response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn a_browser_reaches_a_session_through_ticket_cookie_tunnel_and_socket() {
        let setup = start().await;
        let port = setup.port;
        let dash = format!("hub.localhost:{port}");
        let origin = format!("http://{dash}");
        let sid = "0123456789abcdef0123456789abcdef".to_string();
        let session_host = format!("{sid}.hub.localhost:{port}");
        let base = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        // A key itself is refused, so a bug can't send one unnoticed; so is a stranger.
        let key = new_key().unwrap();
        for (add, status) in [(key.as_str(), 400), ("f00d", 401)] {
            let r = http
                .post(format!("{base}/api/tokens"))
                .header("host", &dash)
                .header("origin", &origin)
                .json(&json!({"add": add}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), status, "{add}");
        }
        assert!(
            ws(
                port,
                "/agent",
                &[
                    ("host", dash.clone()),
                    ("authorization", format!("Bearer {}", new_auth()))
                ]
            )
            .await
            .is_err(),
            "an unknown key doesn't register by connecting"
        );

        let mut agent = agent_hello(port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent).await.contains("\"welcome\""));

        // The same sid under another key is refused.
        let mut thief = agent_hello(port, &setup.other_auth, &sid).await;
        assert_eq!(close_code(&mut thief).await, close::SID_TAKEN);

        // Wrong origin on the token endpoint.
        let r = http
            .post(format!("{base}/api/tokens"))
            .header("host", &dash)
            .header("origin", "http://evil.example")
            .json(&json!({"add": setup.auth}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);

        let r = http
            .post(format!("{base}/api/tokens"))
            .header("host", &dash)
            .header("origin", &origin)
            .json(&json!({"add": setup.auth}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let th = set_cookie(&r);
        assert!(
            r.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("HttpOnly; SameSite=Strict")
        );

        let list: Value = http
            .get(format!("{base}/api/sessions"))
            .header("host", &dash)
            .header("cookie", &th)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list["sessions"][0]["sid"], sid.as_str());
        assert_eq!(list["sessions"][0]["host"], "vps-b");

        // The other key sees nothing and can't get a ticket.
        let r = http
            .post(format!("{base}/api/tokens"))
            .header("host", &dash)
            .header("origin", &origin)
            .json(&json!({"add": setup.other_auth}))
            .send()
            .await
            .unwrap();
        let other_th = set_cookie(&r);
        let r = http
            .post(format!("{base}/api/ticket"))
            .header("host", &dash)
            .header("origin", &origin)
            .header("cookie", &other_th)
            .json(&json!({"sid": sid}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);

        let ticket: Value = http
            .post(format!("{base}/api/ticket"))
            .header("host", &dash)
            .header("origin", &origin)
            .header("cookie", &th)
            .json(&json!({"sid": sid}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let url = ticket["url"].as_str().unwrap();
        let path = url.split(&format!(":{port}")).nth(1).unwrap();

        // Without its cookie, the session host sends the browser for a ticket.
        let r = http
            .get(format!("{base}/"))
            .header("host", &session_host)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 303);
        assert!(
            r.headers()["location"]
                .to_str()
                .unwrap()
                .ends_with(&format!("/open/{sid}"))
        );

        let r = http
            .get(format!("{base}{path}"))
            .header("host", &session_host)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 303);
        let ts = set_cookie(&r);
        // A ticket works once.
        let again = http
            .get(format!("{base}{path}"))
            .header("host", &session_host)
            .send()
            .await
            .unwrap();
        assert_eq!(again.status(), 400);
        // And a session cookie works only on its own host.
        let elsewhere = format!("{}.hub.localhost:{port}", "f".repeat(32));
        let r = http
            .get(format!("{base}/app.js"))
            .header("host", &elsewhere)
            .header("cookie", &ts)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);

        // A tunnelled GET: tamale's headers pass, except set-cookie.
        let page = tokio::spawn({
            let http = http.clone();
            let (base, host, ts) = (base.clone(), session_host.clone(), ts.clone());
            async move {
                http.get(format!("{base}/"))
                    .header("host", host)
                    .header("cookie", ts)
                    .send()
                    .await
                    .unwrap()
            }
        });
        let get: ToAgent = serde_json::from_str(&next_text(&mut agent).await).unwrap();
        let ToAgent::Get { r, path } = get else {
            panic!("{get:?}")
        };
        assert_eq!(path, "/");
        let got = ToHub::Got {
            r,
            status: 200,
            body: "<p>page".into(),
            headers: vec![
                ("content-type".into(), "text/html; charset=utf-8".into()),
                (
                    "content-security-policy".into(),
                    "default-src 'self'".into(),
                ),
                ("set-cookie".into(), "th=x; Domain=hub.localhost".into()),
            ],
            base64: false,
        };
        agent
            .send(tungstenite::Message::Text(
                serde_json::to_string(&got).unwrap().into(),
            ))
            .await
            .unwrap();
        let page = page.await.unwrap();
        assert_eq!(page.status(), 200);
        assert!(page.headers().get("set-cookie").is_none());
        assert_eq!(page.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            page.headers()["content-security-policy"],
            "default-src 'self'"
        );
        assert_eq!(page.text().await.unwrap(), "<p>page");

        // Bytes come as base64 and go out as they were, compressed or not.
        let module = tokio::spawn({
            // Its own client: a workspace build gives reqwest gzip, which would decode
            // the body and drop the header this checks.
            let http = reqwest::Client::builder().no_gzip().build().unwrap();
            let (base, host, ts) = (base.clone(), session_host.clone(), ts.clone());
            async move {
                http.get(format!("{base}/pullspace/pullspace_bg.wasm"))
                    .header("host", host)
                    .header("cookie", ts)
                    .send()
                    .await
                    .unwrap()
            }
        });
        let get: ToAgent = serde_json::from_str(&next_text(&mut agent).await).unwrap();
        let ToAgent::Get { r, path } = get else {
            panic!("{get:?}")
        };
        assert_eq!(path, "/pullspace/pullspace_bg.wasm");
        let got = ToHub::Got {
            r,
            status: 200,
            body: BASE64.encode([0x1f, 0x8b, 0, 0xff]),
            headers: vec![
                ("content-type".into(), "application/wasm".into()),
                ("content-encoding".into(), "gzip".into()),
            ],
            base64: true,
        };
        agent
            .send(tungstenite::Message::Text(
                serde_json::to_string(&got).unwrap().into(),
            ))
            .await
            .unwrap();
        let module = module.await.unwrap();
        assert_eq!(module.status(), 200);
        assert_eq!(module.headers()["content-encoding"], "gzip");
        assert_eq!(
            module.bytes().await.unwrap().as_ref(),
            [0x1f, 0x8b, 0, 0xff]
        );

        // Paths outside the tunnel never reach tamale.
        let r = http
            .get(format!("{base}/etc/passwd"))
            .header("host", &session_host)
            .header("cookie", &ts)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);

        // The socket needs this session's own origin, even with the cookie.
        let sibling = format!("http://{}.hub.localhost:{port}", "f".repeat(32));
        assert!(
            ws(
                port,
                "/ws",
                &[
                    ("host", session_host.clone()),
                    ("cookie", ts.clone()),
                    ("origin", sibling)
                ]
            )
            .await
            .is_err()
        );
        let mut tab = ws(
            port,
            "/ws",
            &[
                ("host", session_host.clone()),
                ("cookie", ts.clone()),
                ("origin", format!("http://{session_host}")),
            ],
        )
        .await
        .unwrap();
        let open: ToAgent = serde_json::from_str(&next_text(&mut agent).await).unwrap();
        let ToAgent::Open { c } = open else {
            panic!("{open:?}")
        };
        agent
            .send(tungstenite::Message::Text(
                serde_json::to_string(&ToHub::Msg {
                    c,
                    data: "{\"t\":\"status\"}".into(),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        assert_eq!(next_text(&mut tab).await, "{\"t\":\"status\"}");
        tab.send(tungstenite::Message::Text(
            "{\"t\":\"send\",\"text\":\"hi\"}".into(),
        ))
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<ToAgent>(&next_text(&mut agent).await).unwrap(),
            ToAgent::Msg {
                c,
                data: "{\"t\":\"send\",\"text\":\"hi\"}".into()
            }
        );

        // A reconnect with the same key takes over; the tab is closed normally to retry.
        let mut agent2 = agent_hello(port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent2).await.contains("\"welcome\""));
        assert_eq!(close_code(&mut agent).await, close::REPLACED);
        assert_eq!(close_code(&mut tab).await, 1012);

        // While the agent is gone, the socket answers 503 so the page keeps retrying.
        drop(agent2);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let offline = ws(
            port,
            "/ws",
            &[
                ("host", session_host.clone()),
                ("cookie", ts.clone()),
                ("origin", format!("http://{session_host}")),
            ],
        )
        .await;
        match offline {
            Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 503),
            other => panic!("{:?}", other.map(|_| ())),
        }

        // Revoking the key (as the CLI would) drops the session within seconds.
        let mut agent3 = agent_hello(port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent3).await.contains("\"welcome\""));
        tokio::time::sleep(Duration::from_millis(1100)).await;
        setup
            .store
            .edit(|s| {
                s.keys.retain(|k| k.name != "mine");
                Ok(())
            })
            .unwrap();
        let code = tokio::time::timeout(Duration::from_secs(6), close_code(&mut agent3))
            .await
            .unwrap();
        assert_eq!(code, close::BAD_TOKEN);
        let r = http
            .get(format!("{base}/app.js"))
            .header("host", &session_host)
            .header("cookie", &ts)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404, "the session cookie dies with its key");
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn universal_links_are_served_only_when_configured() {
        let setup = start().await;
        let base = format!("http://127.0.0.1:{}", setup.port);
        let dash = format!("hub.localhost:{}", setup.port);
        let http = reqwest::Client::new();
        let get = || {
            http.get(format!("{base}/.well-known/apple-app-site-association"))
                .header("host", &dash)
                .send()
        };
        assert_eq!(get().await.unwrap().status(), 404);
        setup
            .hub
            .apple_app_id
            .set("TEAM.com.example.tamale".into())
            .unwrap();
        let r = get().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "application/json");
        let body: Value = r.json().await.unwrap();
        assert_eq!(
            body["applinks"]["details"][0]["appIDs"][0],
            "TEAM.com.example.tamale"
        );
        assert_eq!(
            body["applinks"]["details"][0]["components"][0]["/"],
            "/open/*"
        );
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn a_native_client_uses_a_bearer_key_without_cookies_tickets_or_origin() {
        let setup = start().await;
        let port = setup.port;
        let dash = format!("hub.localhost:{port}");
        let sid = "0123456789abcdef0123456789abcdef".to_string();
        let other_sid = "fedcba9876543210fedcba9876543210".to_string();
        let session_host = format!("{sid}.hub.localhost:{port}");
        let base = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::new();
        let failures = |hub: &Hub| {
            hub.failures
                .lock()
                .unwrap()
                .values()
                .map(|f| f.0)
                .sum::<u32>()
        };

        let mut agent = agent_hello(port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent).await.contains("\"welcome\""));
        let mut other_agent = agent_hello(port, &setup.other_auth, &other_sid).await;
        assert!(next_text(&mut other_agent).await.contains("\"welcome\""));
        let bearer = format!("Bearer {}", setup.auth);

        // The list: only this key's sessions, with no cookie and no Origin.
        let r = http
            .get(format!("{base}/api/sessions"))
            .header("host", &dash)
            .header("authorization", &bearer)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.headers().get("set-cookie").is_none());
        let list: Value = r.json().await.unwrap();
        assert_eq!(list["keys"].as_array().unwrap().len(), 1);
        assert_eq!(list["keys"][0]["name"], "mine");
        let sids: Vec<&str> = list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["sid"].as_str().unwrap())
            .collect();
        assert_eq!(sids, [sid.as_str()]);

        // An encrypted session too, for the app's sample of this list.
        let e2e_sid = "feedfacefeedfacefeedfacefeedface";
        let mut e2e_agent = ws(
            port,
            "/agent",
            &[("host", dash.clone()), ("authorization", bearer.clone())],
        )
        .await
        .unwrap();
        let say = |frame: &ToHub| {
            tungstenite::Message::Text(serde_json::to_string(frame).unwrap().into())
        };
        e2e_agent
            .send(say(&ToHub::Hello {
                proto: protocol::PROTO,
                sid: e2e_sid.into(),
                label: Label::default(),
                mid: Some("a".repeat(32)),
                e2e: true,
                sealed: Some("SEALED-LABEL".into()),
                chef: false,
            }))
            .await
            .unwrap();
        assert!(next_text(&mut e2e_agent).await.contains("\"welcome\""));
        e2e_agent
            .send(say(&ToHub::Status {
                busy: true,
                approval: true,
                title: String::new(),
                model: String::new(),
                sealed: Some("SEALED-STATUS".into()),
            }))
            .await
            .unwrap();
        let mut list: Value = loop {
            let list: Value = http
                .get(format!("{base}/api/sessions"))
                .header("host", &dash)
                .header("authorization", &bearer)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if list["sessions"][0]["sealed_status"].is_string() {
                break list;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        list["keys"][0]["id"] = "0123456789abcdef".into();
        list["keys"][0]["created"] = 1_700_000_000.into();
        for row in list["sessions"].as_array_mut().unwrap() {
            row["idle"] = 0.into();
        }
        check_fixture("hub/sessions.json", &list);
        drop(e2e_agent);

        // A wrong key is a 401 and counts as a failure, everywhere.
        let before = failures(&setup.hub);
        let wrong = format!("Bearer {}", new_auth());
        let r = http
            .get(format!("{base}/api/sessions"))
            .header("host", &dash)
            .header("authorization", &wrong)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let r = http
            .post(format!("{base}/api/ticket"))
            .header("host", &dash)
            .header("authorization", &wrong)
            .json(&json!({"sid": sid}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        match ws(
            port,
            "/ws",
            &[
                ("host", session_host.clone()),
                ("authorization", wrong.clone()),
            ],
        )
        .await
        {
            Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 401),
            other => panic!("{:?}", other.map(|_| ())),
        }
        assert_eq!(failures(&setup.hub), before + 3);

        // A ticket needs no Origin with a bearer; another key's session is a 404.
        let r = http
            .post(format!("{base}/api/ticket"))
            .header("host", &dash)
            .header("authorization", &bearer)
            .json(&json!({"sid": sid}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = http
            .post(format!("{base}/api/ticket"))
            .header("host", &dash)
            .header("authorization", &bearer)
            .json(&json!({"sid": other_sid}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let other_host = format!("{other_sid}.hub.localhost:{port}");
        match ws(
            port,
            "/ws",
            &[("host", other_host), ("authorization", bearer.clone())],
        )
        .await
        {
            Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 404),
            other => panic!("{:?}", other.map(|_| ())),
        }

        // The socket: no cookie, no Origin, no ticket.
        let mut tab = ws(
            port,
            "/ws",
            &[
                ("host", session_host.clone()),
                ("authorization", bearer.clone()),
            ],
        )
        .await
        .unwrap();
        let open: ToAgent = serde_json::from_str(&next_text(&mut agent).await).unwrap();
        let ToAgent::Open { c } = open else {
            panic!("{open:?}")
        };
        tab.send(tungstenite::Message::Text("{\"t\":\"hs1\"}".into()))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<ToAgent>(&next_text(&mut agent).await).unwrap(),
            ToAgent::Msg {
                c,
                data: "{\"t\":\"hs1\"}".into()
            }
        );

        // The cookie path is unchanged: without a bearer, Origin is still required.
        assert!(
            ws(port, "/ws", &[("host", session_host.clone())])
                .await
                .is_err()
        );
        let r = http
            .post(format!("{base}/api/ticket"))
            .header("host", &dash)
            .json(&json!({"sid": sid}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn an_encrypted_session_shows_the_hub_only_opaque_labels_and_keeps_its_artifacts() {
        let setup = start().await;
        let port = setup.port;
        let dash = format!("hub.localhost:{port}");
        let origin = format!("http://{dash}");
        let sid = "feedfacefeedfacefeedfacefeedface".to_string();
        let session_host = format!("{sid}.hub.localhost:{port}");
        let base = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let auth = [
            ("host", dash.clone()),
            ("authorization", format!("Bearer {}", setup.auth)),
        ];
        let say = |frame: &ToHub| {
            tungstenite::Message::Text(serde_json::to_string(frame).unwrap().into())
        };
        let hello = |sid: &str, mid: Option<&str>, e2e, sealed: Option<String>| ToHub::Hello {
            proto: protocol::PROTO,
            sid: sid.into(),
            label: Label::default(),
            mid: mid.map(String::from),
            e2e,
            sealed,
            chef: false,
        };

        // Malformed encryption fields are refused.
        let other = "1".repeat(32);
        for bad in [
            hello(&other, None, true, None),
            hello(&other, Some("NOT-HEX"), true, None),
            hello(
                &other,
                Some(&"a".repeat(32)),
                true,
                Some("x".repeat(protocol::MAX_SEALED + 1)),
            ),
        ] {
            let mut agent = ws(port, "/agent", &auth).await.unwrap();
            agent.send(say(&bad)).await.unwrap();
            assert_eq!(close_code(&mut agent).await, close::BAD_HELLO);
        }
        // So is a protocol from the future; an older tamale still connects, in plaintext.
        let mut agent = ws(port, "/agent", &auth).await.unwrap();
        let future = r#"{"t":"hello","proto":99,"sid":"11111111111111111111111111111111","label":{"host":"","cwd":"","model":""}}"#;
        agent
            .send(tungstenite::Message::Text(future.into()))
            .await
            .unwrap();
        assert_eq!(close_code(&mut agent).await, close::BAD_HELLO);
        let mut old = ws(port, "/agent", &auth).await.unwrap();
        let v1 = r#"{"t":"hello","proto":1,"sid":"22222222222222222222222222222222","label":{"host":"old","cwd":"~","model":"m"}}"#;
        old.send(tungstenite::Message::Text(v1.into()))
            .await
            .unwrap();
        assert!(next_text(&mut old).await.contains("\"welcome\""));

        let mid = "a".repeat(32);
        let mut agent = ws(port, "/agent", &auth).await.unwrap();
        agent
            .send(say(&hello(
                &sid,
                Some(&mid),
                true,
                Some("SEALED-LABEL".into()),
            )))
            .await
            .unwrap();
        assert!(next_text(&mut agent).await.contains("\"welcome\""));
        agent
            .send(say(&ToHub::Status {
                busy: true,
                approval: false,
                title: String::new(),
                model: String::new(),
                sealed: Some("SEALED-STATUS".into()),
            }))
            .await
            .unwrap();

        let r = http
            .post(format!("{base}/api/tokens"))
            .header("host", &dash)
            .header("origin", &origin)
            .json(&json!({"add": setup.auth}))
            .send()
            .await
            .unwrap();
        let th = set_cookie(&r);

        let session = loop {
            let list: Value = http
                .get(format!("{base}/api/sessions"))
                .header("host", &dash)
                .header("cookie", &th)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            let row = list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["sid"] == sid.as_str())
                .cloned()
                .unwrap();
            if row["sealed_status"] == "SEALED-STATUS" {
                break row;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(session["mid"], mid.as_str());
        assert_eq!(session["e2e"], true);
        assert_eq!(session["sealed_label"], "SEALED-LABEL");
        assert_eq!(session["busy"], true, "the list can still badge it");
        for field in ["host", "cwd", "model", "title"] {
            assert_eq!(session[field], "", "{field}");
        }

        let ticket: Value = http
            .post(format!("{base}/api/ticket"))
            .header("host", &dash)
            .header("origin", &origin)
            .header("cookie", &th)
            .json(&json!({"sid": sid}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(ticket["mid"], mid.as_str());
        assert_eq!(ticket["e2e"], true);
        let path = ticket["url"]
            .as_str()
            .unwrap()
            .split_once(&session_host)
            .unwrap()
            .1
            .to_string();
        let r = http
            .get(format!("{base}{path}"))
            .header("host", &session_host)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 303);
        assert_eq!(
            r.headers()["location"],
            "/?e2e=1",
            "the page must insist on the handshake"
        );
        let ts = set_cookie(&r);

        // Artifacts aren't asked of tamale at all; the frame page and script are.
        let r = http
            .get(format!("{base}/artifact/abc"))
            .header("host", &session_host)
            .header("cookie", &ts)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let tunnelled = http
            .get(format!("{base}/artifact-frame"))
            .header("host", &session_host)
            .header("cookie", &ts)
            .send();
        let (r, get) = tokio::join!(tunnelled, next_text(&mut agent));
        assert!(get.contains("\"/artifact-frame\""), "{get}");
        assert!(!get.contains("/artifact/abc"), "{get}");
        let _ = r;

        // The hub's page has what it needs for the keys.
        let r = http
            .get(format!("{base}/seal.js"))
            .header("host", &dash)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(r.text().await.unwrap().contains("machineId"));
        let r = http
            .get(format!("{base}/qrcode.js"))
            .header("host", &dash)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }

    #[tokio::test]
    async fn the_page_registers_rotates_and_deletes_keys() {
        let setup = start().await;
        let port = setup.port;
        let dash = format!("hub.localhost:{port}");
        let origin = format!("http://{dash}");
        let base = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::new();
        let post = |path: &str, cookie: &str, body: Value| {
            http.post(format!("{base}{path}"))
                .header("host", &dash)
                .header("origin", &origin)
                .header("cookie", cookie)
                .json(&body)
                .send()
        };
        let auth = new_auth();

        // Another site can't make keys in a visitor's name.
        let r = http
            .post(format!("{base}/api/keys"))
            .header("host", &dash)
            .header("origin", "http://evil.example")
            .json(&json!({ "auth": auth }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);

        // Only well-formed auth values, and never a key.
        for bad in ["short", &"A".repeat(64), &new_key().unwrap()] {
            let r = post("/api/keys", "", json!({ "auth": bad })).await.unwrap();
            assert_eq!(r.status(), 400, "{bad}");
        }

        let r = post(
            "/api/keys",
            "",
            json!({ "auth": auth, "label": "My Laptop" }),
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 200);
        let th = set_cookie(&r);
        let made: Value = r.json().await.unwrap();
        assert_eq!(made["name"], "my-laptop");
        let stored = setup.store.load().unwrap();
        assert!(stored.by_name("my-laptop").unwrap().web && stored.by_auth(&auth).is_some());

        // Nothing about the key comes back, and it can't be registered twice.
        assert_eq!(made.as_object().unwrap().len(), 1);
        let r = post("/api/keys", "", json!({ "auth": auth }))
            .await
            .unwrap();
        assert_eq!(r.status(), 409);

        // The browser that made it holds it, and machines can connect with it.
        let list: Value = http
            .get(format!("{base}/api/sessions"))
            .header("host", &dash)
            .header("cookie", &th)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list["keys"][0]["name"], "my-laptop");
        assert_eq!(list["keys"][0]["id"], state::hash(&auth)[..16]);
        assert_eq!(list["signup"], true);
        let sid = "abcdef0123456789abcdef0123456789";
        let mut a = agent_hello(port, &auth, sid).await;
        assert!(next_text(&mut a).await.contains("\"welcome\""));

        // Two CLI keys plus this one and another, and the test hub holds at most four.
        let r = post("/api/keys", "", json!({ "auth": new_auth() }))
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = post("/api/keys", "", json!({ "auth": new_auth() }))
            .await
            .unwrap();
        assert_eq!(r.status(), 503);

        // Only a holder can replace a key, and only with a well-formed auth value.
        let new = new_auth();
        let r = post(
            "/api/keys/rotate",
            "",
            json!({ "name": "my-laptop", "auth": new }),
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 404);
        let r = post(
            "/api/keys/rotate",
            &th,
            json!({ "name": "my-laptop", "auth": "short" }),
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 400);
        let r = post(
            "/api/keys/rotate",
            &th,
            json!({ "name": "my-laptop", "auth": setup.auth }),
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 409, "not to another key's");
        let r = post(
            "/api/keys/rotate",
            &th,
            json!({ "name": "my-laptop", "auth": new }),
        )
        .await
        .unwrap();
        assert_eq!(r.status(), 200);
        let new_th = set_cookie(&r);
        assert_ne!(new_th, th);
        // Its sessions end, the old key no longer lists anything or connects.
        let code = tokio::time::timeout(Duration::from_secs(6), close_code(&mut a))
            .await
            .unwrap();
        assert_eq!(code, close::BAD_TOKEN);
        let old: Value = http
            .get(format!("{base}/api/sessions"))
            .header("host", &dash)
            .header("cookie", &th)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(old["keys"].as_array().unwrap().len(), 0);
        assert!(
            ws(
                port,
                "/agent",
                &[
                    ("host", dash.clone()),
                    ("authorization", format!("Bearer {auth}"))
                ]
            )
            .await
            .is_err()
        );
        let mut b = agent_hello(port, &new, sid).await;
        assert!(next_text(&mut b).await.contains("\"welcome\""));

        let r = post("/api/keys/delete", &new_th, json!({ "name": "my-laptop" }))
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert!(
            r.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        assert!(setup.store.load().unwrap().by_name("my-laptop").is_none());
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn closed_signup_refuses_new_keys_and_the_daily_limit_holds() {
        let setup = start().await;
        let dir = setup.store.dir.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let public = Public::parse(&format!("http://hub.localhost:{port}"), true).unwrap();
        let closed = Hub::new(
            public,
            Store { dir: dir.clone() },
            Signup {
                open: false,
                max_keys: 500,
            },
        )
        .unwrap();
        tokio::spawn(run(closed, listener));
        let dash = format!("hub.localhost:{port}");
        let http = reqwest::Client::new();
        let r = http
            .post(format!("http://127.0.0.1:{port}/api/keys"))
            .header("host", &dash)
            .header("origin", format!("http://{dash}"))
            .json(&json!({ "auth": new_auth() }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let public = Public::parse(&format!("http://hub.localhost:{port}"), true).unwrap();
        let open = Hub::new(
            public,
            Store { dir: dir.clone() },
            Signup {
                open: true,
                max_keys: 500,
            },
        )
        .unwrap();
        tokio::spawn(run(open, listener));
        let dash = format!("hub.localhost:{port}");
        for n in 0..=SIGNUPS_PER_IP {
            let r = http
                .post(format!("http://127.0.0.1:{port}/api/keys"))
                .header("host", &dash)
                .header("origin", format!("http://{dash}"))
                .json(&json!({ "auth": new_auth() }))
                .send()
                .await
                .unwrap();
            assert_eq!(
                r.status(),
                if n < SIGNUPS_PER_IP { 200 } else { 429 },
                "key {n}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unused_page_keys_expire_but_cli_keys_stay() {
        let dir = std::env::temp_dir().join(format!(
            "tamale-hub-expire-{}",
            state::hex(&state::random::<8>().unwrap())
        ));
        let store = Store::open(Some(dir.clone())).unwrap();
        store
            .edit(|s| {
                let mut old_web = Key::new("old-web", "a1", true);
                old_web.created = 0;
                let mut idle_web = Key::new("idle-web", "a2", true);
                idle_web.last_used = Some(state::now() - IDLE_EXPIRY - 10);
                let mut old_cli = Key::new("old-cli", "a3", false);
                old_cli.created = 0;
                let fresh_web = Key::new("fresh-web", "a4", true);
                s.keys.extend([old_web, idle_web, old_cli, fresh_web]);
                Ok(())
            })
            .unwrap();
        let public = Public::parse("http://hub.localhost:1", true).unwrap();
        let hub = Hub::new(
            public,
            Store { dir: dir.clone() },
            Signup {
                open: true,
                max_keys: 10,
            },
        )
        .unwrap();
        hub.sweep();
        let names: Vec<String> = store
            .load()
            .unwrap()
            .keys
            .into_iter()
            .map(|k| k.name)
            .collect();
        assert_eq!(names, ["old-cli", "fresh-web"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_hosts_and_www_are_handled() {
        let setup = start().await;
        let base = format!("http://127.0.0.1:{}", setup.port);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let r = http
            .get(&base)
            .header("host", "evil.example")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let r = http
            .get(&base)
            .header("host", format!("www.hub.localhost:{}", setup.port))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 308);
        let r = http
            .get(&base)
            .header("host", format!("hub.localhost:{}", setup.port))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["x-frame-options"], "SAMEORIGIN");
        let csp = r.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("frame-ancestors 'self'"), "{csp}");
        assert!(
            csp.contains(&format!(
                "frame-src 'self' http://*.hub.localhost:{}",
                setup.port
            )),
            "{csp}"
        );
        assert!(r.text().await.unwrap().contains("tamale-hub"));
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    /// Asks `check` every so often for a few seconds, until it's true.
    async fn until(what: &str, check: impl Fn() -> bool) {
        for _ in 0..200 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("never happened: {what}");
    }

    /// A browser's socket, by the native route, with a key.
    async fn tab(port: u16, sid: &str, auth: &str) -> Ws {
        ws(
            port,
            "/ws",
            &[
                ("host", format!("{sid}.hub.localhost:{port}")),
                ("authorization", format!("Bearer {auth}")),
            ],
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_tab_that_stops_reading_is_cut_off_rather_than_filling_the_hub() {
        let setup = start_with(1 << 20).await;
        let sid = "1".repeat(32);
        let mut agent = agent_hello(setup.port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent).await.contains("\"welcome\""));
        // Never read from again.
        let _stuck = tab(setup.port, &sid, &setup.auth).await;
        let opened = next_text(&mut agent).await;
        let Ok(ToAgent::Open { c }) = serde_json::from_str(&opened) else {
            panic!("{opened}");
        };

        // Far more than the limit, and than the socket's own buffers take.
        let piece = "x".repeat(1 << 20);
        let frame = serde_json::to_string(&ToHub::Msg { c, data: piece }).unwrap();
        let pump = async {
            for _ in 0..512 {
                if agent
                    .send(tungstenite::Message::Text(frame.clone().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(60), pump)
            .await
            .expect("the agent kept being read");
        let hub = setup.hub.clone();
        until("the tab being dropped", || {
            hub.sessions
                .lock()
                .unwrap()
                .get(&sid)
                .is_some_and(|s| s.clients.is_empty())
        })
        .await;
        let closed = serde_json::to_string(&ToAgent::Close { c }).unwrap();
        assert_eq!(
            next_text(&mut agent).await,
            closed,
            "tamale is told it's gone"
        );
        until("what was queued being let go", || {
            hub.queued.load(Ordering::Relaxed) == 0
        })
        .await;
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn an_agent_that_stops_reading_is_cut_off_rather_than_filling_the_hub() {
        let setup = start_with(1 << 20).await;
        let sid = "2".repeat(32);
        let mut agent = agent_hello(setup.port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent).await.contains("\"welcome\""));
        let mut browser = tab(setup.port, &sid, &setup.auth).await;
        // The agent reads nothing from here on, but a tab keeps sending.
        let piece = "x".repeat(1 << 20);
        let pump = async {
            for _ in 0..512 {
                if browser
                    .send(tungstenite::Message::Text(piece.clone().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(60), pump)
            .await
            .expect("the tab kept being read");
        let hub = setup.hub.clone();
        until("the agent being dropped", || {
            hub.sessions
                .lock()
                .unwrap()
                .get(&sid)
                .is_some_and(|s| s.agent.is_none())
        })
        .await;
        until("what was queued being let go", || {
            hub.queued.load(Ordering::Relaxed) == 0
        })
        .await;
        drop(agent);
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn a_key_at_its_limit_makes_room_by_dropping_the_session_offline_longest() {
        let setup = start().await;
        let mut agents = Vec::new();
        for i in 0..MAX_SESSIONS_PER_KEY {
            let sid = format!("{:032x}", i + 1);
            let mut agent = agent_hello(setup.port, &setup.auth, &sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            agents.push(agent);
        }
        // All online: one more is refused.
        let extra = format!("{:032x}", 0xffff);
        let mut refused = agent_hello(setup.port, &setup.auth, &extra).await;
        assert_eq!(close_code(&mut refused).await, close::TOO_MANY);

        // The first goes offline (a laptop asleep), and stays listed...
        drop(agents.remove(0));
        let first = format!("{:032x}", 1);
        let hub = setup.hub.clone();
        until("the first session going offline", || {
            hub.sessions
                .lock()
                .unwrap()
                .get(&first)
                .is_some_and(|s| s.agent.is_none())
        })
        .await;
        // ...until a new session needs its place.
        let mut extra_agent = agent_hello(setup.port, &setup.auth, &extra).await;
        assert!(next_text(&mut extra_agent).await.contains("\"welcome\""));
        assert!(!setup.hub.sessions.lock().unwrap().contains_key(&first));
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn offline_sessions_can_be_forgotten_but_online_and_others_stay() {
        let setup = start().await;
        let port = setup.port;
        let dash = format!("hub.localhost:{port}");
        let (gone, live, theirs) = ("d".repeat(32), "e".repeat(32), "f".repeat(32));
        let mut agents = Vec::new();
        for (sid, auth) in [
            (&gone, &setup.auth),
            (&live, &setup.auth),
            (&theirs, &setup.other_auth),
        ] {
            let mut agent = agent_hello(port, auth, sid).await;
            assert!(next_text(&mut agent).await.contains("\"welcome\""));
            agents.push(agent);
        }
        // The first and the other key's go offline.
        drop(agents.remove(2));
        drop(agents.remove(0));
        let hub = setup.hub.clone();
        until("both going offline", || {
            let sessions = hub.sessions.lock().unwrap();
            [&gone, &theirs]
                .iter()
                .all(|sid| sessions.get(*sid).is_some_and(|s| s.agent.is_none()))
        })
        .await;
        let http = reqwest::Client::new();
        let r = http
            .post(format!("http://127.0.0.1:{port}/api/sessions/forget"))
            .header("host", &dash)
            .bearer_auth(&setup.auth)
            .json(&json!({ "sids": [gone, live, theirs] }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let body: Value = r.json().await.unwrap();
        assert_eq!(body["forgotten"], json!([gone]));
        {
            let sessions = setup.hub.sessions.lock().unwrap();
            assert!(!sessions.contains_key(&gone));
            assert!(sessions.contains_key(&live), "online sessions stay");
            assert!(sessions.contains_key(&theirs), "another key's stay");
        }
        // Its tamale coming back lists it again.
        let mut back = agent_hello(port, &setup.auth, &gone).await;
        assert!(next_text(&mut back).await.contains("\"welcome\""));
        assert!(setup.hub.sessions.lock().unwrap().contains_key(&gone));
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn an_ended_session_leaves_the_list_and_its_tabs_stop_for_good() {
        let setup = start().await;
        let sid = "3".repeat(32);
        let mut agent = agent_hello(setup.port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent).await.contains("\"welcome\""));
        let mut browser = tab(setup.port, &sid, &setup.auth).await;
        assert!(next_text(&mut agent).await.contains("\"open\""));

        let end = serde_json::to_string(&ToHub::End).unwrap();
        agent
            .send(tungstenite::Message::Text(end.into()))
            .await
            .unwrap();
        assert_eq!(close_code(&mut browser).await, 4000, "not one to retry");
        let hub = setup.hub.clone();
        until("the session being dropped", || {
            !hub.sessions.lock().unwrap().contains_key(&sid)
        })
        .await;
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn a_key_has_one_chef_online_and_a_new_one_replaces_one_gone_offline() {
        let setup = start().await;
        let port = setup.port;
        let (first, second, worker) = ("a".repeat(32), "b".repeat(32), "c".repeat(32));
        let mut chef = agent_hello_as(port, &setup.auth, &first, true).await;
        assert!(next_text(&mut chef).await.contains("\"welcome\""));

        // A second chef under the key is refused for good; other sessions aren't.
        let mut rival = agent_hello_as(port, &setup.auth, &second, true).await;
        assert_eq!(close_code(&mut rival).await, close::CHEF_TAKEN);
        assert!(protocol::is_permanent(close::CHEF_TAKEN));
        let mut session = agent_hello(port, &setup.auth, &worker).await;
        assert!(next_text(&mut session).await.contains("\"welcome\""));
        // Another key has a chef of its own.
        let mut theirs = agent_hello_as(port, &setup.other_auth, &second, true).await;
        assert!(next_text(&mut theirs).await.contains("\"welcome\""));
        drop(theirs);
        let hub = setup.hub.clone();
        until("the other key's chef going", || {
            hub.sessions
                .lock()
                .unwrap()
                .get(&second)
                .is_none_or(|s| s.agent.is_none())
        })
        .await;
        hub.sessions.lock().unwrap().remove(&second);

        // The chef reconnecting under its own sid takes its place back.
        let mut again = agent_hello_as(port, &setup.auth, &first, true).await;
        assert!(next_text(&mut again).await.contains("\"welcome\""));
        assert_eq!(close_code(&mut chef).await, close::REPLACED);
        let listed = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/api/sessions"))
            .header("host", format!("hub.localhost:{port}"))
            .bearer_auth(&setup.auth)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        let chefs: Vec<_> = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["chef"] == true)
            .map(|s| s["sid"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(chefs, vec![first.clone()]);

        // Gone offline, it gives way to a new chef, and is refused when it comes back.
        drop(again);
        until("the chef going offline", || {
            hub.sessions
                .lock()
                .unwrap()
                .get(&first)
                .is_some_and(|s| s.agent.is_none())
        })
        .await;
        let mut new = agent_hello_as(port, &setup.auth, &second, true).await;
        assert!(next_text(&mut new).await.contains("\"welcome\""));
        assert!(
            !hub.sessions.lock().unwrap().contains_key(&first),
            "the old one left the list"
        );
        let mut old = agent_hello_as(port, &setup.auth, &first, true).await;
        assert_eq!(close_code(&mut old).await, close::CHEF_TAKEN);
        drop((new, session));
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn labels_and_titles_the_hub_keeps_are_bounded() {
        let setup = start().await;
        let port = setup.port;
        let auth = [
            ("host", format!("hub.localhost:{port}")),
            ("authorization", format!("Bearer {}", setup.auth)),
        ];
        let say = |frame: &ToHub| {
            tungstenite::Message::Text(serde_json::to_string(frame).unwrap().into())
        };
        let long = "x".repeat(protocol::MAX_SEALED + 1);
        let sid = "3".repeat(32);
        let hello = |label| ToHub::Hello {
            proto: protocol::PROTO,
            sid: sid.clone(),
            label,
            mid: None,
            e2e: false,
            sealed: None,
            chef: false,
        };
        for label in [
            Label {
                host: long.clone(),
                ..Label::default()
            },
            Label {
                cwd: long.clone(),
                ..Label::default()
            },
            Label {
                model: long.clone(),
                ..Label::default()
            },
        ] {
            let mut agent = ws(port, "/agent", &auth).await.unwrap();
            agent.send(say(&hello(label))).await.unwrap();
            assert_eq!(close_code(&mut agent).await, close::BAD_HELLO);
        }

        let mut agent = ws(port, "/agent", &auth).await.unwrap();
        agent.send(say(&hello(Label::default()))).await.unwrap();
        assert!(next_text(&mut agent).await.contains("\"welcome\""));
        let status = |title: &str, model: &str| ToHub::Status {
            busy: false,
            approval: false,
            title: title.into(),
            model: model.into(),
            sealed: None,
        };
        agent.send(say(&status("short", "m"))).await.unwrap();
        agent.send(say(&status(&long, &long))).await.unwrap();
        agent.send(say(&status("t", "last"))).await.unwrap();
        let hub = setup.hub.clone();
        let shown = || {
            let sessions = hub.sessions.lock().unwrap();
            let session = sessions.get(&sid).unwrap();
            (session.title.clone(), session.label.model.clone())
        };
        until("the last status", || shown().1 == "last").await;
        assert_eq!(shown().0, "t");
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn a_ticket_says_which_key_its_session_is_under_and_a_key_cant_hoard_them() {
        let setup = start().await;
        let port = setup.port;
        let sid = "4".repeat(32);
        let mut agent = agent_hello(port, &setup.auth, &sid).await;
        assert!(next_text(&mut agent).await.contains("\"welcome\""));
        let base = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::new();
        let ask = |auth: String| {
            http.post(format!("{base}/api/ticket"))
                .header("host", format!("hub.localhost:{port}"))
                .bearer_auth(auth)
                .json(&json!({ "sid": sid }))
                .send()
        };
        let first = ask(setup.auth.clone()).await.unwrap();
        assert_eq!(first.status(), 200);
        let first: Value = first.json().await.unwrap();
        assert_eq!(first["id"], state::hash(&setup.auth)[..16]);
        for _ in 1..MAX_TICKETS_PER_KEY {
            assert_eq!(ask(setup.auth.clone()).await.unwrap().status(), 200);
        }
        assert_eq!(ask(setup.auth.clone()).await.unwrap().status(), 429);
        // Another key's tickets are its own business: this one just doesn't have the session.
        assert_eq!(ask(setup.other_auth.clone()).await.unwrap().status(), 404);
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }

    #[tokio::test]
    async fn the_dashboard_keeps_to_its_own_origin() {
        let setup = start().await;
        let r = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}", setup.port))
            .header("host", format!("hub.localhost:{}", setup.port))
            .send()
            .await
            .unwrap();
        // Popups keep their opener: a session's frame sends an artifact to the tab it opens.
        assert_eq!(
            r.headers()["cross-origin-opener-policy"],
            "same-origin-allow-popups"
        );
        assert_eq!(r.headers()["cross-origin-resource-policy"], "same-origin");
        assert!(
            r.headers().get("strict-transport-security").is_none(),
            "this one's http"
        );
        let _ = std::fs::remove_dir_all(&setup.store.dir);
    }
}
