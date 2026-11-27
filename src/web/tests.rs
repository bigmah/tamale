use super::artifacts::{artifact_file_name, content_hash, save_artifact};
use super::mirror::{MAX_PAGE_BYTES, MAX_TOOL_OUTPUT, clip, wire_entry};
use super::*;

fn view(entries: &[Entry]) -> View<'_> {
    View {
        status: Status::default(),
        entries,
        approval: None,
        question: None,
        roots: Vec::new(),
        artifact_dir: None,
        kitchen: None,
    }
}

fn from(message: &str) -> (u64, usize) {
    let v: Value = serde_json::from_str(message).unwrap();
    (
        v["from"].as_u64().unwrap(),
        v["items"].as_array().unwrap().len(),
    )
}

#[test]
fn mirror_sends_only_entries_that_changed() {
    let mut mirror = Mirror::default();
    let mut entries = vec![Entry::User("hi".into()), Entry::Assistant("He".into())];
    let first = mirror.update(&view(&entries));
    assert_eq!(first.len(), 4, "entries, status, approval and questions");
    assert_eq!(from(&first[0]), (0, 2));
    assert!(mirror.update(&view(&entries)).is_empty());

    // Streaming text re-sends just the entry being written.
    entries[1] = Entry::Assistant("Hello **there**".into());
    let streamed = mirror.update(&view(&entries));
    assert_eq!(streamed.len(), 1);
    assert_eq!(from(&streamed[0]), (1, 1));
    assert!(streamed[0].contains("<strong>there</strong>"));

    // A removed entry (a retried request) truncates the tab's list.
    entries.pop();
    assert_eq!(from(&mirror.update(&view(&entries))[0]), (1, 0));

    let full = mirror.full(None);
    assert_eq!(from(&full[0]), (0, 1));
    assert!(full[1].contains("\"t\":\"status\""));
}

#[test]
fn a_tab_can_start_at_the_end_and_page_back() {
    let mut mirror = Mirror::default();
    let entries: Vec<Entry> = (0..250).map(|i| Entry::User(format!("m{i}"))).collect();
    mirror.update(&view(&entries));

    let full = mirror.full(Some(60));
    let loading: Value = serde_json::from_str(&full[0]).unwrap();
    assert_eq!(loading["t"], "loading");
    assert_eq!(loading["bytes"], full[1].len(), "the size of what follows");
    assert_eq!(from(&full[1]), (190, 60));

    let page: Value = serde_json::from_str(&mirror.older(190)).unwrap();
    assert_eq!(page["t"], "older");
    assert_eq!((&page["before"], &page["from"]), (&190.into(), &90.into()));
    assert_eq!(page["items"].as_array().unwrap().len(), 100);
    assert_eq!(page["items"][99]["text"], "m189");

    let page: Value = serde_json::from_str(&mirror.older(90)).unwrap();
    assert_eq!(page["from"], 0, "the last page is short");
    assert_eq!(page["items"].as_array().unwrap().len(), 90);

    // From a tab that's behind a rewind, and from one with nothing left to get.
    let page: Value = serde_json::from_str(&mirror.older(9_999)).unwrap();
    assert_eq!((&page["before"], &page["from"]), (&250.into(), &150.into()));
    let page: Value = serde_json::from_str(&mirror.older(0)).unwrap();
    assert!(page["items"].as_array().unwrap().is_empty());
}

#[test]
fn a_page_stops_at_its_size_limit_but_always_has_an_entry() {
    let mut mirror = Mirror::default();
    let big = "x".repeat(MAX_PAGE_BYTES / 2);
    let entries = vec![
        Entry::User(big.clone()),
        Entry::User(big.clone()),
        Entry::User(big.clone()),
        Entry::User("y".repeat(MAX_PAGE_BYTES * 2)),
    ];
    mirror.update(&view(&entries));
    assert_eq!(
        from(&mirror.full(Some(60))[1]),
        (3, 1),
        "one entry over the limit still goes"
    );
    let page: Value = serde_json::from_str(&mirror.older(3)).unwrap();
    assert_eq!(page["from"], 2, "two would pass the limit with their JSON");
}

#[test]
fn a_tab_that_wants_it_all_is_sent_no_progress() {
    let mut mirror = Mirror::default();
    let entries = vec![Entry::User("hi".into())];
    mirror.update(&view(&entries));
    assert!(mirror.full(None)[0].contains("\"t\":\"entries\""));
}

#[test]
fn artifacts_keep_their_url_across_renders() {
    let mut mirror = Mirror::default();
    let reply = "Sure.\n\n```openui\ntitle = \"T\"\nroot = Card([])\n```";
    let mut entries = vec![Entry::Assistant(reply.into())];
    let first = mirror.update(&view(&entries));
    let url = first[0]
        .split("data-src=\\\"")
        .nth(1)
        .and_then(|rest| rest.split("\\\"").next())
        .expect("an embed")
        .to_string();
    let id = url.strip_prefix("/artifact/").unwrap();
    assert_eq!(id.len(), 32);
    let source = mirror.artifacts.lock().unwrap().sources[id].clone();
    assert_eq!(&*source, "title = \"T\"\nroot = Card([])\n");

    entries[0] = Entry::Assistant(format!("{reply}\n\nMore."));
    let again = mirror.update(&view(&entries));
    assert!(again[0].contains(&url), "{}", again[0]);
    assert_eq!(mirror.artifacts.lock().unwrap().sources.len(), 1);
}

