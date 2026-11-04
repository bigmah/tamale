use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::widgets::Paragraph;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use super::*;

static NEXT: AtomicU32 = AtomicU32::new(0);

/// In /tmp, since socket paths have to be short.
fn temp_dir() -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(format!("/tmp/tb-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn connect(dir: &Path, id: u32, protocol: u32) -> UnixStream {
    let mut stream = UnixStream::connect(socket_path(dir, id)).await.unwrap();
    let hello = ToServer::Hello {
        protocol,
        cols: 20,
        rows: 4,
    };
    write_message(&mut stream, &hello).await.unwrap();
    stream
}

async fn next(host: &mut Host) -> HostEvent {
    tokio::time::timeout(Duration::from_secs(5), host.next())
        .await
        .unwrap()
}

async fn receive(stream: &mut UnixStream) -> Option<ToClient> {
    tokio::time::timeout(Duration::from_secs(5), read_message(stream))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn every_kind_of_message_survives_the_socket() {
    let (mut a, mut b) = UnixStream::pair().unwrap();
    let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 3,
        row: 4,
        modifiers: KeyModifiers::SHIFT,
    };
    let messages = [
        ToServer::Hello {
            protocol: PROTOCOL,
            cols: 80,
            rows: 24,
        },
        ToServer::Input(Event::Key(key)),
        ToServer::Input(Event::Mouse(mouse)),
        ToServer::Input(Event::Paste("multi\nline ☃".into())),
        ToServer::Resize { cols: 1, rows: 2 },
        ToServer::LoginDone {
            success: false,
            status: "exit status: 1".into(),
        },
        ToServer::Quit,
    ];
    for message in &messages {
        write_message(&mut a, message).await.unwrap();
        let read: ToServer = read_message(&mut b).await.unwrap().unwrap();
        assert_eq!(&read, message);
    }
    drop(a);
    assert_eq!(read_message::<ToServer>(&mut b).await.unwrap(), None);
}

#[tokio::test]
async fn a_message_too_long_is_an_error_not_an_allocation() {
    let (mut a, mut b) = UnixStream::pair().unwrap();
    a.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
    assert!(read_message::<ToServer>(&mut b).await.is_err());
}

#[tokio::test]
async fn a_client_attaches_gets_frames_detaches_and_a_new_one_gets_the_whole_screen() {
    let dir = temp_dir();
    let mut host = Host::bind_in(&dir, 1).unwrap();
    let mut first = connect(&dir, 1, PROTOCOL).await;
    assert!(matches!(next(&mut host).await, HostEvent::Attached));

    let hello = || Paragraph::new("hello");
    host.draw(|f| f.render_widget(hello(), f.area())).unwrap();
    let Some(ToClient::Frame(frame)) = receive(&mut first).await else {
        panic!("expected a frame");
    };
    assert_eq!(frame.cells.len(), 5);

    // The same screen again is nothing new.
    let key = ToServer::Input(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    write_message(&mut first, &key).await.unwrap();
    assert!(matches!(next(&mut host).await, HostEvent::Input(_)));

    host.detach();
    assert_eq!(
        receive(&mut first).await,
        Some(ToClient::Bye(Bye::Detached))
    );
    assert_eq!(receive(&mut first).await, None);
    assert!(!list_in(&dir)[0].attached);

    // Nobody's watching: drawing goes nowhere, and the server carries on.
    host.draw(|f| f.render_widget(hello(), f.area())).unwrap();
    drop(first);
    assert!(matches!(next(&mut host).await, HostEvent::Changed));

    let mut second = connect(&dir, 1, PROTOCOL).await;
    assert!(matches!(next(&mut host).await, HostEvent::Attached));
    assert!(list_in(&dir)[0].attached);
    host.draw(|f| f.render_widget(hello(), f.area())).unwrap();
    let Some(ToClient::Frame(frame)) = receive(&mut second).await else {
        panic!("expected a frame");
    };
    assert_eq!(frame.cells.len(), 5);

    host.finish(None).await;
    assert_eq!(
        receive(&mut second).await,
        Some(ToClient::Bye(Bye::Exited(None)))
    );
    assert!(list_in(&dir).is_empty());
    assert!(!socket_path(&dir, 1).exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_second_client_takes_over_and_the_first_one_leaving_changes_nothing() {
    let dir = temp_dir();
    let mut host = Host::bind_in(&dir, 2).unwrap();
    let mut first = connect(&dir, 2, PROTOCOL).await;
    assert!(matches!(next(&mut host).await, HostEvent::Attached));
    let mut second = connect(&dir, 2, PROTOCOL).await;
    assert!(matches!(next(&mut host).await, HostEvent::Attached));
    assert_eq!(
        receive(&mut first).await,
        Some(ToClient::Bye(Bye::Replaced))
    );
    drop(first);
    assert!(matches!(next(&mut host).await, HostEvent::Changed));
    assert!(list_in(&dir)[0].attached);

    host.set_mouse(true);
    host.set_mouse(true);
    assert_eq!(receive(&mut second).await, Some(ToClient::Mouse(true)));
    host.set_mouse(false);
    assert_eq!(receive(&mut second).await, Some(ToClient::Mouse(false)));

    drop(second);
    assert!(matches!(next(&mut host).await, HostEvent::Changed));
    assert!(!list_in(&dir)[0].attached);
    drop(host);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn another_protocol_is_refused_and_quit_needs_no_hello() {
    let dir = temp_dir();
    let mut host = Host::bind_in(&dir, 3).unwrap();
    let mut old = connect(&dir, 3, PROTOCOL + 1).await;
    assert!(matches!(
        receive(&mut old).await,
        Some(ToClient::Refused(_))
    ));

    let mut killer = UnixStream::connect(socket_path(&dir, 3)).await.unwrap();
    write_message(&mut killer, &ToServer::Quit).await.unwrap();
    assert!(matches!(next(&mut host).await, HostEvent::Quit));
    drop(host);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_dead_server_is_swept_up_and_a_live_one_lists_its_status() {
    let dir = temp_dir();
    let mut host = Host::bind_in(&dir, 4).unwrap();
    host.publish(Path::new("/project"), "s1", "fix it".into(), State::Running);
    let status = &list_in(&dir)[0];
    assert_eq!(status.id, 4);
    assert_eq!(status.cwd, Path::new("/project"));
    assert_eq!(status.session_id, "s1");
    assert_eq!(status.title, "fix it");
    assert_eq!(status.state, State::Running);

    // As if it crashed: files with nothing listening.
    let dead = Status {
        id: 99,
        ..status.clone()
    };
    std::fs::write(status_path(&dir, 99), serde_json::to_vec(&dead).unwrap()).unwrap();
    std::fs::write(log_path(&dir, 99), "panic").unwrap();
    assert_eq!(list_in(&dir).len(), 1);
    assert!(!status_path(&dir, 99).exists() && !log_path(&dir, 99).exists());
    drop(host);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn only_an_unused_server_expires() {
    let dir = temp_dir();
    let mut host = Host::bind_in(&dir, 5).unwrap();
    let now = Duration::ZERO;
    assert!(!host.expired_after(true, now));
    assert!(host.expired_after(false, now));
    assert!(!host.expired_after(false, Duration::from_secs(3600)));
    assert!(host.idle_since.is_some());
    assert!(!host.expired_after(true, now));
    assert!(host.idle_since.is_none());

    let _client = connect(&dir, 5, PROTOCOL).await;
    assert!(matches!(next(&mut host).await, HostEvent::Attached));
    assert!(!host.expired_after(false, now));
    drop(host);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn a_sign_in_runs_in_the_client_and_repaints_afterwards() {
    let dir = temp_dir();
    let mut host = Host::bind_in(&dir, 6).unwrap();
    let command = LoginCommand {
        program: "claude".into(),
        args: vec!["auth".into(), "login".into()],
        envs: vec![("ANTHROPIC_API_KEY".into(), None)],
    };
    assert!(host.login(command.clone()).await.is_err());

    let mut client = connect(&dir, 6, PROTOCOL).await;
    assert!(matches!(next(&mut host).await, HostEvent::Attached));
    let expected = command.clone();
    let answer = tokio::spawn(async move {
        assert_eq!(receive(&mut client).await, Some(ToClient::Login(expected)));
        let done = ToServer::LoginDone {
            success: true,
            status: "exit status: 0".into(),
        };
        write_message(&mut client, &done).await.unwrap();
        client
    });
    assert_eq!(
        host.login(command.clone()).await.unwrap(),
        (true, "exit status: 0".to_string())
    );
    let mut client = answer.await.unwrap();

    host.draw(|f| f.render_widget(Paragraph::new("hi"), f.area()))
        .unwrap();
    let Some(ToClient::Frame(frame)) = receive(&mut client).await else {
        panic!("expected a frame");
    };
    assert!(frame.clear);

    // The terminal going away ends the wait.
    let waiting = tokio::spawn(async move {
        let result = host.login(command).await;
        (host, result)
    });
    assert!(matches!(
        receive(&mut client).await,
        Some(ToClient::Login(_))
    ));
    drop(client);
    let (host, result) = waiting.await.unwrap();
    assert!(result.is_err());
    drop(host);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn switching_sends_the_client_on_even_if_the_server_exits_right_after() {
    let dir = temp_dir();
    let mut host = Host::bind_in(&dir, 7).unwrap();
    assert!(!host.switch(8));
    let mut client = connect(&dir, 7, PROTOCOL).await;
    assert!(matches!(next(&mut host).await, HostEvent::Attached));
    assert!(host.switch(8));
    assert!(!list_in(&dir)[0].attached);
    host.finish(None).await;
    assert_eq!(
        receive(&mut client).await,
        Some(ToClient::Bye(Bye::Switch(8)))
    );
    assert_eq!(receive(&mut client).await, None);
    std::fs::remove_dir_all(dir).unwrap();
}
