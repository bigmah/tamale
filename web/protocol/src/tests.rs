use alloc::string::ToString;
use alloc::vec;

use ratatui_core::buffer;
use ratatui_core::style::{self, Modifier};

use super::*;

#[test]
fn named_keys_map_to_terminal_keys() {
    let code = |name, shift| Key::from_dom(name, false, false, shift).unwrap().code;
    assert_eq!(code("Enter", false), KeyCode::Enter);
    assert_eq!(code("Escape", false), KeyCode::Esc);
    assert_eq!(code("ArrowUp", false), KeyCode::Up);
    assert_eq!(code("Tab", false), KeyCode::Tab);
    assert_eq!(code("Tab", true), KeyCode::BackTab);
    assert_eq!(code("F5", false), KeyCode::F(5));
    assert_eq!(code("F", true), KeyCode::Char('F'));
    assert_eq!(code("é", false), KeyCode::Char('é'));
    assert_eq!(Key::from_dom("Fn", false, false, false), None);
    assert_eq!(Key::from_dom("Shift", false, false, true), None);
}

#[test]
fn modifiers_carry_over() {
    let key = Key::from_dom("c", true, false, false).unwrap();
    assert_eq!(
        (key.code, key.ctrl, key.alt, key.shift),
        (KeyCode::Char('c'), true, false, false)
    );
}

#[test]
fn colors_round_trip() {
    for color in [
        style::Color::Reset,
        style::Color::DarkGray,
        style::Color::White,
        style::Color::Indexed(200),
        style::Color::Rgb(1, 2, 3),
    ] {
        assert_eq!(style::Color::from(Color::from(color)), color);
    }
}

#[test]
fn frames_round_trip() {
    let mut cell = buffer::Cell::new("🫔");
    cell.fg = style::Color::Rgb(250, 180, 60);
    cell.modifier = Modifier::BOLD | Modifier::ITALIC;
    let frame = ServerMessage::Frame(Frame {
        clear: true,
        cells: vec![Cell::new(3, 4, &cell)],
        cursor: Some((1, 2)),
    });
    let decoded: ServerMessage = decode(&encode(&frame)).unwrap();
    assert_eq!(decoded, frame);
    let ServerMessage::Frame(decoded) = decoded;
    assert_eq!(decoded.cells[0].to_cell(), cell);

    let paste = ClientMessage::Paste("a\nb".to_string());
    assert_eq!(decode::<ClientMessage>(&encode(&paste)), Some(paste));
    assert_eq!(decode::<ClientMessage>(&[0xff]), None);
}
