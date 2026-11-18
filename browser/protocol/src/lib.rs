//! Messages between tamale and its browser client, encoded with postcard. tamale sends
//! the cells that changed since the last frame, the client sends input and its size.
//! Cells use this crate's own types rather than ratatui's `Cell`, whose fields depend on
//! the features each side's build enables.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use ratatui_core::buffer;
use ratatui_core::style::{self, Modifier};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// The client's grid size; sent on connect and whenever it changes.
    Size {
        cols: u16,
        rows: u16,
    },
    Key(Key),
    /// Text from an input method, typed as if key by key.
    Text(String),
    Paste(String),
    Wheel {
        up: bool,
        col: u16,
        row: u16,
    },
    Click {
        col: u16,
        row: u16,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMessage {
    Frame(Frame),
}

/// Changes to the screen since the previous frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// Blank the screen before applying `cells`.
    pub clear: bool,
    pub cells: Vec<Cell>,
    /// Where the cursor shows, if it does.
    pub cursor: Option<(u16, u16)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub x: u16,
    pub y: u16,
    pub symbol: String,
    pub fg: Color,
    pub bg: Color,
    pub modifier: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    Reset,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Cell {
    pub fn new(x: u16, y: u16, cell: &buffer::Cell) -> Self {
        Self {
            x,
            y,
            symbol: cell.symbol().into(),
            fg: cell.fg.into(),
            bg: cell.bg.into(),
            modifier: cell.modifier.bits(),
        }
    }

    pub fn to_cell(&self) -> buffer::Cell {
        let mut cell = buffer::Cell::default();
        cell.set_symbol(&self.symbol);
        cell.fg = self.fg.into();
        cell.bg = self.bg.into();
        cell.modifier = Modifier::from_bits_truncate(self.modifier);
        cell
    }
}

impl From<style::Color> for Color {
    fn from(color: style::Color) -> Self {
        use style::Color as C;
        match color {
            C::Reset => Self::Reset,
            C::Black => Self::Indexed(0),
            C::Red => Self::Indexed(1),
            C::Green => Self::Indexed(2),
            C::Yellow => Self::Indexed(3),
            C::Blue => Self::Indexed(4),
            C::Magenta => Self::Indexed(5),
            C::Cyan => Self::Indexed(6),
            C::Gray => Self::Indexed(7),
            C::DarkGray => Self::Indexed(8),
            C::LightRed => Self::Indexed(9),
            C::LightGreen => Self::Indexed(10),
            C::LightYellow => Self::Indexed(11),
            C::LightBlue => Self::Indexed(12),
            C::LightMagenta => Self::Indexed(13),
            C::LightCyan => Self::Indexed(14),
            C::White => Self::Indexed(15),
            C::Indexed(i) => Self::Indexed(i),
            C::Rgb(r, g, b) => Self::Rgb(r, g, b),
        }
    }
}

impl From<Color> for style::Color {
    fn from(color: Color) -> Self {
        use style::Color as C;
        const NAMED: [C; 16] = [
            C::Black,
            C::Red,
            C::Green,
            C::Yellow,
            C::Blue,
            C::Magenta,
            C::Cyan,
            C::Gray,
            C::DarkGray,
            C::LightRed,
            C::LightGreen,
            C::LightYellow,
            C::LightBlue,
            C::LightMagenta,
            C::LightCyan,
            C::White,
        ];
        match color {
            Color::Reset => C::Reset,
            Color::Indexed(i) => NAMED.get(i as usize).copied().unwrap_or(C::Indexed(i)),
            Color::Rgb(r, g, b) => C::Rgb(r, g, b),
        }
    }
}

/// A key press, as a terminal would report it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyCode {
    Char(char),
    F(u8),
    Enter,
    Backspace,
    Tab,
    BackTab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
}

impl Key {
    /// Translates a DOM `KeyboardEvent.key` and its modifiers; `None` for keys a terminal
    /// wouldn't report, like Shift on its own.
    pub fn from_dom(key: &str, ctrl: bool, alt: bool, shift: bool) -> Option<Self> {
        let code = match key {
            "Enter" => KeyCode::Enter,
            "Backspace" => KeyCode::Backspace,
            "Tab" if shift => KeyCode::BackTab,
            "Tab" => KeyCode::Tab,
            "Escape" => KeyCode::Esc,
            "ArrowUp" => KeyCode::Up,
            "ArrowDown" => KeyCode::Down,
            "ArrowLeft" => KeyCode::Left,
            "ArrowRight" => KeyCode::Right,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            "Delete" => KeyCode::Delete,
            "Insert" => KeyCode::Insert,
            _ => {
                let mut chars = key.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    (Some('F'), Some(_)) => KeyCode::F(key[1..].parse().ok()?),
                    _ => return None,
                }
            }
        };
        Some(Self {
            code,
            ctrl,
            alt,
            shift,
        })
    }
}

pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    postcard::to_allocvec(message).expect("messages serialize")
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Option<T> {
    postcard::from_bytes(bytes).ok()
}
