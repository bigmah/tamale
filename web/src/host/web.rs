//! The page's host: the browser, through web-sys. Nothing here evaluates code, so the page
//! keeps a Content Security Policy without `unsafe-eval`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_channel::oneshot;
use js_sys::{Array, Object, Reflect, Uint8Array};
use serde_json::Value;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{
    BinaryType, Blob, BlobPropertyBag, CloseEvent, Document, Element, HtmlElement,
    HtmlTextAreaElement, KeyboardEvent, MessageEvent, WebSocket, Window,
};

use super::{HostEvent, Key, KeyHandler, Place, SocketEvent};
use crate::e2e;

fn window() -> Window {
    web_sys::window().expect("a window")
}

fn document() -> Document {
    window().document().expect("a document")
}

fn by_id(id: &str) -> Option<Element> {
    document().get_element_by_id(id)
}

fn get(target: &JsValue, key: &str) -> JsValue {
    Reflect::get(target, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn set(target: &JsValue, key: &str, value: impl Into<JsValue>) {
    let _ = Reflect::set(target, &JsValue::from_str(key), &value.into());
}

const KEY_STORED: &str = "tamale.e2e.key";

/// Where this page is, from its address: `?t=` is the token; through a hub, `?e2e=1` says the
/// session is encrypted and `#k=` carries its key, which is kept for this tab's reloads.
pub fn place() -> Place {
    let location = window().location();
    let host = location.host().unwrap_or_default();
    let hostname = location.hostname().unwrap_or_default();
    let protocol = location.protocol().unwrap_or_default();
    let search = location.search().unwrap_or_default();
    let hash = location.hash().unwrap_or_default();
    let params = web_sys::UrlSearchParams::new_with_str(&search).ok();
    let token = params.as_ref().and_then(|p| p.get("t")).unwrap_or_default();
    let wants_e2e = params.as_ref().is_some_and(|p| p.has("e2e"));
    let given = hash
        .strip_prefix("#k=")
        .filter(|k| {
            k.len() == 43
                && k.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .map(str::to_string);
    let session = window().session_storage().ok().flatten();
    if let Some(key) = &given {
        let path = format!("{}{}", location.pathname().unwrap_or_default(), search);
        let _ = window()
            .history()
            .and_then(|h| h.replace_state_with_url(&JsValue::NULL, "", Some(&path)));
        if let Some(s) = &session {
            let _ = s.set_item(KEY_STORED, key);
        }
    }
    let key = given.or_else(|| session.and_then(|s| s.get_item(KEY_STORED).ok().flatten()));
    let e2e = if key.is_some() || wants_e2e {
        Some(key.and_then(|k| e2e::decode(&k)))
    } else {
        None
    };
    let scheme = if protocol == "https:" { "wss" } else { "ws" };
    let parent_hub = host
        .split_once('.')
        .map(|(_, rest)| format!("{protocol}//{rest}"));
    Place {
        base: String::new(),
        ws_url: format!(
            "{scheme}://{host}/ws?t={}",
            js_sys::encode_uri_component(&token)
        ),
        sid: hostname.split('.').next().unwrap_or_default().to_string(),
        hub: parent_hub.filter(|_| {
            host.contains('.')
                && !host.starts_with("127.")
                && !hostname.chars().all(|c| c.is_ascii_digit() || c == '.')
        }),
        e2e,
    }
}

pub fn forget_key() {
    if let Some(s) = window().session_storage().ok().flatten() {
        let _ = s.remove_item(KEY_STORED);
    }
}

fn framed() -> bool {
    let w = window();
    match w.parent() {
        Ok(Some(parent)) => !Object::is(&parent, &w),
        _ => false,
    }
}

// ---------------------------------------------------------------- sockets

/// A WebSocket. Its events come on the receiver `connect` returns, in order.
pub struct Socket {
    ws: WebSocket,
    _handlers: Vec<Closure<dyn FnMut(JsValue)>>,
}

impl Socket {
    pub fn connect(url: &str) -> Result<(Socket, UnboundedReceiver<SocketEvent>), String> {
        let ws = WebSocket::new(url).map_err(|e| format!("{e:?}"))?;
        ws.set_binary_type(BinaryType::Arraybuffer);
        let (tx, rx) = unbounded();
        let open = {
            let tx = tx.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |_| {
                let _ = tx.unbounded_send(SocketEvent::Open);
            })
        };
        let message = {
            let tx = tx.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
                let ev: MessageEvent = ev.unchecked_into();
                let data = ev.data();
                let text = match data.as_string() {
                    Some(text) => text,
                    None => String::from_utf8_lossy(&Uint8Array::new(&data).to_vec()).into_owned(),
                };
                let _ = tx.unbounded_send(SocketEvent::Text(text));
            })
        };
        let close = {
            let tx = tx.clone();
            Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
                let ev: CloseEvent = ev.unchecked_into();
                let _ = tx.unbounded_send(SocketEvent::Closed {
                    code: ev.code(),
                    reason: ev.reason(),
                });
            })
        };
        ws.set_onopen(Some(open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(close.as_ref().unchecked_ref()));
        Ok((
            Socket {
                ws,
                _handlers: vec![open, message, close],
            },
            rx,
        ))
    }

    pub fn is_open(&self) -> bool {
        self.ws.ready_state() == WebSocket::OPEN
    }

    pub fn send(&self, text: &str) -> bool {
        self.is_open() && self.ws.send_with_str(text).is_ok()
    }

    /// Closes it without a word to the receiver.
    pub fn close(&self) {
        self.ws.set_onclose(None);
        self.ws.set_onmessage(None);
        let _ = self.ws.close();
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.close();
    }
}

