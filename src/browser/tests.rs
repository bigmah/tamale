use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};

use super::{ClientMessage, key_event, takes_over};

fn events(json: &str) -> Vec<Event> {
    serde_json::from_str::<ClientMessage>(json)
        .unwrap()
        .events()
}

#[test]
fn named_keys_map_to_terminal_keys() {
    let key = |name, shift| key_event(name, false, false, shift).unwrap().code;
    assert_eq!(key("Enter", false), KeyCode::Enter);
    assert_eq!(key("Escape", false), KeyCode::Esc);
    assert_eq!(key("ArrowUp", false), KeyCode::Up);
    assert_eq!(key("Tab", false), KeyCode::Tab);
    assert_eq!(key("Tab", true), KeyCode::BackTab);
    assert_eq!(key("F5", false), KeyCode::F(5));
    assert_eq!(key("F", true), KeyCode::Char('F'));
    assert_eq!(key("é", false), KeyCode::Char('é'));
    assert_eq!(key_event("Fn", false, false, false), None);
    assert_eq!(key_event("Shift", false, false, true), None);
}

#[test]
fn modifiers_carry_over() {
    let key = key_event("c", true, false, false).unwrap();
    assert_eq!(
        key,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
    );
    let key = key_event("Enter", false, true, true).unwrap();
    assert_eq!(key.modifiers, KeyModifiers::ALT | KeyModifiers::SHIFT);
}

#[test]
fn messages_become_events() {
    assert_eq!(
        events(r#"{"t":"key","key":"a"}"#),
        [Event::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE
        ))]
    );
    // Composed text is typed character by character; control characters are dropped.
    let typed = events(r#"{"t":"text","text":"日本\r"}"#);
    assert_eq!(typed.len(), 2);
    assert_eq!(
        events(r#"{"t":"paste","text":"a\nb"}"#),
        [Event::Paste("a\nb".into())]
    );
    let [Event::Mouse(wheel)] = events(r#"{"t":"wheel","up":true,"col":3,"row":4}"#)[..] else {
        panic!("expected one mouse event");
    };
    assert_eq!(
        (wheel.kind, wheel.column, wheel.row),
        (MouseEventKind::ScrollUp, 3, 4)
    );
    assert!(events(r#"{"t":"size","cols":80,"rows":24}"#).is_empty());
    assert!(serde_json::from_str::<ClientMessage>(r#"{"t":"bogus"}"#).is_err());
}

#[test]
fn only_deliberate_input_takes_over() {
    let click = &events(r#"{"t":"click","col":0,"row":0}"#)[0];
    assert!(matches!(click, Event::Mouse(m) if m.kind == MouseEventKind::Down(MouseButton::Left)));
    assert!(takes_over(click));
    assert!(!takes_over(
        &events(r#"{"t":"wheel","up":false,"col":0,"row":0}"#)[0]
    ));
    assert!(takes_over(&Event::Paste(String::new())));
    let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    release.kind = KeyEventKind::Release;
    assert!(!takes_over(&Event::Key(release)));
    assert!(!takes_over(&Event::Resize(80, 24)));
}
