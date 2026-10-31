//! A ratatui backend that records what each draw changes, for sending to the browser.

use std::convert::Infallible;

use ratatui::backend::{Backend, ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use tamale_web_protocol::{self as protocol, Frame};

pub struct FrameBackend {
    size: Size,
    frame: Frame,
    cursor: Position,
    cursor_visible: bool,
    /// The cursor the client last got, to only send it when it changes.
    sent_cursor: Option<(u16, u16)>,
}

impl FrameBackend {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            size: Size::new(cols, rows),
            frame: Frame::default(),
            cursor: Position::ORIGIN,
            cursor_visible: false,
            sent_cursor: None,
        }
    }

    /// Takes effect at the next draw, which then repaints everything.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.size = Size::new(cols, rows);
    }

    /// The changes since the last call, or `None` if nothing changed.
    pub fn take_frame(&mut self) -> Option<Frame> {
        let cursor = self
            .cursor_visible
            .then_some((self.cursor.x, self.cursor.y));
        if !self.frame.clear && self.frame.cells.is_empty() && cursor == self.sent_cursor {
            return None;
        }
        self.sent_cursor = cursor;
        let mut frame = std::mem::take(&mut self.frame);
        frame.cursor = cursor;
        Some(frame)
    }
}

impl Backend for FrameBackend {
    type Error = Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Infallible>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.frame
            .cells
            .extend(content.map(|(x, y, cell)| protocol::Cell::new(x, y, cell)));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Infallible> {
        self.cursor_visible = false;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Infallible> {
        self.cursor_visible = true;
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Infallible> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Infallible> {
        self.cursor = position.into();
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Infallible> {
        self.frame.clear = true;
        self.frame.cells.clear();
        Ok(())
    }

    /// The viewport is the whole screen, so ratatui only clears all of it.
    fn clear_region(&mut self, _: ClearType) -> Result<(), Infallible> {
        self.clear()
    }

    fn size(&self) -> Result<Size, Infallible> {
        Ok(self.size)
    }

    fn window_size(&mut self) -> Result<WindowSize, Infallible> {
        Ok(WindowSize {
            columns_rows: self.size,
            pixels: Size::default(),
        })
    }

    fn flush(&mut self) -> Result<(), Infallible> {
        Ok(())
    }
}