// ---------------------------------------------------------------- time and storage

pub fn now_ms() -> f64 {
    js_sys::Date::now()
}

pub async fn sleep(ms: u64) {
    gloo_timers::future::TimeoutFuture::new(ms.min(u32::MAX as u64) as u32).await;
}

/// The next frame, once it's painted.
pub async fn next_frame() {
    let (tx, rx) = oneshot::channel::<()>();
    let tx = Cell::new(Some(tx));
    let f = Closure::once(move |_: JsValue| {
        if let Some(tx) = tx.take() {
            let _ = tx.send(());
        }
    });
    let _ = window().request_animation_frame(f.as_ref().unchecked_ref());
    let _ = rx.await;
    drop(f);
}

pub fn store_get(key: &str) -> Option<Value> {
    let storage = window().local_storage().ok().flatten()?;
    let raw = storage.get_item(&format!("tamale.{key}")).ok().flatten()?;
    serde_json::from_str(&raw).ok()
}

pub fn store_set(key: &str, value: &Value) {
    if let Some(storage) = window().local_storage().ok().flatten() {
        let _ = storage.set_item(&format!("tamale.{key}"), &value.to_string());
    }
}

pub fn local_date(secs: f64) -> String {
    js_sys::Date::new(&JsValue::from_f64(secs * 1000.0))
        .to_locale_date_string("default", &JsValue::UNDEFINED)
        .as_string()
        .unwrap_or_default()
}

// ---------------------------------------------------------------- the document

pub fn set_title(title: &str) {
    document().set_title(title);
}

pub fn body_class(name: &str, on: bool) {
    if let Some(body) = document().body() {
        let _ = body.class_list().toggle_with_force(name, on);
    }
}

pub fn html_class(name: &str, on: bool) {
    if let Some(root) = document().document_element() {
        let _ = root.class_list().toggle_with_force(name, on);
    }
}

pub fn theme_color(color: &str) {
    if let Ok(Some(meta)) = document().query_selector("meta[name=\"theme-color\"]") {
        let _ = meta.set_attribute("content", color);
    }
}

pub fn has_focus() -> bool {
    document().has_focus().unwrap_or(true)
}

pub fn page_hidden() -> bool {
    document().hidden()
}

pub fn secure_context() -> bool {
    window().is_secure_context()
}

pub fn matches(query: &str) -> bool {
    window()
        .match_media(query)
        .ok()
        .flatten()
        .is_some_and(|m| m.matches())
}

/// A phone or tablet: no hover, a finger for a pointer, an on-screen keyboard.
pub fn touch() -> bool {
    matches("(hover: none) and (pointer: coarse)")
}

pub fn inner_height() -> f64 {
    window()
        .inner_height()
        .ok()
        .and_then(|h| h.as_f64())
        .unwrap_or(800.0)
}

/// Sizes the app to the visible part of the page. iOS doesn't shrink the layout viewport
/// for the on-screen keyboard (it pans the page instead), so without this the composer
/// hides behind the keyboard and the top bar scrolls away. True if the height changed.
pub fn fit_viewport() -> bool {
    let w = window();
    let Some(vv) = w.visual_viewport() else {
        return false;
    };
    if (vv.scale() - 1.0).abs() > 0.01 {
        return false;
    }
    let height = format!("{}px", vv.height().round());
    let mut changed = false;
    if let Some(root) = document()
        .document_element()
        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
    {
        let style = root.style();
        if style.get_property_value("--app-h").unwrap_or_default() != height {
            let _ = style.set_property("--app-h", &height);
            changed = true;
        }
    }
    if w.scroll_y().unwrap_or(0.0) != 0.0 {
        w.scroll_to_with_x_and_y(0.0, 0.0);
    }
    changed
}

