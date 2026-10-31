//! tamale's browser client. tamale sends the cells that change on its screen over a
//! WebSocket; the client keeps a copy of the screen, draws it with ratzilla's WebGL2
//! backend, and sends back its size, key presses, pastes, the mouse wheel and clicks.

use std::cell::RefCell;
use std::rc::Rc;

use ratzilla::WebRenderer;
use ratzilla::backend::cell_sized::CellSized;
use ratzilla::backend::webgl2::{
    FontAtlasConfig, SelectionMode, WebGl2Backend, WebGl2BackendOptions,
};
use ratzilla::ratatui::Terminal;
use ratzilla::ratatui::buffer::Buffer;
use ratzilla::ratatui::layout::Rect;
use ratzilla::ratatui::style::{Color, Style};
use ratzilla::ratatui::widgets::{Clear, Paragraph};
use tamale_web_protocol::{self as protocol, ClientMessage, Key, ServerMessage};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    BinaryType, ClipboardEvent, CloseEvent, CompositionEvent, Document, HtmlTextAreaElement,
    InputEvent, KeyboardEvent, MessageEvent, MouseEvent, WebSocket, WheelEvent,
};

const FONTS: &[&str] = &[
    "Menlo",
    "SF Mono",
    "Monaco",
    "Consolas",
    "DejaVu Sans Mono",
    "monospace",
];
const FONT_SIZE: f32 = 14.0;
/// The 16 named colors (Tango, like xterm.js), and the default text and background.
const PALETTE: [u32; 16] = [
    0x2e3436, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, 0x555753,
    0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
];
const FOREGROUND: u32 = 0xeeeeec;
const BACKGROUND: u32 = 0x111111;
/// Wheel distance, in pixels, that scrolls one step; a mouse notch is about 100.
const WHEEL_STEP: f64 = 60.0;

struct State {
    socket: WebSocket,
    /// The screen as tamale last drew it.
    screen: Buffer,
    cursor: Option<(u16, u16)>,
    /// The size last reported to tamale.
    sent_size: Option<(u16, u16)>,
    /// Why the connection ended, once it has.
    closed: Option<String>,
    /// Wheel distance not yet sent as a step.
    wheel: f64,
}

impl State {
    fn send(&self, message: &ClientMessage) {
        if self.socket.ready_state() == WebSocket::OPEN {
            let _ = self.socket.send_with_u8_array(&protocol::encode(message));
        }
    }

    fn apply(&mut self, frame: protocol::Frame) {
        if frame.clear {
            self.screen = blank(self.screen.area);
        }
        let area = self.screen.area;
        // Cells drawn for an older size than ours are dropped; tamale redraws on resize.
        for cell in frame
            .cells
            .iter()
            .filter(|c| c.x < area.width && c.y < area.height)
        {
            let mut themed = cell.to_cell();
            themed.fg = theme(cell.fg, FOREGROUND);
            themed.bg = theme(cell.bg, BACKGROUND);
            self.screen[(cell.x, cell.y)] = themed;
        }
        self.cursor = frame.cursor;
    }
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;

    let backend = WebGl2Backend::new_with_options(
        WebGl2BackendOptions::new()
            .grid_id("grid")
            .canvas_padding_color(rgb(BACKGROUND))
            // The page's CSS sizes the canvas to the window; the grid follows it.
            .disable_auto_css_resize()
            .font_atlas_config(FontAtlasConfig::dynamic(FONTS, FONT_SIZE))
            .enable_mouse_selection_with_mode(SelectionMode::Linear),
    )
    .map_err(|e| e.to_string())?;
    let (cell_width, cell_height) = backend.cell_size_css_px();
    let terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let location = window.location();
    let socket = WebSocket::new(&format!(
        "ws://{}/ws{}",
        location.host()?,
        location.search()?
    ))?;
    socket.set_binary_type(BinaryType::Arraybuffer);
    let state = Rc::new(RefCell::new(State {
        socket: socket.clone(),
        screen: blank(Rect::ZERO),
        cursor: None,
        sent_size: None,
        closed: None,
        wheel: 0.0,
    }));

    listen(&socket, "message", {
        let state = state.clone();
        move |event: MessageEvent| {
            let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
            if let Some(ServerMessage::Frame(frame)) = protocol::decode(&bytes) {
                state.borrow_mut().apply(frame);
            }
        }
    });
    listen(&socket, "close", {
        let state = state.clone();
        move |event: CloseEvent| {
            let reason = event.reason();
            state.borrow_mut().closed = Some(if reason.is_empty() {
                "Disconnected from tamale. Run /browser in tamale for a new link.".into()
            } else {
                reason
            });
        }
    });

    let input = text_input(&document)?;
    listen_input(&input, &state);
    // Keep typing going to the input after clicks and window switches.
    listen(&document, "mouseup", {
        let input = input.clone();
        move |_: MouseEvent| {
            let _ = input.focus();
        }
    });
    listen(&window, "focus", {
        let input = input.clone();
        move |_: web_sys::Event| {
            let _ = input.focus();
        }
    });