#[test]
fn native_clients_get_markdown_artifact_blocks_and_stable_hashes() {
    let mut mirror = Mirror::default();
    let doc = "title = \"T\"\nroot = Card([TextContent(\"x\")])\n";
    let entries = vec![
        Entry::Assistant(format!("Sure.\n\n```openui\n{doc}```\n\nMore.")),
        Entry::Note("*noted*".into()),
        Entry::Assistant("```openui\ntitle = \"Half\"\nroot = Card([".into()),
    ];
    let sent = mirror.update(&view(&entries));
    let message: Value = serde_json::from_str(&sent[0]).unwrap();
    let items = message["items"].as_array().unwrap();

    let reply = &items[0];
    assert_eq!(reply["md"], "Sure.\n\n<!-- artifact 0 -->\n\nMore.");
    assert!(
        reply["html"].as_str().unwrap().contains("data-src"),
        "the page's html stays"
    );
    let block = &reply["artifacts"][0];
    assert_eq!(block["title"], "T");
    assert_eq!(block["done"], true);
    assert_eq!(block["hash"], content_hash(doc));
    let id = block["id"].as_str().unwrap();
    assert!(
        reply["html"]
            .as_str()
            .unwrap()
            .contains(&format!("/artifact/{id}"))
    );

    assert_eq!(items[1]["md"], "*noted*");
    assert!(items[1].get("artifacts").is_none());

    let building = &items[2]["artifacts"][0];
    assert_eq!(building["done"], false);
    assert!(building["id"].is_null() && building["hash"].is_null());
    assert_eq!(building["title"], "Half");

    // The artifact reply names the document by the same hash, and carries it, and a page
    // that draws it for clients that show artifacts in a web view.
    let artifacts = mirror.artifacts.lock().unwrap();
    let reply = artifact_reply(
        id,
        artifacts.sources.get(id).map(|p| &**p),
        artifacts.hashes.get(id).map(|h| h.as_str()),
        true,
    );
    assert_eq!(reply["hash"], content_hash(doc));
    assert_eq!(reply["source"], doc);
    assert_eq!(reply["title"], "T");
    // A page that needs nothing from anywhere: the document, the viewer and its module.
    let html = reply["html"].as_str().unwrap();
    assert!(html.contains("id=\"openui-artifact\""), "{}", &html[..400]);
    assert!(html.contains("id=\"openui-module\"") && html.contains("__wbg_init"));
    assert!(!html.contains("src=\"/"), "it loads nothing");
    // The web UI asks for the document alone.
    let bare = artifact_reply(id, artifacts.sources.get(id).map(|p| &**p), None, false);
    assert!(bare["html"].is_null() && bare["source"] == doc);
    assert_eq!(
        content_hash("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn artifacts_are_saved_once_under_their_title() {
    let dir = std::env::temp_dir().join(format!(
        "tamale-artifacts-test-{}",
        crate::session::new_id()
    ));
    let reply = "Done.\n\n```openui\ntitle = \"Auth flow: v2!\"\nroot = Card([])\n```";
    let entries = vec![Entry::Assistant(reply.into())];
    let mut mirror = Mirror::default();
    let mut with_dir = view(&entries);
    with_dir.artifact_dir = Some(dir.clone());
    mirror.update(&with_dir);
    mirror.update(&with_dir);
    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].starts_with("auth-flow-v2-") && files[0].ends_with(".openui"));
    let saved = std::fs::read_to_string(dir.join(&files[0])).unwrap();
    assert_eq!(saved, "title = \"Auth flow: v2!\"\nroot = Card([])\n");
    assert_eq!(
        artifact_file_name("root = Card([])").split('-').next(),
        Some("artifact")
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn saved_artifacts_list_the_current_session_first_and_share_chat_urls() {
    let root = std::env::temp_dir().join(format!("tamale-saved-test-{}", crate::session::new_id()));
    let (now, old, empty) = (root.join("now"), root.join("old"), root.join("empty"));
    let store = Mutex::new(Artifacts::default());
    save_artifact(&old, "title = \"Old\"").unwrap();
    save_artifact(&now, "title = \"New\"").unwrap();
    let chat_url = store.lock().unwrap().register("title = \"New\"", None);
    let listed = saved_artifacts(
        Some(now.clone()),
        [(old, None), (now, None), (empty, None)].into_iter(),
        &store,
    );
    let sessions = listed["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0]["current"], true);
    assert_eq!(sessions[0]["items"][0]["title"], "New");
    assert_eq!(sessions[0]["items"][0]["url"], chat_url.as_str());
    assert_eq!(
        sessions[0]["items"][0]["hash"],
        content_hash("title = \"New\"")
    );
    assert_eq!(sessions[1]["items"][0]["title"], "Old");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_page_checking_its_connection_hears_back() {
    let server = Server::start(false).await.unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let id = server.shared.add_client(tx, true, None);
    while rx.try_recv().is_ok() {}
    handle(&server.shared, id, json!({ "t": "ping" }));
    let Some(Out::Text(pong)) = rx.recv().await else {
        panic!("no answer")
    };
    assert_eq!(
        serde_json::from_str::<Value>(&pong).unwrap(),
        json!({ "t": "pong" })
    );
}

#[tokio::test]
async fn a_terminal_says_what_it_is_and_gets_ordinary_replies() {
    let mut server = Server::start(false).await.unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let id = server.shared.add_client(tx, true, None);
    assert!(matches!(
        server.events.recv().await,
        Some(WebEvent::Connected(_))
    ));
    assert_eq!((server.remote_tabs(), server.web_remote_tabs()), (1, 1));
    handle(&server.shared, id, json!({ "t": "send", "text": "a" }));
    assert!(matches!(server.events.recv().await, Some(WebEvent::Send(t)) if t == "a"));

    // The model picker's list comes from the session, back to this tab alone.
    handle(&server.shared, id, json!({ "t": "models", "seq": 4 }));
    assert!(matches!(
        server.events.recv().await,
        Some(WebEvent::Models { tab, seq: 4 }) if tab == id
    ));

    handle(&server.shared, id, json!({ "t": "terminal" }));
    assert_eq!((server.remote_tabs(), server.web_remote_tabs()), (1, 0));
    handle(&server.shared, id, json!({ "t": "send", "text": "b" }));
    assert!(matches!(server.events.recv().await, Some(WebEvent::TerminalSend(t)) if t == "b"));
}

#[tokio::test]
async fn a_tab_asks_for_the_changes_and_for_just_their_counts() {
    let repo = std::env::temp_dir().join(format!("tamale-web-diff-{}", crate::session::new_id()));
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    let git = |args: &[&str]| {
        let done = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(done.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "first",
    ]);
    std::fs::write(repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();

    let server = Server::start(false).await.unwrap();
    *server.shared.roots.lock().unwrap() = vec![repo.clone()];
    let (tx, mut rx) = mpsc::unbounded_channel();
    let id = server.shared.add_client(tx, false, None);
    let mut reply = async |request: Value| -> Value {
        handle(&server.shared, id, request);
        loop {
            if let Some(Out::Text(text)) = rx.recv().await
                && let Ok(value) = serde_json::from_str::<Value>(&text)
                && value.get("seq").is_some()
            {
                return value;
            }
        }
    };

    let stat = reply(json!({ "t": "diff_stat", "seq": 1 })).await;
    assert_eq!(stat["t"], "diff_stat");
    assert_eq!(
        (&stat["files"], &stat["added"], &stat["removed"]),
        (&json!(1), &json!(2), &json!(1))
    );
    assert_eq!(stat["seq"], 1);
    let diff = reply(json!({ "t": "diff", "seq": 2 })).await;
    assert_eq!(diff["files"][0]["path"], "a.txt");
    assert_eq!(diff["seq"], 2);
    let _ = std::fs::remove_dir_all(repo);
}

#[tokio::test]
async fn the_share_link_is_the_hubs_while_connected_to_one() {
    let mut server = Server::start(false).await.unwrap();
    let local = server.url.clone();
    assert_eq!(server.share_url(), local);
    let label = relay::Label {
        host: "h".into(),
        cwd: "c".into(),
        model: "m".into(),
        ..Default::default()
    };
    // Nothing listens there: the link doesn't wait for the connection.
    server.connect_hub("ws://127.0.0.1:1", "token", label, None);
    assert_eq!(
        server.share_url(),
        relay::open_url("ws://127.0.0.1:1", relay::sid())
    );
    assert!(server.disconnect_hub());
    assert_eq!(server.share_url(), local);
}

#[tokio::test]
async fn artifacts_are_served_as_data_to_this_host_only() {
    let mut server = Server::start(false).await.unwrap();
    let entries = vec![Entry::Assistant(
        "```openui\ntitle = \"T</script><script>1\"\nroot = Card([])\n```".into(),
    )];
    server.sync(view(&entries));
    let id = server
        .shared
        .artifacts
        .lock()
        .unwrap()
        .sources
        .keys()
        .next()
        .unwrap()
        .clone();
    let base = server.url.split("/?").next().unwrap().to_string();
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{base}/artifact/{id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
        .to_str()
        .unwrap();
    // Only the app's own script runs; the document is data in the page.
    assert!(
        csp.contains("script-src 'self' 'unsafe-eval' 'wasm-unsafe-eval';"),
        "{csp}"
    );
    assert!(!csp.contains("sandbox"), "{csp}");
    let page = response.text().await.unwrap();
    assert!(
        page.contains("<script type=\"application/json\" id=\"openui-artifact\">"),
        "{page}"
    );
    assert!(!page.contains("<script>1"), "{page}");
    assert!(
        page.contains("<title>T&lt;/script&gt;&lt;script&gt;1</title>"),
        "{page}"
    );
    // The viewer that draws it comes from this server too.
    assert!(page.contains("<script type=\"module\" src=\"/openui.js\">"));
    for path in ["/openui.js", "/openui_bg.wasm"] {
        let got = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(got.status(), 200, "{path}");
    }

    let missing = client
        .get(format!("{base}/artifact/nope"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let rebound = client
        .get(format!("{base}/artifact/{id}"))
        .header(header::HOST, "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);
}

#[tokio::test]
async fn on_the_network_this_machine_gets_a_loopback_address_too() {
    // Off any network there's no LAN server to test.
    let Ok(server) = Server::start(true).await else {
        return;
    };
    assert_ne!(server.local_url, server.url);
    assert!(server.local_url.starts_with("http://127.0.0.1:"));
    assert_eq!(server.open_url(), server.local_url);
    assert_eq!(server.share_url(), server.url);
    let client = reqwest::Client::new();
    for url in [&server.url, &server.local_url] {
        assert_eq!(client.get(url).send().await.unwrap().status(), 200, "{url}");
        let base = url.split("/?").next().unwrap();
        let script = client.get(format!("{base}/app.js")).send().await.unwrap();
        assert_eq!(script.status(), 200, "{url}");
    }
    // Only this server's own addresses.
    let rebound = client
        .get(&server.local_url)
        .header(header::HOST, "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);
}

#[tokio::test]
async fn relayed_tabs_get_the_same_session_and_pages_as_local_ones() {
    use tamale_hub_protocol::{ToAgent, ToHub};
    // A stand-in hub: welcomes the agent, opens one channel, asks for the page.
    let (frames_tx, mut frames) = mpsc::unbounded_channel::<ToHub>();
    let (auth_tx, mut auth) = mpsc::unbounded_channel::<String>();
    let app = Router::new().route(
        "/agent",
        get(move |headers: HeaderMap, upgrade: WebSocketUpgrade| {
            let (frames_tx, auth_tx) = (frames_tx.clone(), auth_tx.clone());
            async move {
                let _ = auth_tx.send(headers[header::AUTHORIZATION].to_str().unwrap().to_string());
                upgrade.on_upgrade(move |mut ws| async move {
                    let send =
                        |f: &ToAgent| Message::Text(serde_json::to_string(f).unwrap().into());
                    let Some(Ok(Message::Text(hello))) = ws.next().await else {
                        return;
                    };
                    let _ = frames_tx.send(serde_json::from_str(&hello).unwrap());
                    ws.send(send(&ToAgent::Welcome { proto: 1 })).await.unwrap();
                    ws.send(send(&ToAgent::Open { c: 7 })).await.unwrap();
                    ws.send(send(&ToAgent::Get {
                        r: 1,
                        path: "/".into(),
                    }))
                    .await
                    .unwrap();
                    ws.send(send(&ToAgent::Get {
                        r: 2,
                        path: "/ws".into(),
                    }))
                    .await
                    .unwrap();
                    ws.send(send(&ToAgent::Get {
                        r: 3,
                        path: "/app_bg.wasm".into(),
                    }))
                    .await
                    .unwrap();
                    while let Some(Ok(Message::Text(t))) = ws.next().await {
                        let _ = frames_tx.send(serde_json::from_str(&t).unwrap());
                    }
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });

    let mut server = Server::start(false).await.unwrap();
    let entries = vec![Entry::User("Fix the flaky test".into())];
    server.sync(view(&entries));
    server.connect_hub(&hub, "f00d", Label::default(), None);
    assert_eq!(auth.recv().await.unwrap(), "Bearer f00d");
    let next = async |frames: &mut mpsc::UnboundedReceiver<ToHub>| {
        tokio::time::timeout(std::time::Duration::from_secs(5), frames.recv())
            .await
            .unwrap()
            .unwrap()
    };
    let ToHub::Hello { sid, .. } = next(&mut frames).await else {
        panic!("no hello")
    };
    assert_eq!(sid, relay::sid());

    // The channel registers as a remote tab, which the app welcomes as it would a local one.
    let id = loop {
        if let Some(WebEvent::Connected(id)) =
            tokio::time::timeout(std::time::Duration::from_secs(5), server.events.recv())
                .await
                .unwrap()
        {
            break id;
        }
    };
    assert_eq!((server.tabs(), server.remote_tabs()), (1, 1));
    server.welcome(id);

    let mut got_page = false;
    let mut got_404 = false;
    let mut got_module = false;
    let mut got_entries = false;
    let mut got_status = false;
    while !(got_page && got_404 && got_module && got_entries && got_status) {
        match next(&mut frames).await {
            ToHub::Got {
                r: 3,
                status,
                headers,
                body,
                base64,
            } => {
                // The page's module: bytes, so base64, and gzipped.
                use base64::Engine;
                assert_eq!(status, 200);
                assert!(base64);
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(body)
                    .unwrap();
                assert_eq!(bytes, super::MODULE);
                assert!(
                    headers
                        .iter()
                        .any(|(k, v)| k == "content-encoding" && v == "gzip")
                );
                got_module = true;
            }
            ToHub::Got {
                r: 1,
                status,
                headers,
                body,
                ..
            } => {
                assert_eq!(status, 200);
                assert!(body.contains("<script type=\"module\" src=\"/app.js\">"));
                assert!(
                    headers
                        .iter()
                        .any(|(k, v)| k == "content-security-policy" && v == PAGE_CSP)
                );
                got_page = true;
            }
            ToHub::Got { r: 2, status, .. } => {
                assert_eq!(status, 404, "only page paths are served");
                got_404 = true;
            }
            ToHub::Msg { c: 7, data } if data.contains("\"t\":\"entries\"") => {
                assert!(data.contains("Fix the flaky test"));
                got_entries = true;
            }
            ToHub::Status { title, .. } => {
                assert_eq!(title, "Fix the flaky test");
                got_status = true;
            }
            _ => {}
        }
    }
    assert!(matches!(server.hub_state(), Some(HubState::Live(_))));
    assert!(
        server
            .hub_url()
            .unwrap()
            .ends_with(&format!("/open/{}", relay::sid()))
    );
    assert!(server.disconnect_hub());
    assert_eq!(server.remote_tabs(), 0);
}

/// A stand-in hub for tests of the relay: frames the test sends go to the agent, and
/// what the agent sends comes back.
async fn stand_in_hub() -> (
    String,
    mpsc::UnboundedSender<tamale_hub_protocol::ToAgent>,
    mpsc::UnboundedReceiver<String>,
) {
    use tamale_hub_protocol::ToAgent;
    let (to_agent, from_test) = mpsc::unbounded_channel::<ToAgent>();
    let (to_test, from_agent) = mpsc::unbounded_channel::<String>();
    let from_test = Arc::new(tokio::sync::Mutex::new(from_test));
    let app = Router::new().route(
        "/agent",
        get(move |upgrade: WebSocketUpgrade| {
            let (from_test, to_test) = (from_test.clone(), to_test.clone());
            async move {
                upgrade.max_message_size(64 << 20).on_upgrade(move |mut ws| async move {
                    let Some(Ok(Message::Text(hello))) = ws.next().await else { return };
                    let _ = to_test.send(hello.to_string());
                    let welcome = serde_json::to_string(&ToAgent::Welcome { proto: 2 }).unwrap();
                    ws.send(Message::Text(welcome.into())).await.unwrap();
                    let mut from_test = from_test.lock().await;
                    loop {
                        tokio::select! {
                            Some(frame) = from_test.recv() => {
                                let text = serde_json::to_string(&frame).unwrap();
                                ws.send(Message::Text(text.into())).await.unwrap();
                            }
                            message = ws.next() => match message {
                                Some(Ok(Message::Text(t))) => { let _ = to_test.send(t.to_string()); }
                                Some(Ok(_)) => {}
                                _ => return,
                            },
                        }
                    }
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (hub, to_agent, from_agent)
}

#[tokio::test]
async fn a_dropped_hub_connection_comes_back_without_a_word() {
    use std::time::Duration;
    use tamale_hub_protocol::ToAgent;
    let (to_test, mut hellos) = mpsc::unbounded_channel::<()>();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let app = Router::new().route(
        "/agent",
        get(move |upgrade: WebSocketUpgrade| {
            let to_test = to_test.clone();
            let first = count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0;
            async move {
                upgrade.on_upgrade(move |mut ws| async move {
                    let Some(Ok(Message::Text(_))) = ws.next().await else {
                        return;
                    };
                    let welcome = serde_json::to_string(&ToAgent::Welcome { proto: 2 }).unwrap();
                    ws.send(Message::Text(welcome.into())).await.unwrap();
                    let _ = to_test.send(());
                    // The first connection just drops, as a sleeping laptop's does.
                    if first {
                        drop(ws);
                    } else {
                        std::future::pending::<()>().await;
                    }
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    let mut server = Server::start(false).await.unwrap();
    server.connect_hub(&hub, "f00d", Label::default(), None);
    for _ in 0..2 {
        tokio::time::timeout(Duration::from_secs(5), hellos.recv())
            .await
            .unwrap()
            .unwrap();
    }
    // Long enough for the second connection's welcome to be acted on.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut said = Vec::new();
    while let Ok(event) = server.events.try_recv() {
        if let WebEvent::Hub(result) = event {
            said.push(result);
        }
    }
    assert!(matches!(&said[..], [Ok(_)]), "{said:?}");
    assert!(matches!(server.hub_state(), Some(HubState::Live(_))));
}

#[tokio::test]
async fn a_chef_says_so_in_the_clear_and_hears_when_the_key_has_one() {
    use std::time::Duration;
    use tamale_hub_protocol::{ToHub, close};
    let (to_test, mut hellos) = mpsc::unbounded_channel::<String>();
    let app = Router::new().route(
        "/agent",
        get(move |upgrade: WebSocketUpgrade| {
            let to_test = to_test.clone();
            async move {
                upgrade.on_upgrade(move |mut ws| async move {
                    let Some(Ok(Message::Text(hello))) = ws.next().await else {
                        return;
                    };
                    let _ = to_test.send(hello.to_string());
                    let refusal = axum::extract::ws::CloseFrame {
                        code: close::CHEF_TAKEN,
                        reason: "this key already has a tamale chef running".into(),
                    };
                    let _ = ws.send(Message::Close(Some(refusal))).await;
                })
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub = format!("ws://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });

    let mut server = Server::start(false).await.unwrap();
    let label = Label {
        host: "laptop".into(),
        chef: true,
        ..Default::default()
    };
    server.connect_hub(&hub, "f00d", label, Some(e2e::Keys::from_psk([3u8; 32])));
    let hello = tokio::time::timeout(Duration::from_secs(5), hellos.recv())
        .await
        .unwrap()
        .unwrap();
    let ToHub::Hello {
        chef,
        label,
        sealed,
        ..
    } = serde_json::from_str(&hello).unwrap()
    else {
        panic!("no hello")
    };
    assert!(chef, "{hello}");
    assert!(
        label.host.is_empty() && sealed.is_some(),
        "the rest stays sealed: {hello}"
    );
    loop {
        match tokio::time::timeout(Duration::from_secs(5), server.events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            WebEvent::ChefTaken(why) => {
                assert!(why.contains("already has a tamale chef"), "{why}");
                break;
            }
            WebEvent::Hub(Err(e)) => panic!("reported as a plain problem: {e}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_tab_can_close_the_session_and_the_hub_hears_it_ended() {
    use std::time::Duration;
    use tamale_hub_protocol::ToAgent;
    let (hub, to_agent, mut frames) = stand_in_hub().await;
    let mut server = Server::start(false).await.unwrap();
    server.connect_hub(&hub, "f00d", Label::default(), None);
    let mut next_event = async || {
        tokio::time::timeout(Duration::from_secs(5), server.events.recv())
            .await
            .unwrap()
            .unwrap()
    };
    loop {
        if let WebEvent::Hub(Ok(_)) = next_event().await {
            break;
        }
    }
    to_agent.send(ToAgent::Open { c: 1 }).unwrap();
    let close = r#"{"t":"close"}"#.to_string();
    to_agent.send(ToAgent::Msg { c: 1, data: close }).unwrap();
    loop {
        if let WebEvent::Close = next_event().await {
            break;
        }
    }

    tokio::time::timeout(Duration::from_secs(5), server.end())
        .await
        .unwrap();
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), frames.recv())
            .await
            .unwrap()
            .unwrap();
        if frame == r#"{"t":"end"}"# {
            break;
        }
    }
    assert!(server.hub_url().is_none());
}

#[tokio::test]
async fn an_encrypted_relay_shows_the_hub_nothing_and_takes_nothing_from_it() {
    use super::e2e::testing::Initiator;
    use std::time::Duration;
    use tamale_hub_protocol::{ToAgent, ToHub};
    const SECRET: &str = "Fix-the-flaky-test-SECRET";
    const LABEL: &str = "SECRET-CWD";
    let psk = [9u8; 32];
    let (hub, to_agent, mut frames) = stand_in_hub().await;
    let mut server = Server::start(false).await.unwrap();
    let entries = vec![
        Entry::User(SECRET.into()),
        Entry::Assistant(format!(
            "```openui\nroot = Card([TextContent(\"{SECRET}\")])\n```"
        )),
    ];
    server.sync(view(&entries));
    let keys = e2e::Keys::from_psk(psk);
    server.connect_hub(
        &hub,
        "f00d",
        Label {
            host: "laptop".into(),
            cwd: LABEL.into(),
            model: "m".into(),
            ..Default::default()
        },
        Some(keys.clone()),
    );
    // The link shown on the terminal has nothing secret in it.
    assert!(
        server
            .hub_url()
            .unwrap()
            .ends_with(&format!("/open/{}", relay::sid()))
    );

    // Everything the hub is sent, checked as it passes.
    let next = async |frames: &mut mpsc::UnboundedReceiver<String>| -> ToHub {
        let text = tokio::time::timeout(Duration::from_secs(5), frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !text.contains(SECRET) && !text.contains(LABEL) && !text.contains("laptop"),
            "{text}"
        );
        serde_json::from_str(&text).unwrap()
    };
    let ToHub::Hello {
        mid,
        e2e: on,
        sealed,
        label,
        ..
    } = next(&mut frames).await
    else {
        panic!("no hello")
    };
    assert_eq!((mid.as_deref(), on), (Some(keys.mid.as_str()), true));
    assert!(sealed.is_some());
    assert_eq!(label, Label::default());
    let ToHub::Status { title, sealed, .. } = next(&mut frames).await else {
        panic!("no status")
    };
    assert!(title.is_empty() && sealed.is_some());

    // Frames until channel `c` says something, or closes.
    async fn answer(frames: &mut mpsc::UnboundedReceiver<String>, c: u64) -> Result<Value, String> {
        loop {
            let text = tokio::time::timeout(Duration::from_secs(5), frames.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(!text.contains(SECRET), "{text}");
            match serde_json::from_str::<ToHub>(&text).unwrap() {
                ToHub::Msg { c: from, data } if from == c => {
                    return Ok(serde_json::from_str(&data).unwrap());
                }
                ToHub::Close { c: from, reason } if from == c => return Err(reason),
                _ => {}
            }
        }
    }
    let nonce = |v: &Value| {
        use base64::Engine;
        assert_eq!(v["t"], "hs0");
        base64::engine::general_purpose::STANDARD
            .decode(v["n"].as_str().unwrap())
            .unwrap()
    };
    let msg = |c, data: String| ToAgent::Msg { c, data };

    // A hub without the key, typing a prompt or approving a tool, is refused.
    for (c, forged) in [
        (1, json!({ "t": "send", "text": "rm -rf /" })),
        (2, json!({ "t": "approve", "choice": "yes" })),
        (3, json!({ "t": "box", "m": "AAAA" })),
    ] {
        to_agent.send(ToAgent::Open { c }).unwrap();
        nonce(&answer(&mut frames, c).await.unwrap());
        to_agent.send(msg(c, forged.to_string())).unwrap();
        assert!(answer(&mut frames, c).await.is_err());
    }

    // Another machine's key fails the handshake.
    to_agent.send(ToAgent::Open { c: 4 }).unwrap();
    let n = nonce(&answer(&mut frames, 4).await.unwrap());
    let hs1 = Initiator::new(&[1u8; 32], relay::sid(), &n, None).hs1(b"{}");
    to_agent
        .send(msg(4, json!({ "t": "hs1", "m": hs1 }).to_string()))
        .unwrap();
    assert_eq!(answer(&mut frames, 4).await.unwrap_err(), "e2e: wrong key");
    assert_eq!(server.remote_tabs(), 0, "no tab has been made");

    // The right key, on channel 8.
    to_agent.send(ToAgent::Open { c: 8 }).unwrap();
    let n = nonce(&answer(&mut frames, 8).await.unwrap());
    let mut browser = Initiator::new(&psk, relay::sid(), &n, None);
    let hs1 = browser.hs1(b"{}");
    to_agent
        .send(msg(8, json!({ "t": "hs1", "m": hs1 }).to_string()))
        .unwrap();
    let hs2 = answer(&mut frames, 8).await.unwrap();
    assert_eq!(hs2["t"], "hs2");
    let mut browser = browser.finish(hs2["m"].as_str().unwrap()).unwrap();

    // Only now is it a tab, and what it's sent is sealed.
    let id = loop {
        let event = tokio::time::timeout(Duration::from_secs(5), server.events.recv())
            .await
            .unwrap();
        match event {
            Some(WebEvent::Connected(id)) => break id,
            Some(WebEvent::Send(_) | WebEvent::Approve(..) | WebEvent::Answer(_)) => {
                panic!("a forged message got through")
            }
            _ => {}
        }
    };
    server.welcome(id);
    let mut opened = |v: Value| browser.open(v["m"].as_str().unwrap(), 64 << 20).unwrap();
    let first = answer(&mut frames, 8).await.unwrap();
    assert_eq!(first["t"], "box");
    let welcome = opened(first).unwrap();
    assert!(welcome.contains(SECRET) && welcome.contains("\"t\":\"entries\""));
    let artifact_id = {
        let artifacts = server.shared.artifacts.lock().unwrap();
        artifacts.sources.keys().next().unwrap().clone()
    };
    let _ = opened(answer(&mut frames, 8).await.unwrap()); // status
    let _ = opened(answer(&mut frames, 8).await.unwrap()); // approval
    let _ = opened(answer(&mut frames, 8).await.unwrap()); // questions

    // The hub can't tunnel an artifact.
    to_agent
        .send(ToAgent::Get {
            r: 1,
            path: format!("/artifact/{artifact_id}"),
        })
        .unwrap();
    loop {
        if let ToHub::Got {
            r: 1, status, body, ..
        } = next(&mut frames).await
        {
            assert_eq!(status, 404);
            assert!(body.is_empty());
            break;
        }
    }

    // Asked over the sealed socket, a long artifact arrives whole.
    let send_sealed = |browser: &mut e2e::Channel, c: u64, text: String| -> Vec<String> {
        let boxes = browser.seal(&text).unwrap();
        for m in &boxes {
            to_agent
                .send(msg(c, json!({ "t": "box", "m": m }).to_string()))
                .unwrap();
        }
        boxes
    };
    let up = send_sealed(
        &mut browser,
        8,
        json!({ "t": "artifact", "id": artifact_id }).to_string(),
    );
    // With the viewer in its page, it comes in several pieces.
    let reply: Value = loop {
        let frame = answer(&mut frames, 8).await.unwrap();
        if let Some(text) = browser
            .open(frame["m"].as_str().unwrap(), 64 << 20)
            .unwrap()
        {
            break serde_json::from_str(&text).unwrap();
        }
    };
    assert_eq!(reply["t"], "artifact");
    assert!(reply["source"].as_str().unwrap().contains(SECRET));
    assert!(reply["html"].as_str().unwrap().contains("openui-module"));

    // A genuine prompt gets through, in several pieces if need be.
    let long = "p".repeat(200_000);
    send_sealed(
        &mut browser,
        8,
        json!({ "t": "send", "text": long }).to_string(),
    );
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), server.events.recv())
            .await
            .unwrap();
        if let Some(WebEvent::Send(text)) = event {
            assert_eq!(text, long);
            break;
        }
    }

    // A replay closes the channel, and the tab with it.
    to_agent
        .send(msg(8, json!({ "t": "box", "m": up[0] }).to_string()))
        .unwrap();
    assert!(answer(&mut frames, 8).await.is_err());
    assert_eq!(server.remote_tabs(), 0);

    // A handshake recorded on channel 8, replayed on channel 9, fails.
    to_agent.send(ToAgent::Open { c: 9 }).unwrap();
    nonce(&answer(&mut frames, 9).await.unwrap());
    to_agent
        .send(msg(9, json!({ "t": "hs1", "m": hs1 }).to_string()))
        .unwrap();
    assert_eq!(answer(&mut frames, 9).await.unwrap_err(), "e2e: wrong key");
}

/// Against a real hub: `TAMALE_HUB_URL` and `TAMALE_HUB_KEY` set, then
/// `cargo test real_hub -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_hub_relays_a_browser_end_to_end() {
    use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};
    let env = |k| std::env::var(k).unwrap_or_else(|_| panic!("{k} not set"));
    let hub = env("TAMALE_HUB_URL");
    let key = tamale_hub_protocol::decode_key(&env("TAMALE_HUB_KEY")).expect("a key");
    let token = tamale_hub_protocol::auth_value(&key);
    let origin = hub.replacen("wss://", "https://", 1);
    let mut server = Server::start(false).await.unwrap();
    let entries = vec![Entry::User("Remote smoke test".into())];
    server.sync(view(&entries));
    server.connect_hub(
        &hub,
        &token,
        Label {
            host: "laptop".into(),
            cwd: "~/dev/tamale".into(),
            model: "test".into(),
            ..Default::default()
        },
        None,
    );
    let deadline = std::time::Duration::from_secs(15);
    loop {
        match tokio::time::timeout(deadline, server.events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            WebEvent::Hub(Ok(_)) => break,
            WebEvent::Hub(Err(e)) => panic!("{e}"),
            _ => {}
        }
    }
    let sid = relay::sid().to_string();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let cookie_of = |r: &reqwest::Response| {
        r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    };
    let r = http
        .post(format!("{origin}/api/tokens"))
        .header("origin", &origin)
        .json(&json!({ "add": token }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let th = cookie_of(&r);
    assert!(th.starts_with("__Host-th="));
    let list: Value = http
        .get(format!("{origin}/api/sessions"))
        .header("cookie", &th)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["sid"] == sid.as_str()),
        "{list}"
    );
    let ticket: Value = http
        .post(format!("{origin}/api/ticket"))
        .header("origin", &origin)
        .header("cookie", &th)
        .json(&json!({ "sid": sid }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let r = http
        .get(ticket["url"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 303);
    let ts = cookie_of(&r);
    let session = origin.replacen("https://", &format!("https://{sid}."), 1);
    let page = http
        .get(format!("{session}/"))
        .header("cookie", &ts)
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), 200);
    assert_eq!(page.headers()[header::CONTENT_SECURITY_POLICY], PAGE_CSP);
    assert!(page.text().await.unwrap().contains("/app.js"));

    let mut request = format!("{}/ws", session.replacen("https://", "wss://", 1))
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", session.parse().unwrap());
    request.headers_mut().insert("cookie", ts.parse().unwrap());
    let (mut tab, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let id = loop {
        match tokio::time::timeout(deadline, server.events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            WebEvent::Connected(id) => break id,
            _ => {}
        }
    };
    server.welcome(id);
    loop {
        let Some(Ok(tungstenite::Message::Text(t))) =
            tokio::time::timeout(deadline, tab.next()).await.unwrap()
        else {
            panic!("tab closed")
        };
        if t.contains("Remote smoke test") {
            break;
        }
    }
    tab.send(tungstenite::Message::Text(
        r#"{"t":"send","text":"hello through the hub"}"#.into(),
    ))
    .await
    .unwrap();
    loop {
        match tokio::time::timeout(deadline, server.events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            WebEvent::Send(text) => {
                assert_eq!(text, "hello through the hub");
                break;
            }
            _ => {}
        }
    }
    assert_eq!(server.remote_tabs(), 1);
}

/// What the stand-in session answers "long" with: taller than the screen, with a background
/// of its own and a sticky header, as a real artifact has.
const LONG_ARTIFACT: &str = r#"title = "Launch checklist"
root = Card([header, progress, steps, done])
header = CardHeader("Launch checklist", "Fourteen steps, each with an owner and a command")
progress = Progress(3, 14, "Steps done", "good")
steps = Steps(@Each(items, "i", StepsItem(
  "Step " + i.n,
  Stack([
    TextContent("Everything that has to be true before step " + i.n + " is done, in enough words to wrap across a few lines on a phone."),
    Table([Col("Owner", ["Sam"]), Col("Due", ["Mar " + i.n])]),
    CodeBlock("sh", "make step-" + i.n)
  ]),
  i.n <= 3 ? "done" : i.n == 4 ? "active" : "todo"
)))
items = [
  {n: 1},
  {n: 2},
  {n: 3},
  {n: 4},
  {n: 5},
  {n: 6},
  {n: 7},
  {n: 8},
  {n: 9},
  {n: 10},
  {n: 11},
  {n: 12},
  {n: 13},
  {n: 14},
]
done = Callout("success", "The end of the page", "Everything above is in order.")"#;

/// An artifact with most of the components in it, for looking at (TAMALE_FAKE_ARTIFACT).
const SHOWCASE_ARTIFACT: &str = r#"title = "Inline cap"
root = Card([header, stats, flow, tabs, files, more, followUps])
header = CardHeader("Files up to 512 KB are sent whole", "Bigger ones go in 64 KB pieces, at most 37 of them: `src/web/pullspace.rs`")
stats = Grid([Metric("512 KB", "Sent whole", "+384 KB", "good"), Metric("37", "Pieces at most", "−5", "good"), Metric("2.3 s", "Worst load on 3G", "+0.4 s", "bad")], 160)
flow = Flow([FlowNode("Tab asks", "pullspace::Link"), FlowNode("Size check", "inline_cap()", "accent"), FlowNode("Whole file", "≤ 512 KB", "success"), FlowNode("Pieces", "64 KB × n", "warning")])
tabs = Tabs([TabItem("why", "Why", why), TabItem("diff", "Diff", diff), TabItem("numbers", "Numbers", numbers)])
why = Stack([Callout("danger", "Root cause", "The cap was **128 KB**, so most source files went in pieces and each piece was a round trip."), Markdown("Raising it:\n\n- cuts round trips for 92% of files\n- keeps the biggest message under the socket's limit\n\n> Measured over this repository's 1,204 files.")])
diff = Diff("src/web/pullspace.rs", "@@ -40,7 +40,7 @@ impl Link {\n /// Files up to this size are sent in one message.\n-const INLINE_CAP: usize = 128 * 1024;\n+const INLINE_CAP: usize = 512 * 1024;\n const PIECE: usize = 64 * 1024;\n const MAX_PIECES: usize = 37;", "Four times the cap: one message for nearly every file")
numbers = Stack([SegmentedControl("chart", [SelectItem("bars", "By size"), SelectItem("line", "Over time"), SelectItem("pie", "Share")], $chart), shown])
$chart = "bars"
shown = $chart == "bars" ? bars : $chart == "line" ? line : pie
bars = BarChart(["<64K", "64–128K", "128–512K", ">512K"], [Series("Before", [3, 4, 9, 9]), Series("After", [1, 1, 1, 9])], "grouped", "File size", "Round trips")
line = LineChart(["Mon", "Tue", "Wed", "Thu", "Fri"], [Series("p95 load (ms)", [820, 790, 410, 380, 372])])
pie = PieChart(["Whole", "In pieces"], [1108, 96], "donut")
files = Accordion([AccordionItem("files", "Files touched (" + @Count(changed) + ")", Table([Col("File", @Each(changed, "f", f.path), "code"), Col("Lines", @Each(changed, "f", f.lines), "number"), Col("Change", @Each(changed, "f", f.kind), "tag")]))])
changed = [{path: "src/web/pullspace.rs", lines: 2, kind: "cap"}, {path: "web/src/review.rs", lines: 14, kind: "pieces"}, {path: "src/web/tests.rs", lines: 31, kind: "tests"}]
more = Steps([StepsItem("Raise the cap", "Done in `pullspace.rs`.", "done"), StepsItem("Check the hub's limit", Progress(48, 64, "MB of the hub's 64 MB frame"), "active"), StepsItem("Ship", null, "todo")])
followUps = FollowUpBlock([FollowUpItem("Apply it to the browser mirror too"), FollowUpItem("How was this measured?")])"#;

/// A lesson, as the lesson button would get one: from the simplest case up to the change.
const LESSON_ARTIFACT: &str = r#"title = "How the page gets a file"
root = Lesson("Why big files now come in 64 KB pieces, and why that makes the page faster", [p1, p2, p3, p4])
p1 = LessonPart("Asking for a file", "The simplest case", [TextContent("When you open a file in the code view, the page in your browser doesn't have it yet. It **asks** tamale for it, and tamale, on your computer, reads the file and sends the text back. Like asking a librarian for a book: you ask, they fetch it, you read."), Flow([FlowNode("You open main.rs", "the page"), FlowNode("Ask for it", "a message", "accent"), FlowNode("Read it", "tamale"), FlowNode("Show it", "the page", "success")]), Callout("info", "Remember", "One ask, one answer: the whole file at once.")], [Check("If the file were 50 MB, what would you notice while it's sent whole?", "Think about what the page can do while it waits.", "Nothing shows until the whole file has arrived; on a slow phone connection that can be many seconds, and one huge message can be refused or time out."), QuickCheck("Where does the file's text come from?", ["The browser's cache", "tamale reading it on your computer", "GitHub"], 1, "The page has no files of its own: tamale reads them where they live.")])
p2 = LessonPart("A size limit", "+ A cap", [TextContent("Sending everything at once breaks down for big files. So tamale has a **cap**: files up to a size are sent whole, bigger ones aren't."), CodeBlock("rust", "pub const MAX_INLINE: u64 = 512 * 1024;", "src/web/pullspace.rs")], [Check("Why have a cap at all, instead of always sending files whole?", "What did part 1's question show?", "Whole files are simplest and fastest when small; past some size, one message gets too slow and too big, so there has to be a line.")])
p3 = LessonPart("Pieces", "Where main is today", [TextContent("Past the cap, a file is sent in **pieces** of 64 KB, which the page puts back together. The page can show the first piece while the rest arrive.")], [Check("A 200 KB file: how many pieces, and why might that be slower than sending it whole?", null, "Four pieces (64 KB each, the last one partly full): four round trips instead of one, each paying the network's delay.")])
p4 = LessonPart("Raising the cap", "The change", [TextContent("Part 3's answer is the whole story: small files paid for pieces they didn't need. The branch raises the cap from 128 KB to **512 KB**, so most files go back to the simplest case."), Diff("src/web/pullspace.rs", "@@ -38,1 +38,1 @@\n-pub const MAX_INLINE: u64 = 128 * 1024;\n+pub const MAX_INLINE: u64 = 512 * 1024;")], [Check("In terms of part 1, what did this change do for a 300 KB file?")])"#;

/// A chef's page with a sample kitchen and conversation, served on this machine until
/// `TAMALE_FAKE_SECONDS` (default 60) pass, for looking at: prints `URL=<url>`.
/// `cargo test -p tamale fake_chef_page -- --ignored --nocapture`
#[tokio::test]
#[ignore]
async fn fake_chef_page() {
    let seconds: u64 = std::env::var("TAMALE_FAKE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let empty = std::env::var_os("TAMALE_FAKE_EMPTY").is_some();
    let mut server = Server::start(false).await.unwrap();
    println!("URL={}", server.url);
    let mut entries = vec![Entry::Info(
        "Welcome! Type a request and press Enter. /help for commands.".into(),
    )];
    if !empty {
        entries.extend([
            Entry::User("What is everyone working on?".into()),
            Entry::Tool {
                name: "list_sessions".into(),
                summary: String::new(),
                description: "Look around the kitchen".into(),
                output: Some(("3 sessions on the hub".into(), false)),
            },
            Entry::Tool {
                name: "ask_sessions".into(),
                summary: "a1b2c3, d4e5f6 → What are you working on, and what's left?".into(),
                description: "Ask the two busy sessions what's left".into(),
                output: Some(("## laptop:~/dev/api [a1b2c3]\n…".into(), false)),
            },
            Entry::Assistant(
                "Three sessions are cooking. **One needs you.**\n\n\
                 - **`laptop:~/dev/tamale`** (0a9b8c) is waiting for approval to run \
                 `cargo publish`. Worth a look before you say yes.\n\
                 - **`laptop:~/dev/api`** (a1b2c3) is adding rate limits: middleware done, \
                 two tests still failing in `limits_test.rs`. It expects ~10 more minutes.\n\
                 - **`vps-1:~/web`** (d4e5f6) finished the login fix and is idle, waiting \
                 for review.\n\n\
                 > [!NOTE]\n> `api` and `web` both touched `auth/session.rs`; expect a \
                 merge conflict."
                    .into(),
            ),
        ]);
    }
    let session = |id: &str, host: &str, cwd: &str, title: &str, state, viewers| KitchenSession {
        id: id.into(),
        host: host.into(),
        cwd: cwd.into(),
        title: title.into(),
        model: "claude-opus-5-5".into(),
        state,
        viewers,
    };
    let kitchen = Kitchen {
        sessions: vec![
            session(
                "0a9b8c",
                "laptop",
                "~/dev/tamale",
                "release 0.1.7 to crates.io",
                "approval",
                0,
            ),
            session(
                "a1b2c3",
                "laptop",
                "~/dev/api",
                "add rate limits to the public API and make the tests pass",
                "working",
                1,
            ),
            session(
                "d4e5f6",
                "vps-1",
                "~/web",
                "fix the login page redirect loop",
                "idle",
                0,
            ),
            session("77aa11", "mini", "~/dev/old-thing", "", "offline", 0),
        ],
        problem: None,
        loading: false,
    };
    fn chef_view<'a>(entries: &'a [Entry], kitchen: &Kitchen) -> View<'a> {
        View {
            status: Status {
                project: "chef".into(),
                cwd: "~".into(),
                model: "Claude Opus 5.5".into(),
                backend: "Anthropic API".into(),
                signed_in: true,
                mode: "ask".into(),
                thinking: "adaptive".into(),
                window: 200_000,
                context: 18_000,
                cost: 0.21,
                chef: true,
                asks: true,
                ..Status::default()
            },
            entries,
            approval: None,
            question: None,
            roots: Vec::new(),
            artifact_dir: None,
            kitchen: Some(kitchen.clone()),
        }
    }
    let view = |entries| chef_view(entries, &kitchen);
    server.sync(view(&entries));
    let end = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(end) => break,
            Some(event) = server.events.recv() => {
                if let WebEvent::Connected(id) = event {
                    server.sync(view(&entries));
                    server.welcome(id);
                }
            }
        }
    }
}

/// A stand-in session for the iOS app's integration test (`apple/scripts/integration.sh`):
/// connects to the hub at `TAMALE_HUB_URL` with `TAMALE_HUB_KEY`, end-to-end encrypted,
/// prints `SID=<sid>`, and answers each prompt with a reply and an artifact until
/// `TAMALE_FAKE_SECONDS` (default 120) pass.
/// A page with the review panel, on a repository with changes (TAMALE_FAKE_ROOT, or
/// one made up), for trying pullspace in a browser: prints the address, serves for
/// TAMALE_FAKE_SECONDS. TAMALE_FAKE_LAN serves on the local network instead, as
/// `/web lan` does: plain http to an address that isn't this machine's own is not a
/// secure context, so the browser withholds what a phone on the network goes without.
#[tokio::test]
#[ignore]
async fn fake_review_page() {
    let seconds: u64 = std::env::var("TAMALE_FAKE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let repo = match std::env::var("TAMALE_FAKE_ROOT") {
        Ok(root) => PathBuf::from(root),
        Err(_) => {
            let repo =
                std::env::temp_dir().join(format!("tamale-review-{}", crate::session::new_id()));
            std::fs::create_dir_all(repo.join("src")).unwrap();
            let body: Vec<String> = (1..=60).map(|i| format!("fn line_{i}() {{}}")).collect();
            std::fs::write(repo.join("src/main.rs"), body.join("\n") + "\n").unwrap();
            std::fs::write(repo.join("gone.txt"), "going\n").unwrap();
            let git = |args: &[&str]| {
                let done = std::process::Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(done.status.success(), "git {args:?}");
            };
            git(&["init", "-q"]);
            git(&["add", "."]);
            git(&[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "first",
            ]);
            let mut changed = body.clone();
            changed[29] = "fn line_thirty_changed() {}".into();
            std::fs::write(repo.join("src/main.rs"), changed.join("\n") + "\n").unwrap();
            std::fs::remove_file(repo.join("gone.txt")).unwrap();
            std::fs::write(repo.join("new.txt"), "brand new\n").unwrap();
            repo
        }
    };
    let lan = std::env::var_os("TAMALE_FAKE_LAN").is_some();
    let mut server = Server::start(lan).await.unwrap();
    // A conversation about the repository, so the chat has steps and paths that
    // open in the panel (TAMALE_FAKE_EMPTY leaves it out).
    let tool = |name: &str, summary: &str, description: &str, output: &str| Entry::Tool {
        name: name.into(),
        summary: summary.into(),
        description: description.into(),
        output: Some((output.into(), false)),
    };
    let mut entries = vec![Entry::Info(
        "Welcome! Type a request and press Enter. /help for commands.".into(),
    )];
    if std::env::var_os("TAMALE_FAKE_EMPTY").is_none() {
        entries.extend([
            Entry::User("Cap how much of a file the page is handed at once.".into()),
            tool("read_file", "src/web/files.rs", "Read src/web/files.rs", "use std::path::Path;\n…"),
            tool("edit_file", "src/web/files.rs", "Edit src/web/files.rs", "@@ -38,6 +38,8 @@\n }\n \n+/// Largest file the page is handed whole.\n+pub const MAX_INLINE: u64 = 512 * 1024;\n"),
            tool("bash", "cargo test --bin tamale files", "Run the file tests", "test result: ok. 12 passed; 0 failed"),
            Entry::Assistant(
                "Done. `src/web/files.rs` now has a `MAX_INLINE` cap of 512 KB, checked where the \
                 file is read (`src/web/files.rs:121`).\n\n\
                 - The constant is at the top, beside `relative`.\n\
                 - `docs/review-panel.md` is new, and `e2e.md` is gone.\n\n\
                 The file tests pass."
                    .into(),
            ),
        ]);
    }
    // A reply that shows a video (TAMALE_FAKE_VIDEO: its path), as one rendered with Manim would.
    if let Ok(video) = std::env::var("TAMALE_FAKE_VIDEO") {
        entries.extend([
            Entry::User("Animate how the cap works.".into()),
            Entry::Assistant(format!("Here's the animation.\n\n```video\n{video}\n```")),
        ]);
    }
    // A lesson (TAMALE_FAKE_LESSON): what's sent from it gets a tutor's reply, streamed.
    if std::env::var_os("TAMALE_FAKE_LESSON").is_some() {
        entries.extend([
            Entry::User(
                "Can you teach me this from the ground up, as a lesson I can check my understanding in?"
                    .into(),
            ),
            Entry::Assistant(format!(
                "Here's the cap from the ground up: from how the page asks for one file to why                  the branch raises it.\n\n```openui\n{LESSON_ARTIFACT}\n```"
            )),
        ]);
    }
    // A reply with an artifact (TAMALE_FAKE_ARTIFACT), to open or talk about.
    if std::env::var_os("TAMALE_FAKE_ARTIFACT").is_some() {
        entries.push(Entry::Assistant(format!(
            "The cap, on a page:\n\n```openui\n{SHOWCASE_ARTIFACT}\n```"
        )));
    }
    let mut v = view(&entries);
    v.status = Status {
        project: repo
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        cwd: repo.display().to_string(),
        model: "Claude Opus 5.5".into(),
        backend: "Anthropic API".into(),
        signed_in: true,
        mode: "ask".into(),
        effort: Some("high".into()),
        thinking: "adaptive".into(),
        window: 200_000,
        context: 31_400,
        cost: 0.42,
        ..Status::default()
    };
    v.roots = vec![repo.clone()];
    let (status, roots) = (v.status.clone(), v.roots.clone());
    server.sync(v);
    println!("URL={}", server.url);
    // With a hub (TAMALE_HUB_URL, TAMALE_HUB_KEY), encrypted, and a link into it.
    if let (Ok(hub), Ok(key)) = (
        std::env::var("TAMALE_HUB_URL"),
        std::env::var("TAMALE_HUB_KEY"),
    ) {
        use base64::Engine;
        let key = tamale_hub_protocol::decode_key(&key).expect("a key");
        let token = tamale_hub_protocol::auth_value(&key);
        server.connect_hub(
            &hub,
            &token,
            Label::default(),
            Some(e2e::Keys::from_key(&key)),
        );
        loop {
            match server.events.recv().await.unwrap() {
                WebEvent::Hub(Ok(_)) => break,
                WebEvent::Hub(Err(e)) => panic!("{e}"),
                _ => {}
            }
        }
        let origin = hub
            .replacen("wss://", "https://", 1)
            .replacen("ws://", "http://", 1);
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let r = http
            .post(format!("{origin}/api/tokens"))
            .header("origin", &origin)
            .json(&json!({ "add": token }))
            .send()
            .await
            .unwrap();
        let th = r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let ticket: Value = http
            .post(format!("{origin}/api/ticket"))
            .header("origin", &origin)
            .header("cookie", &th)
            .json(&json!({ "sid": relay::sid() }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let psk = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(tamale_hub_protocol::e2e_secret(&key));
        println!("HUB_URL={}#k={psk}", ticket["url"].as_str().unwrap());
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, server.events.recv()).await {
        match event {
            WebEvent::Connected(id) => server.welcome(id),
            // A reply that streams in a document (TAMALE_FAKE_STREAM: its path) as a model
            // would write it, a few characters at a time.
            WebEvent::Send(text) if std::env::var_os("TAMALE_FAKE_STREAM").is_some() => {
                println!("SEND={text:?}");
                let doc =
                    std::fs::read_to_string(std::env::var("TAMALE_FAKE_STREAM").unwrap()).unwrap();
                entries.push(Entry::User(text));
                entries.push(Entry::Assistant(String::new()));
                let full = format!("Here it is.\n\n```openui\n{}\n```\n\nDone.", doc.trim_end());
                let cuts: Vec<usize> = full
                    .char_indices()
                    .map(|(i, _)| i)
                    .step_by(24)
                    .chain([full.len()])
                    .collect();
                for cut in cuts {
                    *entries.last_mut().unwrap() = Entry::Assistant(full[..cut].to_string());
                    let mut v = view(&entries);
                    v.status = status.clone();
                    v.roots = roots.clone();
                    server.sync(v);
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                }
                println!("STREAMED");
            }
            // An answer or a question from the lesson: a tutor's reply, a few words at a time.
            WebEvent::Send(text) if openui::lesson::parse(&text).is_some() => {
                println!("SEND={text:?}");
                let answer = openui::lesson::parse(&text)
                    .and_then(|m| openui::lesson::says::answered(m.body))
                    .is_some();
                let reply = if answer {
                    "**Partly.** You've got the waiting: nothing shows until the last byte arrives. \
                     But think about the message itself. Back in the simplest case, the answer \
                     is *one* message. What might the connection do with one 50 MB message?"
                } else {
                    "Good question. Go back to the librarian: asking for a book costs a trip whatever \
                     its size. Each piece is a trip of its own, so pieces only pay when the book \
                     is too heavy to carry at once. Does that make the 512 KB cap feel right?"
                };
                entries.push(Entry::User(text));
                entries.push(Entry::Assistant(String::new()));
                let mut st = status.clone();
                st.running = true;
                let words: Vec<&str> = reply.split_inclusive(' ').collect();
                for k in 0..=words.len() {
                    *entries.last_mut().unwrap() = Entry::Assistant(words[..k].concat());
                    let mut v = view(&entries);
                    v.status = if k == words.len() {
                        status.clone()
                    } else {
                        st.clone()
                    };
                    v.roots = roots.clone();
                    server.sync(v);
                    tokio::time::sleep(std::time::Duration::from_millis(if k == 0 {
                        900
                    } else {
                        40
                    }))
                    .await;
                }
            }
            // What the page sent, for checking it from a script.
            WebEvent::Send(text) => println!("SEND={text:?}"),
            WebEvent::Interrupt => println!("INTERRUPT"),
            _ => {}
        }
    }
}

#[tokio::test]
#[ignore]
async fn fake_session_for_the_apple_app() {
    let env = |k| std::env::var(k).unwrap_or_else(|_| panic!("{k} not set"));
    let hub = env("TAMALE_HUB_URL");
    let key = tamale_hub_protocol::decode_key(&env("TAMALE_HUB_KEY")).expect("a key");
    let seconds: u64 = std::env::var("TAMALE_FAKE_SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let mut server = Server::start(false).await.unwrap();
    // A repository with changes, for the diff viewer.
    let repo = std::env::temp_dir().join(format!("tamale-fake-{}", crate::session::new_id()));
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let body: Vec<String> = (1..=60).map(|i| format!("line {i}")).collect();
    std::fs::write(repo.join("src/main.rs"), body.join("\n") + "\n").unwrap();
    std::fs::write(repo.join("gone.txt"), "going\n").unwrap();
    let git = |args: &[&str]| {
        let done = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(done.status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-qm",
        "first",
    ]);
    let mut changed = body.clone();
    changed[29] = "line thirty, changed".into();
    std::fs::write(repo.join("src/main.rs"), changed.join("\n") + "\n").unwrap();
    std::fs::remove_file(repo.join("gone.txt")).unwrap();
    std::fs::write(repo.join("new.txt"), "brand new\n").unwrap();
    let roots = vec![repo.clone()];
    // TAMALE_FAKE_HISTORY turns in the middle, for loading a long session.
    let history: usize = std::env::var("TAMALE_FAKE_HISTORY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut entries = vec![
        Entry::Info("Welcome! A stand-in session for the iOS app's tests.".into()),
        Entry::User("Show me the plan".into()),
    ];
    entries.extend((0..history).flat_map(|i| {
        [
            Entry::Assistant(format!(
                "Old answer {i}.\n\nWith a second paragraph to give it height."
            )),
            Entry::User(format!("Old question {i}")),
        ]
    }));
    entries.extend([Entry::Assistant(
        "Here it is.\n\n```openui\nroot = Card([CardHeader(\"The plan\"), TextContent(\"Plan\")])\n```"
            .into(),
    )]);
    let mut approval = Some(Approval {
        name: "bash".into(),
        summary: "rm -rf target".into(),
        description: "Clean the build".into(),
        always: vec!["bash: rm *".into()],
    });
    // "ask" gets questions instead of an approval.
    let mut question: Option<Questions> = None;
    let sync = |server: &mut Server,
                entries: &[Entry],
                approval: &Option<Approval>,
                question: &Option<Questions>| {
        let mut v = view(entries);
        v.approval = approval.clone();
        v.question = question.clone();
        v.roots = roots.clone();
        server.sync(v);
    };
    sync(&mut server, &entries, &approval, &question);
    server.connect_hub(
        &hub,
        &tamale_hub_protocol::auth_value(&key),
        Label {
            host: "fake-host".into(),
            cwd: "~/dev/fake".into(),
            model: "fake-model".into(),
            ..Default::default()
        },
        Some(e2e::Keys::from_key(&key)),
    );
    println!("SID={}", relay::sid());
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(seconds);
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, server.events.recv()).await {
        match event {
            WebEvent::Connected(id) => server.welcome(id),
            WebEvent::Older { id, before } => server.older(id, before),
            WebEvent::Send(text) if text == "ask" => {
                entries.push(Entry::User(text));
                question = Some(Questions {
                    description: "Ask about the release".into(),
                    questions: crate::tools::ask::parse(&serde_json::json!({
                        "questions": [
                            {
                                "question": "Which channel should it go to?",
                                "header": "Channel",
                                "options": [
                                    { "label": "Beta (Recommended)", "description": "Testers first" },
                                    { "label": "Stable" }
                                ]
                            },
                            {
                                "question": "What should the notes cover?",
                                "header": "Notes",
                                "options": [{ "label": "Fixes" }, { "label": "Features" }],
                                "multi_select": true
                            }
                        ]
                    }))
                    .unwrap(),
                });
            }
            WebEvent::Answer(answers) => {
                question = None;
                // As after any prompt, so the next test finds one.
                approval.get_or_insert_with(|| Approval {
                    name: "bash".into(),
                    summary: "rm -rf target".into(),
                    description: "Clean the build".into(),
                    always: Vec::new(),
                });
                let shown: Vec<String> = answers
                    .iter()
                    .map(|a| {
                        let mut parts = a.chosen.clone();
                        parts.extend(a.other.clone());
                        parts.join(" + ")
                    })
                    .collect();
                entries.push(Entry::Info(format!("answered: {}", shown.join(" | "))));
            }
            WebEvent::Send(text) if text == "Can you generate an artifact for this?" => {
                // As a model writes one: a little prose, then the page line by line.
                entries.push(Entry::User(text));
                let mut reply = "Sure, here's a page for it.\n\n```openui\n".to_string();
                entries.push(Entry::Assistant(reply.clone()));
                // Named apart from "long"'s page, which other tests look for.
                let page = LONG_ARTIFACT.replace("Launch checklist", "Release plan");
                for line in page.lines() {
                    reply += line;
                    reply += "\n";
                    *entries.last_mut().unwrap() = Entry::Assistant(reply.clone());
                    sync(&mut server, &entries, &approval, &question);
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                reply += "```\n\nOpen it to see every step.";
                *entries.last_mut().unwrap() = Entry::Assistant(reply);
            }
            WebEvent::Send(text) => {
                approval.get_or_insert_with(|| Approval {
                    name: "bash".into(),
                    summary: "rm -rf target".into(),
                    description: "Clean the build".into(),
                    always: Vec::new(),
                });
                entries.push(Entry::User(text.clone()));
                if text == "stream" {
                    // A reply taller than the screen, coming in bit by bit.
                    let mut reply = String::new();
                    entries.push(Entry::Assistant(reply.clone()));
                    for i in 0..60 {
                        reply += &format!("Streamed paragraph {i}.\n\n");
                        *entries.last_mut().unwrap() = Entry::Assistant(reply.clone());
                        sync(&mut server, &entries, &approval, &question);
                        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                    }
                    continue;
                }
                if text == "long" {
                    entries.push(Entry::Assistant(format!(
                        "A long one.\n\n```openui\n{LONG_ARTIFACT}\n```"
                    )));
                    sync(&mut server, &entries, &approval, &question);
                    continue;
                }
                entries.push(Entry::Assistant(format!(
                    "You said **{text}**.\n\n```openui\ntitle = \"Echo\"\nroot = Card([TextContent({})])\n```",
                    json!(text)
                )));
            }
            WebEvent::Approve(choice, reason) => {
                approval = None;
                entries.push(Entry::Info(format!(
                    "approved: {choice:?} {}",
                    reason.unwrap_or_default()
                )));
            }
            WebEvent::Interrupt => entries.push(Entry::Info("interrupted".into())),
            WebEvent::Hub(Err(e)) => eprintln!("hub: {e}"),
            WebEvent::Close => {
                server.end().await;
                return;
            }
            _ => {}
        }
        sync(&mut server, &entries, &approval, &question);
    }
}

#[test]
fn tool_output_is_clipped_at_a_character_boundary() {
    let (text, cut) = clip("aé", 2);
    assert_eq!((text, cut), ("a", true));
    assert_eq!(clip("abc", 10), ("abc", false));
    let tool = wire_entry(
        &Entry::Tool {
            name: "read_file".into(),
            summary: "src/main.rs".into(),
            description: "Read src/main.rs".into(),
            output: Some(("x".repeat(MAX_TOOL_OUTPUT + 5), false)),
        },
        &Mutex::default(),
        None,
    );
    let wire::Entry::Tool { output, cut, .. } = tool else {
        panic!("not a tool: {tool:?}");
    };
    assert!(cut);
    assert_eq!(output.unwrap().len(), MAX_TOOL_OUTPUT);
}

/// pullspace is drawn in the page with a stylesheet of its own, so every rule of the page's
/// stops at its box: `svg`, `button` and the class names both use (`.app`, `.main`…).
#[test]
fn the_pages_stylesheet_stops_at_pullspaces_box() {
    let served: &str = &SERVED_STYLE;
    let plain = pullspace_embed::css::strip_comments(STYLE);
    let count = |s: &str, c: char| s.chars().filter(|x| *x == c).count();
    assert_eq!(count(served, '{'), count(&plain, '{'));
    assert_eq!(count(served, '}'), count(&plain, '}'));
    for rule in ["svg", "button", ".app", ".main", ".topbar"] {
        assert!(
            served.contains(&format!("\n{rule}:where(:not(.pullspace *)){{")),
            "{rule}"
        );
    }
    // Every selector of every style rule carries the guard; at-rules and keyframes don't.
    let mut depth_in_keyframes = None;
    let mut depth = 0;
    for line in served.lines() {
        let line = line.trim();
        if line.starts_with("@keyframes") {
            depth_in_keyframes = Some(depth);
        }
        if depth_in_keyframes.is_none() && !line.starts_with('@') && line.contains('{') {
            let selectors = &line[..line.find('{').unwrap()];
            assert!(
                selectors
                    .trim_start_matches('}')
                    .contains(":where(:not(.pullspace *))"),
                "{line}"
            );
        }
        depth += count(line, '{') as i32 - count(line, '}') as i32;
        if depth_in_keyframes.is_some_and(|d| depth <= d) {
            depth_in_keyframes = None;
        }
    }
}

#[test]
fn the_artifact_guide_lists_every_component_and_its_examples_draw() {
    let guide = artifact_guide();
    assert!(!guide.contains("{{components}}"));
    for c in openui::library::LIBRARY {
        assert!(guide.contains(&c.signature()), "{}", c.name);
    }
    // Every example in it is a document tamale can draw, with nothing left out.
    let mut examples = 0;
    for block in guide.split("```openui\n").skip(1) {
        let source = block.split("```").next().unwrap();
        assert_eq!(openui::problems(source), Vec::<String>::new(), "{source}");
        assert!(openui::title(source).is_some(), "{source}");
        examples += 1;
    }
    assert!(examples > 0);
    // So do the documents the fake pages show.
    for doc in [LONG_ARTIFACT, SHOWCASE_ARTIFACT, LESSON_ARTIFACT] {
        assert_eq!(openui::problems(doc), Vec::<String>::new(), "{doc}");
    }
}

/// Writes the page the iOS app is sent for the showcase artifact (or `TAMALE_PAGE_SRC`) to `TAMALE_PAGE_OUT`, for
/// trying it in a web view: `cargo test -p tamale write_embedded_page -- --ignored`
#[test]
#[ignore]
fn write_embedded_page() {
    let out = std::env::var("TAMALE_PAGE_OUT").expect("TAMALE_PAGE_OUT");
    // Or another document, from `TAMALE_PAGE_SRC`.
    let source = std::env::var("TAMALE_PAGE_SRC")
        .map(|path| std::fs::read_to_string(path).unwrap())
        .unwrap_or_else(|_| SHOWCASE_ARTIFACT.to_string());
    std::fs::write(out, super::artifacts::embedded_page(&source)).unwrap();
}