pub fn focus(id: &str) {
    if let Some(el) = by_id(id).and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
        let _ = el.focus();
    }
}

pub fn blur(id: &str) {
    if let Some(el) = by_id(id).and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
        let _ = el.blur();
    }
}

/// Sets (or with `None` removes) a style property of the element with `id`, through the
/// CSSOM, which the page's Content Security Policy allows where a `style` attribute isn't.
pub fn set_style(id: &str, property: &str, value: Option<&str>) {
    if let Some(el) = by_id(id).and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
        let style = el.style();
        match value {
            Some(v) => {
                let _ = style.set_property(property, v);
            }
            None => {
                let _ = style.remove_property(property);
            }
        }
    }
}

/// Takes the focus from whatever has it.
pub fn blur_active() {
    if let Some(el) = document()
        .active_element()
        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
    {
        let _ = el.blur();
    }
}

/// Whether something matches `selector`.
pub fn exists(selector: &str) -> bool {
    document().query_selector(selector).ok().flatten().is_some()
}

/// Scrolls the first match for `selector` to the top of its scroller, smoothly.
pub fn scroll_selector_into_view(selector: &str) {
    if let Ok(Some(e)) = document().query_selector(selector) {
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Start);
        options.set_behavior(web_sys::ScrollBehavior::Smooth);
        e.scroll_into_view_with_scroll_into_view_options(&options);
    }
}

/// The code view's element: everything pullspace draws is inside it.
const CODE: &str = "review";

thread_local! {
    /// Whether the pointer last went down in the code view.
    static POINTER_IN_CODE: Cell<bool> = const { Cell::new(false) };
}

/// Focuses pullspace, so keys go to it.
pub fn focus_code() {
    if let Some(code) = document()
        .query_selector(&format!("#{CODE} .pullspace"))
        .ok()
        .flatten()
        .and_then(|e| e.dyn_into::<HtmlElement>().ok())
    {
        let _ = code.focus();
        POINTER_IN_CODE.set(true);
    }
}

pub fn near_bottom(id: &str, within: f64) -> bool {
    by_id(id).is_none_or(|e| {
        f64::from(e.scroll_height()) - e.scroll_top() as f64 - f64::from(e.client_height()) < within
    })
}

pub fn scroll_to_bottom(id: &str, smooth: bool) {
    if let Some(e) = by_id(id) {
        let options = web_sys::ScrollToOptions::new();
        options.set_top(f64::from(e.scroll_height()));
        options.set_behavior(if smooth {
            web_sys::ScrollBehavior::Smooth
        } else {
            web_sys::ScrollBehavior::Auto
        });
        e.scroll_to_with_scroll_to_options(&options);
    }
}

/// Scrolls the element with `id` into view: `start` puts it at the top, else as little as
/// will do.
pub fn scroll_into_view(id: &str, start: bool) {
    if let Some(e) = by_id(id) {
        let options = web_sys::ScrollIntoViewOptions::new();
        if start {
            options.set_block(web_sys::ScrollLogicalPosition::Start);
            options.set_behavior(web_sys::ScrollBehavior::Smooth);
        } else {
            options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        }
        e.scroll_into_view_with_scroll_into_view_options(&options);
    }
}

/// The element's box: left, top, width, height.
pub fn rect(id: &str) -> Option<(f64, f64, f64, f64)> {
    let r = by_id(id)?.get_bounding_client_rect();
    Some((r.left(), r.top(), r.width(), r.height()))
}

/// Grows the prompt with what's typed, up to 40% of the window.
pub fn autosize(id: &str) {
    if let Some(el) = by_id(id).and_then(|e| e.dyn_into::<HtmlElement>().ok()) {
        let style = el.style();
        let _ = style.set_property("height", "auto");
        let height = (f64::from(el.scroll_height()) + 2.0).min(inner_height() * 0.4);
        let _ = style.set_property("height", &format!("{height}px"));
    }
}

/// Where the text field's selection starts and ends, in UTF-16 units.
pub fn selection(id: &str) -> (usize, usize) {
    by_id(id)
        .and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok())
        .map(|t| {
            (
                t.selection_start().ok().flatten().unwrap_or(0) as usize,
                t.selection_end().ok().flatten().unwrap_or(0) as usize,
            )
        })
        .unwrap_or((0, 0))
}