    let grid = document.get_element_by_id("grid").ok_or("no #grid")?;
    // The grid's cell under the pointer.
    let cell_at = {
        let grid = grid.clone();
        move |event: &MouseEvent| {
            let rect = grid.get_bounding_client_rect();
            let col = (event.client_x() as f64 - rect.left()) / cell_width as f64;
            let row = (event.client_y() as f64 - rect.top()) / cell_height as f64;
            (col.max(0.0) as u16, row.max(0.0) as u16)
        }
    };
    listen(&grid, "wheel", {
        let state = state.clone();
        let cell_at = cell_at.clone();
        move |event: WheelEvent| {
            event.prevent_default();
            let mut state = state.borrow_mut();
            state.wheel += match event.delta_mode() {
                WheelEvent::DOM_DELTA_LINE => event.delta_y() * 33.0,
                _ => event.delta_y(),
            };
            let (col, row) = cell_at(&event);
            while state.wheel.abs() >= WHEEL_STEP {
                let up = state.wheel < 0.0;
                state.send(&ClientMessage::Wheel { up, col, row });
                state.wheel -= WHEEL_STEP.copysign(state.wheel);
            }
        }
    });
    listen(&grid, "mousedown", {
        let state = state.clone();
        move |event: MouseEvent| {
            if event.button() == 0 {
                let (col, row) = cell_at(&event);
                state.borrow().send(&ClientMessage::Click { col, row });
            }
        }
    });

    terminal.draw_web(move |frame| {
        let mut state = state.borrow_mut();
        let area = frame.area();
        if state.screen.area != area {
            state.screen = blank(area);
            state.sent_size = None;
        }
        let size = (area.width, area.height);
        if state.sent_size != Some(size) && state.socket.ready_state() == WebSocket::OPEN {
            state.send(&ClientMessage::Size {
                cols: size.0,
                rows: size.1,
            });
            state.sent_size = Some(size);
        }
        frame.buffer_mut().clone_from(&state.screen);
        let Some(reason) = &state.closed else {
            if let Some(cursor) = state.cursor {
                frame.set_cursor_position(cursor);
            }
            return;
        };
        let bottom = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
        frame.render_widget(Clear, bottom);
        frame.render_widget(
            Paragraph::new(reason.as_str())
                .centered()
                .style(Style::new().fg(rgb(FOREGROUND)).bg(rgb(PALETTE[8]))),
            bottom,
        );
    });
    Ok(())
}

/// An empty screen in the theme's colors.
fn blank(area: Rect) -> Buffer {
    let mut cell = ratzilla::ratatui::buffer::Cell::default();
    cell.fg = rgb(FOREGROUND);
    cell.bg = rgb(BACKGROUND);
    Buffer::filled(area, cell)
}

fn rgb(color: u32) -> Color {
    let [_, r, g, b] = color.to_be_bytes();
    Color::Rgb(r, g, b)
}

/// The color to draw: named colors and the default (`Reset`) come from the theme.
fn theme(color: protocol::Color, default: u32) -> Color {
    match color {
        protocol::Color::Reset => rgb(default),
        protocol::Color::Indexed(i) if i < 16 => rgb(PALETTE[i as usize]),
        other => other.into(),
    }
}

/// A hidden text area that takes keyboard focus, so input methods and paste work.
fn text_input(document: &Document) -> Result<HtmlTextAreaElement, JsValue> {
    let input: HtmlTextAreaElement = document.create_element("textarea")?.unchecked_into();
    for (name, value) in [
        ("autocapitalize", "off"),
        ("autocomplete", "off"),
        ("autocorrect", "off"),
        ("spellcheck", "false"),
        ("aria-label", "tamale input"),
    ] {
        input.set_attribute(name, value)?;
    }
    input.set_attribute(
        "style",
        "position: fixed; left: 0; top: 0; width: 1px; height: 1px; opacity: 0; \
         border: 0; padding: 0; resize: none; overflow: hidden;",
    )?;
    document.body().ok_or("no body")?.append_child(&input)?;
    input.focus()?;
    Ok(input)
}

fn listen_input(input: &HtmlTextAreaElement, state: &Rc<RefCell<State>>) {
    // Keys go to tamale as a terminal would report them. Cmd shortcuts (copy, paste,
    // closing the tab) stay with the browser, and input methods compose in the text area.
    listen(input, "keydown", {
        let state = state.clone();
        move |event: KeyboardEvent| {
            if event.is_composing() || event.key_code() == 229 || event.meta_key() {
                return;
            }
            let key = Key::from_dom(
                &event.key(),
                event.ctrl_key(),
                event.alt_key(),
                event.shift_key(),
            );
            if let Some(key) = key {
                event.prevent_default();
                state.borrow().send(&ClientMessage::Key(key));
            }
        }
    });
    // Composed text arrives at compositionend in some browsers and as a final input
    // event in others; either way the text area is emptied once it's sent.
    let send_text = {
        let state = state.clone();
        let input = input.clone();
        move || {
            let text = input.value();
            if !text.is_empty() {
                input.set_value("");
                state.borrow().send(&ClientMessage::Text(text));
            }
        }
    };
    listen(input, "compositionend", {
        let send_text = send_text.clone();
        move |_: CompositionEvent| send_text()
    });
    listen(input, "input", move |event: InputEvent| {
        if !event.is_composing() {
            send_text();
        }
    });
    listen(input, "paste", {
        let state = state.clone();
        move |event: ClipboardEvent| {
            event.prevent_default();
            if let Some(text) = event
                .clipboard_data()
                .and_then(|d| d.get_data("text/plain").ok())
            {
                state.borrow().send(&ClientMessage::Paste(text));
            }
        }
    });
}

/// Adds an event listener for the page's lifetime. It isn't passive, so it can cancel the
/// browser's default, like scrolling on wheel.
fn listen<E: JsCast + 'static>(
    target: &web_sys::EventTarget,
    name: &str,
    mut handler: impl FnMut(E) + 'static,
) {
    let closure = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        handler(event.unchecked_into())
    });
    let options = web_sys::AddEventListenerOptions::new();
    options.set_passive(false);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        name,
        closure.as_ref().unchecked_ref(),
        &options,
    );
    closure.forget();
}
