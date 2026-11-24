//! The voice card above the prompt: the conversation with Gemini, or its settings.

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use serde_json::json;

use crate::app::App;
use crate::host;
use crate::icons::{I, icon};
use crate::voice::{GEMINI_KEY_URL, GEMINI_VOICES, Phase, Who};

#[component]
pub fn VoiceCard() -> Element {
    let app = use_context::<App>();
    if *app.voice.settings_view.read() {
        rsx! { VoiceSettings {} }
    } else {
        rsx! { VoiceTalk {} }
    }
}

#[component]
fn VoiceTalk() -> Element {
    let app = use_context::<App>();
    let mut voice = app.voice;
    let talk = voice.talk.read().clone();
    let level = *voice.level.read();
    let folded = *voice.folded.read();
    let topic = voice.topic.read().clone();
    let mut typed = use_signal(String::new);
    let active = talk.phase.active();
    let class = format!(
        "voice{}{}{}",
        if folded { " folded" } else { "" },
        if talk.phase == Phase::Speaking {
            " speaking"
        } else {
            ""
        },
        if active { " active" } else { "" }
    );
    let status = match talk.phase {
        Phase::Idle => "Starting…",
        Phase::Connecting => "Connecting…",
        Phase::Listening if !talk.mic_off.is_empty() => "Type to Gemini",
        Phase::Listening if talk.muted => "Muted",
        Phase::Listening => "Listening",
        Phase::Speaking => "Gemini is speaking. Talk to interrupt.",
        Phase::Ended => "Ended",
    };
    let title = match &topic {
        Some(t) => format!("Gemini · {}", t.title),
        None => "Gemini".into(),
    };
    // The orb follows your voice, and glows while Gemini speaks.
    let orb = if talk.phase == Phase::Speaking {
        0.5
    } else {
        level
    };
    let mut notes: Vec<(&str, String)> = Vec::new();
    if talk.lines.is_empty() && talk.phase == Phase::Listening {
        notes.push((
            "hint",
            match &topic {
                Some(t) => format!("Ask anything about “{}”.", t.title),
                None => "Ask what tamale is doing, or tell it what to do.".into(),
            },
        ));
    }
    if !talk.mic_off.is_empty() {
        notes.push(("warn", talk.mic_off.clone()));
    }
    if !talk.notice.is_empty() {
        notes.push(("warn", talk.notice.clone()));
    }
    if talk.phase == Phase::Ended && !talk.reason.is_empty() {
        notes.push(("error", talk.reason.clone()));
    }
    let mut send_typed = move || {
        if voice.send_text(&typed.peek()) {
            typed.set(String::new());
        }
    };
    use_effect(use_reactive!(|orb| host::set_style(
        "vorb",
        "--level",
        Some(&format!("{orb:.2}"))
    )));
    let lines_len = talk.lines.len();
    use_effect(use_reactive!(|lines_len| {
        let _ = lines_len;
        host::scroll_to_bottom("vlines", false);
    }));
    let placeholder = if talk.ready {
        if talk.mic_off.is_empty() {
            "Or type to Gemini…"
        } else {
            "Type to Gemini…"
        }
    } else {
        ""
    };
    rsx! {
        div { class: "{class}", id: "voice",
            div { class: "vcard",
                div {
                    class: "vhead",
                    onclick: move |_| {
                        let f = *voice.folded.peek();
                        voice.folded.set(!f);
                    },
                    span { class: "vorb", id: "vorb" }
                    span { class: "vnames", span { class: "vtitle", "{title}" } span { class: "vstatus", "{status}" } }
                    span { class: "vmodel", "{talk.model}" }
                    span { class: "spacer" }
                    button {
                        class: "vbtn",
                        title: "Gemini settings",
                        "aria-label": "Gemini settings",
                        onclick: move |ev| {
                            ev.stop_propagation();
                            spawn_forever(async move {
                                let _ = voice.load_settings(app).await;
                                voice.settings_view.set(true);
                            });
                        },
                        {icon(I::Gear)}
                    }
                    button { class: "vbtn", title: "Close", "aria-label": "Close", onclick: move |ev| { ev.stop_propagation(); voice.close() }, {icon(I::Close)} }
                }
                div { class: "vbody",
                    div { class: "vlines", id: "vlines", "aria-live": "polite",
                        for line in talk.lines.iter() {
                            match line.who {
                                Who::Action => rsx! { div { class: "vline action", "{line.text}" } },
                                Who::You => rsx! { div { class: "vline you", span { class: "vwho", "You" } span { class: "vtext", "{line.text}" } } },
                                Who::Gemini => rsx! { div { class: "vline gemini", span { class: "vwho", "Gemini" } span { class: "vtext", "{line.text}" } } },
                            }
                        }
                    }
                    div { class: "vnotes",
                        for (kind, text) in notes {
                            div { class: "vnote {kind}", "{text}" }
                        }
                    }
                }
                div { class: "vcontrols",
                    input {
                        class: "vfield",
                        enterkeyhint: "send",
                        autocomplete: "off",
                        disabled: !talk.ready,
                        placeholder: "{placeholder}",
                        value: "{typed}",
                        oninput: move |ev| typed.set(ev.value()),
                        onkeydown: move |ev| {
                            if ev.key() == Key::Enter && !ev.is_composing() {
                                ev.prevent_default();
                                send_typed();
                            }
                            ev.stop_propagation();
                        },
                    }
                    button { class: "vbtn vsend", title: "Send to Gemini", "aria-label": "Send to Gemini", onclick: move |_| send_typed(), {icon(I::Up)} }
                    button {
                        class: if talk.muted { "vbtn on" } else { "vbtn" },
                        title: if talk.muted { "Unmute" } else { "Mute" },
                        "aria-label": if talk.muted { "Unmute" } else { "Mute" },
                        hidden: !active || !talk.mic_off.is_empty(),
                        onclick: move |_| {
                            let muted = voice.talk.peek().muted;
                            voice.set_muted(!muted);
                        },
                        if talk.muted { {icon(I::MicOff)} } else { {icon(I::Mic)} }
                    }
                    button {
                        class: if active { "vbtn danger" } else { "vbtn" },
                        title: if active { "End the conversation" } else { "Start again" },
                        "aria-label": if active { "End the conversation" } else { "Start again" },
                        onclick: move |_| {
                            if voice.active() {
                                voice.stop();
                            } else {
                                let topic = voice.topic.peek().clone();
                                voice.start(app, topic, host::Audio::new());
                            }
                        },
                        if active { {icon(I::Hangup)} } else { {icon(I::Reload)} }
                    }
                }
            }
        }
    }
}

