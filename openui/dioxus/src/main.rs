//! The viewer: one artifact on its own. The page it's in (see tamale's
//! `src/web/artifacts.rs`) carries the document as JSON in `<script id="openui-artifact">`;
//! this draws it with the same components the web UI uses.

use dioxus::prelude::*;
use openui_dioxus::Standalone;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let page = use_resource(|| async {
        dioxus::document::eval(
            "const e = document.getElementById('openui-artifact'); \
             return e ? JSON.parse(e.textContent) : null;",
        )
        .join::<serde_json::Value>()
        .await
        .ok()
    });
    let got = page.read().clone().flatten();
    let field = |v: &serde_json::Value, k: &str| {
        v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
    };
    match got {
        Some(v) if !v.is_null() => {
            rsx! { Standalone { title: field(&v, "title"), source: field(&v, "source") } }
        }
        Some(_) => rsx! { p { class: "oui-empty", "This page has no artifact in it." } },
        None => rsx! {},
    }
}
