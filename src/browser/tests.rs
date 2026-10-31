use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use tamale_web_protocol::{self as protocol, ClientMessage};

use super::{events, takes_over, terminal};

fn key(code: protocol::KeyCode, ctrl: bool, alt: bool, shift: bool) -> ClientMessage {
    ClientMessage::Key(protocol::Key {
        code,
        ctrl,
        alt,
        shift,
    })
}

#[test]
fn keys_become_terminal_keys() {
    assert_eq!(
        events(key(protocol::KeyCode::Char('c'), true, false, false)),
        [Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))]
    );
    let [Event::Key(enter)] = events(key(protocol::KeyCode::Enter, false, true, true))[..] else {
        panic!("expected one key");
    };
    assert_eq!(enter.code, KeyCode::Enter);
    assert_eq!(enter.modifiers, KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(
        events(key(protocol::KeyCode::BackTab, false, false, true))[0],
        Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))
    );
}

#[test]
fn messages_become_events() {
    // Composed text is typed character by character; control characters are dropped.
    assert_eq!(events(ClientMessage::Text("日本\r".into())).len(), 2);
    assert_eq!(
        events(ClientMessage::Paste("a\nb".into())),
        [Event::Paste("a\nb".into())]
    );
    let [Event::Mouse(wheel)] = events(ClientMessage::Wheel {
        up: true,
        col: 3,
        row: 4,
    })[..] else {
        panic!("expected one mouse event");
    };
    assert_eq!(
        (wheel.kind, wheel.column, wheel.row),
        (MouseEventKind::ScrollUp, 3, 4)
    );
    assert!(events(ClientMessage::Size { cols: 80, rows: 24 }).is_empty());
}

#[test]
fn only_deliberate_input_takes_over() {
    let click = &events(ClientMessage::Click { col: 0, row: 0 })[0];
    assert!(matches!(click, Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left)));
    assert!(takes_over(click));
    let wheel = &events(ClientMessage::Wheel {
        up: false,
        col: 0,
        row: 0,
    })[0];
    assert!(!takes_over(wheel));
    assert!(takes_over(&Event::Paste(String::new())));
    let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    assert!(!takes_over(&Event::Key(release)));
    assert!(!takes_over(&Event::Resize(80, 24)));
}

#[test]
fn frames_carry_only_changes() {
    let mut web = terminal(10, 2);
    let draw = |web: &mut super::WebTerminal, text: &'static str| {
        web.draw(|f| f.render_widget(Line::from(text.red()), Rect::new(0, 0, 10, 1)))
            .unwrap();
        web.backend_mut().take_frame()
    };
    let first = draw(&mut web, "hi").unwrap();
    assert_eq!(first.cells.len(), 2);
    assert_eq!(first.cells[0].symbol, "h");
    assert_eq!(first.cursor, None);
    assert_eq!(draw(&mut web, "hi"), None);
    let changed = draw(&mut web, "ho").unwrap();
    assert_eq!(
        changed
            .cells
            .iter()
            .map(|c| (c.x, c.symbol.as_str()))
            .collect::<Vec<_>>(),
        [(1, "o")]
    );

    // A resize repaints everything at the new size.
    web.backend_mut().resize(4, 1);
    let resized = draw(&mut web, "ho").unwrap();
    assert!(resized.clear);
    assert!(resized.cells.iter().all(|c| c.x < 4 && c.y < 1));
}