/// Puts the caret at the end of the prompt (or `at`, in UTF-16 units), and the end in view.
pub fn set_caret(id: &str, at: Option<usize>) {
    if let Some(t) = by_id(id).and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok()) {
        let end = at.unwrap_or_else(|| t.value().encode_utf16().count()) as u32;
        let _ = t.set_selection_range(end, end);
        if at.is_none() {
            t.set_scroll_top(t.scroll_height());
        }
    }
}

/// Sets the text field's value directly, for when it must change before the next render.
pub fn set_value(id: &str, value: &str) {
    if let Some(t) = by_id(id).and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok()) {
        if t.value() != value {
            t.set_value(value);
        }
    } else if let Some(t) = by_id(id).and_then(|e| e.dyn_into::<web_sys::HtmlInputElement>().ok()) {
        if t.value() != value {
            t.set_value(value);
        }
    }
}

pub async fn copy_text(text: &str) -> bool {
    let clipboard = window().navigator().clipboard();
    if get(&clipboard, "writeText").is_undefined() {
        return false;
    }
    wasm_bindgen_futures::JsFuture::from(clipboard.write_text(text))
        .await
        .is_ok()
}

/// In the desktop app (`shell.rs`), which says so before the page starts.
pub fn desktop() -> bool {
    Reflect::get(&window(), &"tamaleDesktop".into()).is_ok_and(|v| v.is_truthy())
}

/// Takes the desktop app back to its list of sessions.
pub fn desktop_home() {
    let ipc = Reflect::get(&window(), &"ipc".into()).ok();
    let post = ipc
        .as_ref()
        .and_then(|ipc| Reflect::get(ipc, &"postMessage".into()).ok())
        .and_then(|f| f.dyn_into::<js_sys::Function>().ok());
    if let (Some(ipc), Some(post)) = (ipc, post) {
        let _ = post.call1(&ipc, &r#"{"do":"home"}"#.into());
    }
}

pub fn open_tab(url: &str) -> bool {
    window()
        .open_with_url_and_target_and_features(url, "_blank", "noopener,noreferrer")
        .is_ok()
}

/// A `blob:` URL for `bytes`.
pub fn blob_url(bytes: &[u8], mime: &str) -> Option<String> {
    let parts = Array::new();
    parts.push(&Uint8Array::from(bytes));
    let bag = BlobPropertyBag::new();
    bag.set_type(mime);
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &bag).ok()?;
    web_sys::Url::create_object_url_with_blob(&blob).ok()
}