#[component]
fn VoiceSettings() -> Element {
    let app = use_context::<App>();
    let mut voice = app.voice;
    let s = voice.settings.read().clone().unwrap_or_default();
    let note_top = voice.note.read().clone();
    let mut key = use_signal(String::new);
    let mut model = use_signal(|| s.model.clone().unwrap_or_default());
    let mut chosen_voice = use_signal(|| s.voice.clone().unwrap_or_default());
    let mut note = use_signal(String::new);
    let has_key = s.key.as_ref().is_some_and(|k| !k.is_empty());
    let key_for_models = s.key.clone().unwrap_or_default();
    // The models this key can use, once Google says.
    let models = use_resource(move || {
        let key = key_for_models.clone();
        async move {
            if key.is_empty() {
                return Ok(Vec::new());
            }
            voice.live_models(&key).await
        }
    });
    let models_now = match &*models.read() {
        Some(Ok(m)) => m.clone(),
        Some(Err(e)) => {
            if note.peek().is_empty() {
                note.set(e.clone());
            }
            Vec::new()
        }
        None => Vec::new(),
    };
    let saved_model = s.model.clone().unwrap_or_default();
    let unlisted = !saved_model.is_empty() && !models_now.contains(&saved_model);
    let env = s.env.clone().unwrap_or_else(|| "GEMINI_API_KEY".into());
    let place = if !has_key {
        format!("No key yet. Get one at AI Studio, or set {env} where tamale runs.")
    } else if s.from.as_deref() == Some("env") {
        format!("Using {env} from tamale's environment. A key pasted here is used instead.")
    } else {
        "Saved on this machine, in ~/.config/tamale/gemini.json (only you can read it).".into()
    };
    let active = voice.active();
    let body = move |extra: Option<serde_json::Value>| {
        let mut body =
            json!({ "model": model.peek().clone(), "voice": chosen_voice.peek().clone() });
        match extra {
            Some(extra) => {
                if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
                    b.extend(e.clone());
                }
            }
            None => {
                let k = key.peek().trim().to_string();
                if !k.is_empty() {
                    body["key"] = json!(k);
                }
            }
        }
        body
    };
    rsx! {
        div { class: "voice", id: "voice",
            div { class: "vcard settings",
                div { class: "vhead",
                    span { class: "vnames", span { class: "vtitle", "Gemini settings" } }
                    span { class: "spacer" }
                    button { class: "vbtn", title: "Close", "aria-label": "Close", onclick: move |_| voice.close(), {icon(I::Close)} }
                }
                if !note_top.is_empty() { div { class: "vnote warn", "{note_top}" } }
                label { class: "vlabel",
                    span { "API key" }
                    input {
                        class: "vfield",
                        r#type: "password",
                        autocomplete: "off",
                        spellcheck: "false",
                        placeholder: if has_key { "Paste a different key to replace it" } else { "Paste a key: AIza…" },
                        value: "{key}",
                        oninput: move |ev| key.set(ev.value()),
                        onkeydown: move |ev| ev.stop_propagation(),
                    }
                }
                div { class: "vnote hint", span { "{place} " } a { href: GEMINI_KEY_URL, target: "_blank", rel: "noopener noreferrer", "Get a key" } }
                label { class: "vlabel",
                    span { "Model" }
                    select { class: "vselect", value: "{model}", onchange: move |ev| model.set(ev.value()),
                        option { value: "", selected: model().is_empty(), "The newest Live model (automatic)" }
                        for m in models_now.iter() {
                            option { value: "{m}", selected: *m == model(), "{m}" }
                        }
                        if unlisted { option { value: "{saved_model}", selected: saved_model == model(), "{saved_model} (not listed)" } }
                    }
                }
                label { class: "vlabel",
                    span { "Voice" }
                    select { class: "vselect", value: "{chosen_voice}", onchange: move |ev| chosen_voice.set(ev.value()),
                        option { value: "", selected: chosen_voice().is_empty(), "Gemini's choice" }
                        for v in GEMINI_VOICES {
                            option { value: "{v}", selected: *v == chosen_voice(), "{v}" }
                        }
                    }
                }
                div { class: "vnote hint", "{note}" }
                div { class: "ap-actions vactions",
                    if s.from.as_deref() == Some("saved") {
                        button {
                            class: "btn danger",
                            onclick: move |_| {
                                let body = body(Some(json!({ "key": "" })));
                                note.set("Saving…".into());
                                spawn_forever(async move {
                                    match voice.save_settings(app, body).await {
                                        Ok(_) => note.set(String::new()),
                                        Err(e) => note.set(e),
                                    }
                                });
                            },
                            span { "Forget the saved key" }
                        }
                    }
                    span { class: "spacer" }
                    if active {
                        button { class: "btn", onclick: move |_| { voice.note.set(String::new()); voice.settings_view.set(false); }, span { "Back" } }
                    }
                    button {
                        class: "btn primary",
                        onclick: move |_| {
                            // Made in the click: only then may a page play sound in every browser.
                            let audio = host::Audio::new();
                            let body = body(None);
                            note.set("Saving…".into());
                            spawn_forever(async move {
                                match voice.save_settings(app, body).await {
                                    Ok(saved) if saved.key.as_ref().is_some_and(|k| !k.is_empty()) => {
                                        // The choices take effect in a new conversation.
                                        let topic = voice.topic.peek().clone();
                                        voice.start(app, topic, audio);
                                    }
                                    Ok(_) => {
                                        note.set("Paste a key first.".into());
                                        if let Some(a) = audio { a.close() }
                                    }
                                    Err(e) => {
                                        note.set(e);
                                        if let Some(a) = audio { a.close() }
                                    }
                                }
                            });
                        },
                        span { if active { "Save and restart" } else { "Save and start" } }
                    }
                }
            }
        }
    }
}