/// Has the browser save `bytes` as `name`.
pub fn download(name: &str, bytes: &[u8], mime: &str) {
    let Some(url) = blob_url(bytes, mime) else {
        return;
    };
    if let Ok(link) = document().create_element("a") {
        let _ = link.set_attribute("href", &url);
        let _ = link.set_attribute("download", name);
        if let Ok(link) = link.dyn_into::<HtmlElement>() {
            link.click();
        }
    }
    let revoke = Closure::once(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(
        revoke.as_ref().unchecked_ref(),
        10_000,
    );
    revoke.forget();
}

/// Resolves once the element with `id` comes within 600px of the screen.
pub async fn visible(id: &str) {
    let Some(el) = by_id(id) else { return };
    let (tx, rx) = oneshot::channel::<()>();
    let tx = Cell::new(Some(tx));
    let seen = Closure::<dyn FnMut(Array, web_sys::IntersectionObserver)>::new(
        move |items: Array, obs: web_sys::IntersectionObserver| {
            let any = items
                .iter()
                .any(|i| get(&i, "isIntersecting").as_bool() == Some(true));
            if any {
                obs.disconnect();
                if let Some(tx) = tx.take() {
                    let _ = tx.send(());
                }
            }
        },
    );
    let options = web_sys::IntersectionObserverInit::new();
    options.set_root_margin("600px 0px");
    let Ok(observer) =
        web_sys::IntersectionObserver::new_with_options(seen.as_ref().unchecked_ref(), &options)
    else {
        return;
    };
    observer.observe(&el);
    let _ = rx.await;
    drop(seen);
}

// ---------------------------------------------------------------- history

thread_local! {
    /// History entries pushed for layers and not yet gone back over, and pops to ignore.
    static HISTORY: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}

/// Brings the history entries in line with `layers` open overlays, so the browser's Back
/// closes them, innermost first.
pub fn sync_history(layers: usize) {
    let Ok(history) = window().history() else {
        return;
    };
    let (mut entries, mut skip) = HISTORY.get();
    while entries < layers {
        entries += 1;
        let state = Object::new();
        set(&state, "tamaleLayer", entries as f64);
        let _ = history.push_state(&state, "");
    }
    if entries > layers {
        skip += 1;
        let _ = history.go_with_delta(layers as i32 - entries as i32);
        entries = layers;
    }
    HISTORY.set((entries, skip));
}

// ---------------------------------------------------------------- the hub's page

thread_local! {
    static TOLD_SHOWN: Cell<bool> = const { Cell::new(false) };
}

fn hub_origin() -> Option<String> {
    let location = window().location();
    let host = location.host().ok()?;
    let (_, rest) = host.split_once('.')?;
    Some(format!("{}//{rest}", location.protocol().ok()?))
}

/// In a hub's page, which keeps this frame out of sight until it's drawn rather than flash
/// an empty page: says it is, once, after the next paint.
pub fn shown() {
    if TOLD_SHOWN.get() || !framed() {
        return;
    }
    TOLD_SHOWN.set(true);
    wasm_bindgen_futures::spawn_local(async {
        next_frame().await;
        sleep(0).await;
        let (Some(hub), Ok(Some(parent))) = (hub_origin(), window().parent()) else {
            return;
        };
        let message = Object::new();
        set(&message, "tamaleShown", true);
        let _ = parent.post_message(&message, &hub);
    });
}

// ---------------------------------------------------------------- events

fn key_of(ev: &KeyboardEvent, prompt: &str) -> Key {
    let target = ev.target().and_then(|t| t.dyn_into::<Element>().ok());
    let tag = target
        .as_ref()
        .map(|t| t.tag_name().to_ascii_lowercase())
        .unwrap_or_default();
    let in_prompt = target.as_ref().is_some_and(|t| t.id() == prompt);
    let code = by_id(CODE);
    let in_code = match (&target, &code) {
        (Some(t), Some(code)) if code.contains(Some(t)) => true,
        // Focus on nothing in particular: where the pointer last went down says.
        (Some(t), Some(code)) => {
            (t.tag_name().eq_ignore_ascii_case("body") || t.tag_name().eq_ignore_ascii_case("html"))
                && POINTER_IN_CODE.get()
                && code.client_width() > 0
        }
        _ => false,
    };
    let prompt_empty = by_id(prompt)
        .and_then(|e| e.dyn_into::<HtmlTextAreaElement>().ok())
        .is_none_or(|t| t.value().is_empty());
    Key {
        key: ev.key(),
        code: ev.code(),
        meta: ev.meta_key(),
        ctrl: ev.ctrl_key(),
        alt: ev.alt_key(),
        shift: ev.shift_key(),
        composing: ev.is_composing(),
        in_field: tag == "input" || tag == "textarea" || tag == "select",
        in_prompt,
        prompt_empty,
        default_prevented: ev.default_prevented(),
        in_code,
    }
}

/// Listens to the window for the rest of the page's life: keys go to `keys` as they're
/// dispatched, everything else to `events`.
pub fn install(keys: KeyHandler, events: UnboundedSender<HostEvent>, prompt: &'static str) {
    let w = window();
    let listen = |target: &web_sys::EventTarget,
                  name: &str,
                  f: Closure<dyn FnMut(JsValue)>,
                  capture: bool| {
        let _ = target.add_event_listener_with_callback_and_bool(
            name,
            f.as_ref().unchecked_ref(),
            capture,
        );
        f.forget();
    };
    let send = |events: &UnboundedSender<HostEvent>, event: HostEvent| {
        let _ = events.unbounded_send(event);
    };

    // Inside a tamale-hub's page, which lists the sessions and says this page's name. Keys
    // go to the frame with focus, so the ones that switch sessions are passed up to it.
    if framed() {
        html_class("framed", true);
        listen(
            &w,
            "keydown",
            Closure::new(move |ev: JsValue| {
                let ev: KeyboardEvent = ev.unchecked_into();
                if !ev.alt_key() || ev.ctrl_key() || ev.meta_key() {
                    return;
                }
                let code = ev.code();
                let digit = code
                    .strip_prefix("Digit")
                    .and_then(|d| d.parse::<u32>().ok())
                    .filter(|d| (1..=9).contains(d));
                let target = match (ev.key().as_str(), digit) {
                    ("ArrowUp", _) => JsValue::from_str("up"),
                    ("ArrowDown", _) => JsValue::from_str("down"),
                    (_, Some(d)) => JsValue::from_f64(f64::from(d)),
                    _ => return,
                };
                ev.prevent_default();
                ev.stop_propagation();
                let (Some(hub), Ok(Some(parent))) = (hub_origin(), window().parent()) else {
                    return;
                };
                let message = Object::new();
                set(&message, "tamaleHubSwitch", target);
                let _ = parent.post_message(&message, &hub);
            }),
            true,
        );
        // Back on this session from the hub's list: carry on typing.
        listen(
            &w,
            "focus",
            Closure::new(move |_| {
                if matches("(pointer: fine)") {
                    focus(prompt);
                }
            }),
            false,
        );
    }

    {
        let keys = keys.clone();
        listen(
            &w,
            "keydown",
            Closure::new(move |ev: JsValue| {
                let ev: KeyboardEvent = ev.unchecked_into();
                if keys(&key_of(&ev, prompt)) {
                    ev.prevent_default();
                }
            }),
            false,
        );
    }
    // Where the pointer last went down, for keys pressed with the focus on nothing: in the
    // code view they're pullspace's.
    listen(
        &document(),
        "pointerdown",
        Closure::new(move |ev: JsValue| {
            let ev: web_sys::Event = ev.unchecked_into();
            let target = ev.target().and_then(|t| t.dyn_into::<web_sys::Node>().ok());
            let inside = by_id(CODE).is_some_and(|code| code.contains(target.as_ref()));
            POINTER_IN_CODE.set(inside);
        }),
        true,
    );
    // File paths in replies (rendered by tamale, so not the app's own elements) open in the
    // code view.
    {
        let events = events.clone();
        listen(
            &document(),
            "click",
            Closure::new(move |ev: JsValue| {
                let ev: web_sys::Event = ev.unchecked_into();
                let Some(target) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
                    return;
                };
                let Ok(Some(link)) = target.closest(".pathlink") else {
                    return;
                };
                ev.stop_propagation();
                let path = link
                    .get_attribute("data-path")
                    .unwrap_or_else(|| link.text_content().unwrap_or_default());
                send(&events, HostEvent::OpenPath { path, diff: false });
            }),
            false,
        );
    }
    {
        let events = events.clone();
        listen(
            &w,
            "popstate",
            Closure::new(move |_| {
                let (entries, skip) = HISTORY.get();
                if skip > 0 {
                    HISTORY.set((entries, skip - 1));
                } else if entries > 0 {
                    HISTORY.set((entries - 1, skip));
                    send(&events, HostEvent::Back);
                }
            }),
            false,
        );
    }
    for name in ["visibilitychange", "online"] {
        let events = events.clone();
        listen(
            &w,
            name,
            Closure::new(move |_| send(&events, HostEvent::Wake)),
            false,
        );
    }
    {
        let events = events.clone();
        listen(
            &w,
            "pageshow",
            Closure::new(move |ev: JsValue| {
                if get(&ev, "persisted").as_bool() == Some(true) {
                    send(&events, HostEvent::Wake);
                }
            }),
            false,
        );
    }
    {
        let events = events.clone();
        listen(
            &w,
            "focus",
            Closure::new(move |_| send(&events, HostEvent::Focus)),
            false,
        );
    }
    for query in [
        "(max-width: 760px)",
        "(max-width: 980px)",
        "(hover: none) and (pointer: coarse)",
    ] {
        if let Ok(Some(list)) = w.match_media(query) {
            let events = events.clone();
            listen(
                &list,
                "change",
                Closure::new(move |_| send(&events, HostEvent::Media)),
                false,
            );
        }
    }
    {
        let events = events.clone();
        listen(
            &w,
            "resize",
            Closure::new(move |_| send(&events, HostEvent::Media)),
            false,
        );
    }
    if let Some(vv) = w.visual_viewport() {
        for name in ["resize", "scroll"] {
            let events = events.clone();
            listen(
                &vv,
                name,
                Closure::new(move |_| send(&events, HostEvent::Viewport)),
                false,
            );
        }
    }
    {
        let events = events.clone();
        listen(
            &w,
            "message",
            Closure::new(move |ev: JsValue| {
                let ev: MessageEvent = ev.unchecked_into();
                if let Some(event) = frame_message(&ev) {
                    send(&events, event);
                }
            }),
            false,
        );
    }
}

/// What a message from the hub's page means, if it's one the app listens to.
fn frame_message(ev: &MessageEvent) -> Option<HostEvent> {
    let data = ev.data();
    let source = ev.source()?;
    let source: JsValue = source.into();
    let w = window();
    // The hub's list closing this session (it asked first).
    if framed() {
        let parent: JsValue = w.parent().ok().flatten()?.into();
        if Object::is(&source, &parent) {
            if hub_origin().is_some_and(|hub| ev.origin() == hub)
                && get(&data, "tamaleClose").as_bool() == Some(true)
            {
                return Some(HostEvent::HubClose);
            }
            return None;
        }
    }
    None
}

// ---------------------------------------------------------------- fetch

/// GETs `url` with `headers`, and reads the answer as JSON: its status and body.
pub async fn fetch_json(url: &str, headers: &[(&str, &str)]) -> Result<(u16, Value), String> {
    let init = web_sys::RequestInit::new();
    let h = web_sys::Headers::new().map_err(|e| format!("{e:?}"))?;
    for (k, v) in headers {
        let _ = h.set(k, v);
    }
    init.set_headers(&h);
    let request =
        web_sys::Request::new_with_str_and_init(url, &init).map_err(|e| format!("{e:?}"))?;
    let response = wasm_bindgen_futures::JsFuture::from(window().fetch_with_request(&request))
        .await
        .map_err(|_| "Couldn't reach Gemini. Check the network.".to_string())?;
    let response: web_sys::Response = response.unchecked_into();
    let status = response.status();
    let body = match response.text() {
        Ok(p) => wasm_bindgen_futures::JsFuture::from(p)
            .await
            .ok()
            .and_then(|t| t.as_string())
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
}

/// Why a conversation can't start without audio.
pub const NO_AUDIO: &str = "This browser can't play audio from a page.";

// ---------------------------------------------------------------- audio

pub use audio::Audio;

mod audio {
    //! The microphone and the speaker, for the voice conversation: the microphone's samples
    //! as they come, and 16-bit PCM played back to back.

    use super::*;
    use web_sys::{
        AudioBufferSourceNode, AudioContext, MediaStream, MediaStreamConstraints,
        ScriptProcessorNode,
    };

    pub struct Audio {
        ctx: AudioContext,
        stream: RefCell<Option<MediaStream>>,
        proc: RefCell<Option<ScriptProcessorNode>>,
        on_mic: RefCell<Option<Closure<dyn FnMut(JsValue)>>>,
        sources: RefCell<Vec<(AudioBufferSourceNode, Closure<dyn FnMut(JsValue)>)>>,
        play_at: Cell<f64>,
    }

    impl Audio {
        /// An audio context, made in a click: only then may a page play sound in every browser.
        pub fn new() -> Option<Rc<Audio>> {
            let ctx = AudioContext::new().ok()?;
            // It may be closed before this settles (no key, say), which rejects it.
            if let Ok(resuming) = ctx.resume() {
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = wasm_bindgen_futures::JsFuture::from(resuming).await;
                });
            }
            Some(Rc::new(Audio {
                ctx,
                stream: RefCell::new(None),
                proc: RefCell::new(None),
                on_mic: RefCell::new(None),
                sources: RefCell::new(Vec::new()),
                play_at: Cell::new(0.0),
            }))
        }

        pub fn sample_rate(&self) -> f64 {
            f64::from(self.ctx.sample_rate())
        }

        /// Starts the microphone: `samples` gets each block of it. An error says why there's
        /// none, in words for the user.
        pub async fn start_mic(
            self: &Rc<Self>,
            samples: Box<dyn Fn(Vec<f32>)>,
        ) -> Result<(), String> {
            let navigator = window().navigator();
            let devices = get(&navigator, "mediaDevices");
            if devices.is_undefined() || get(&devices, "getUserMedia").is_undefined() {
                return Err(if secure_context() {
                    "This browser doesn't give pages a microphone. Type instead.".into()
                } else {
                    "Browsers only give the microphone to secure pages: open this one through the hub, or at this machine's own address (127.0.0.1). Type instead.".into()
                });
            }
            let devices: web_sys::MediaDevices = devices.unchecked_into();
            let audio = Object::new();
            set(&audio, "channelCount", 1);
            set(&audio, "echoCancellation", true);
            set(&audio, "noiseSuppression", true);
            set(&audio, "autoGainControl", true);
            let constraints = MediaStreamConstraints::new();
            constraints.set_audio(&audio);
            let promise = devices
                .get_user_media_with_constraints(&constraints)
                .map_err(|e| format!("{e:?}"))?;
            let stream: MediaStream = match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(s) => s.unchecked_into(),
                Err(e) => {
                    let name = get(&e, "name").as_string().unwrap_or_default();
                    let message = get(&e, "message").as_string().unwrap_or_default();
                    return Err(if name == "NotAllowedError" {
                        "The microphone isn't allowed here; allow it in the browser's site settings, or type instead.".into()
                    } else {
                        format!("No microphone ({message}). Type instead.")
                    });
                }
            };
            let source = self
                .ctx
                .create_media_stream_source(&stream)
                .map_err(|e| format!("{e:?}"))?;
            // A script processor rather than a worklet: a worklet's module would be a file of
            // its own, which a hub doesn't pass through. 4096 frames is under 100 ms.
            let proc = self
                .ctx
                .create_script_processor_with_buffer_size_and_number_of_input_channels_and_number_of_output_channels(4096, 1, 1)
                .map_err(|e| format!("{e:?}"))?;
            let silent = self.ctx.create_gain().map_err(|e| format!("{e:?}"))?;
            silent.gain().set_value(0.0);
            let _ = source.connect_with_audio_node(&proc);
            let _ = proc.connect_with_audio_node(&silent);
            let _ = silent.connect_with_audio_node(&self.ctx.destination());
            let f = Closure::<dyn FnMut(JsValue)>::new(move |ev: JsValue| {
                let ev: web_sys::AudioProcessingEvent = ev.unchecked_into();
                if let Ok(buffer) = ev.input_buffer()
                    && let Ok(data) = buffer.get_channel_data(0)
                {
                    samples(data);
                }
            });
            proc.set_onaudioprocess(Some(f.as_ref().unchecked_ref()));
            *self.on_mic.borrow_mut() = Some(f);
            *self.proc.borrow_mut() = Some(proc);
            *self.stream.borrow_mut() = Some(stream);
            Ok(())
        }

        /// Plays 16-bit little-endian PCM at `rate` after what's already queued. `ended` runs
        /// when the last queued piece has played.
        pub fn play(self: &Rc<Self>, pcm: &[u8], rate: f32, ended: Rc<dyn Fn()>) -> bool {
            let frames = pcm.len() / 2;
            if frames == 0 {
                return false;
            }
            let Ok(buffer) = self.ctx.create_buffer(1, frames as u32, rate) else {
                return false;
            };
            let mut out: Vec<f32> = Vec::with_capacity(frames);
            for pair in pcm.chunks_exact(2) {
                out.push(f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0);
            }
            let _ = buffer.copy_to_channel(&mut out, 0);
            let Ok(source) = self.ctx.create_buffer_source() else {
                return false;
            };
            source.set_buffer(Some(&buffer));
            let _ = source.connect_with_audio_node(&self.ctx.destination());
            let at = (self.ctx.current_time() + 0.02).max(self.play_at.get());
            let _ = source.start_with_when(at);
            self.play_at.set(at + buffer.duration());
            let me = Rc::downgrade(self);
            let node = source.clone();
            let done = Closure::<dyn FnMut(JsValue)>::new(move |_| {
                if let Some(me) = me.upgrade() {
                    let empty = {
                        let mut sources = me.sources.borrow_mut();
                        sources.retain(|(s, _)| !Object::is(s, &node));
                        sources.is_empty()
                    };
                    if empty {
                        ended();
                    }
                }
            });
            #[allow(deprecated)]
            source.set_onended(Some(done.as_ref().unchecked_ref()));
            self.sources.borrow_mut().push((source, done));
            true
        }

        pub fn playing(&self) -> bool {
            !self.sources.borrow().is_empty()
        }

        /// Whatever was playing stops now.
        pub fn interrupt(&self) {
            for (source, _) in self.sources.borrow_mut().drain(..) {
                #[allow(deprecated)]
                source.set_onended(None);
                #[allow(deprecated)]
                let _ = source.stop();
            }
            self.play_at.set(0.0);
        }

        pub fn close(&self) {
            self.interrupt();
            if let Some(stream) = self.stream.borrow_mut().take() {
                for track in stream.get_tracks().iter() {
                    let track: web_sys::MediaStreamTrack = track.unchecked_into();
                    track.stop();
                }
            }
            if let Some(proc) = self.proc.borrow_mut().take() {
                proc.set_onaudioprocess(None);
                let _ = proc.disconnect();
            }
            self.on_mic.borrow_mut().take();
            // Once: closing a closed context rejects, and nothing would catch it.
            if self.ctx.state() != web_sys::AudioContextState::Closed
                && let Ok(closing) = self.ctx.close()
            {
                wasm_bindgen_futures::spawn_local(async move {
                    let _ = wasm_bindgen_futures::JsFuture::from(closing).await;
                });
            }
        }
    }
}
