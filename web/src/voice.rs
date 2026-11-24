//! Talking to the session through Gemini's Live API, as the iOS app does (apple/Tamale/Voice):
//! the page streams the microphone to Gemini as 16 kHz PCM, plays its 24 kHz answers, and
//! shows what both sides said. Gemini drives the session through functions that act on the
//! page's socket, the way the composer and the cards do: send a prompt, read the
//! conversation and the changes, interrupt, and answer approvals and questions. Turns take
//! minutes, so a prompt returns at once, and the page tells Gemini when the turn ends or
//! tamale asks something. On an artifact's bar, the same conversation starts with the
//! artifact's text in Gemini's instructions.
//!
//! The key and the chosen model and voice are kept by tamale (see its `src/web/gemini.rs`):
//! each session's page is an origin of its own, so the browser couldn't keep them. Without
//! a microphone (a page on a LAN address isn't a secure context) it still works typed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use dioxus::core::spawn_forever;
use dioxus::prelude::*;
use futures_channel::oneshot;
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::app::{App, fmt_elapsed};
use crate::host::{self, Audio, SocketEvent};
use crate::wire::{self, Approval, Entry, GeminiSettings, Questions};

const GEMINI_HOST: &str = "generativelanguage.googleapis.com";
pub const GEMINI_VOICES: &[&str] = &[
    "Puck", "Charon", "Kore", "Fenrir", "Aoede", "Leda", "Orus", "Zephyr",
];
pub const GEMINI_KEY_URL: &str = "https://aistudio.google.com/apikey";

/// Where a conversation is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Phase {
    #[default]
    Idle,
    Connecting,
    Listening,
    Speaking,
    Ended,
}

impl Phase {
    pub fn active(mut self) -> bool {
        matches!(self, Phase::Connecting | Phase::Listening | Phase::Speaking)
    }
}

/// Who said a line of the transcript: you, Gemini, or the page (something it did).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Who {
    You,
    Gemini,
    Action,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Line {
    pub who: Who,
    pub text: String,
    pub closed: bool,
}

/// What the card shows of the conversation.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Talk {
    pub phase: Phase,
    /// Why it ended, if not by the user.
    pub reason: String,
    pub lines: Vec<Line>,
    pub model: String,
    /// Set when Google says the connection will close soon.
    pub notice: String,
    /// Why there's no microphone, when there isn't one: it's typed, then.
    pub mic_off: String,
    pub muted: bool,
    pub ready: bool,
}

/// An artifact a conversation is about.
#[derive(Clone, PartialEq, Debug)]
pub struct Topic {
    pub title: String,
    pub url: String,
}

/// The conversation's own state, which nothing draws.
#[derive(Default)]
struct Session {
    generation: u64,
    socket: Option<Rc<host::Socket>>,
    audio: Option<Rc<Audio>>,
    /// Events waiting for Gemini to finish speaking.
    pending: Vec<String>,
    setup: Option<oneshot::Sender<Result<(), String>>>,
    /// What Gemini answered itself, so the page doesn't tell it the user did.
    answering_approval: bool,
    answering_question: bool,
}

/// The voice card: the conversation, what it's about, and which view shows.
#[derive(Clone, Copy)]
pub struct Voice {
    session: CopyValue<Rc<RefCell<Session>>>,
    pub talk: Signal<Talk>,
    /// The microphone's loudness, 0…1.
    pub level: Signal<f64>,
    pub open: Signal<bool>,
    pub topic: Signal<Option<Topic>>,
    pub settings: Signal<Option<GeminiSettings>>,
    /// The settings form rather than the conversation.
    pub settings_view: Signal<bool>,
    /// The transcript folded away, to give the chat the room.
    pub folded: Signal<bool>,
    pub note: Signal<String>,
    /// The models each key can hold a live conversation with.
    models: CopyValue<HashMap<String, Vec<String>>>,
}

impl Voice {
    pub fn new() -> Voice {
        Voice {
            session: CopyValue::new(Rc::new(RefCell::new(Session::default()))),
            talk: Signal::new(Talk::default()),
            level: Signal::new(0.0),
            open: Signal::new(false),
            topic: Signal::new(None),
            settings: Signal::new(None),
            settings_view: Signal::new(false),
            folded: Signal::new(false),
            note: Signal::new(String::new()),
            models: CopyValue::new(HashMap::new()),
        }
    }

    fn session(&self) -> Rc<RefCell<Session>> {
        self.session.read().clone()
    }

    pub fn active(&self) -> bool {
        self.talk.peek().phase.active()
    }

    /// Opens the card and starts a conversation, about `topic` if given. Called from a
    /// click: the audio has to start in it.
    pub fn start(mut self, app: App, topic: Option<Topic>, audio: Option<Rc<Audio>>) {
        self.stop();
        self.topic.set(topic.clone());
        self.settings_view.set(false);
        self.folded.set(false);
        self.note.set(String::new());
        self.open.set(true);
        app.show_chat();
        let generation = {
            let session = self.session();
            let mut s = session.borrow_mut();
            s.generation += 1;
            s.pending.clear();
            s.audio = audio.clone();
            s.generation
        };
        self.talk.set(Talk {
            phase: Phase::Connecting,
            ..Talk::default()
        });
        let Some(audio) = audio else {
            return self.end(generation, host::NO_AUDIO);
        };
        spawn_forever(async move {
            let settings = match self.load_settings(app).await {
                Ok(s) => s,
                Err(e) => return self.end(generation, &e),
            };
            let Some(key) = settings.key.clone().filter(|k| !k.is_empty()) else {
                audio.close();
                self.session().borrow_mut().audio = None;
                self.talk.write().phase = Phase::Idle;
                self.settings_view.set(true);
                self.note
                    .set("Add a Gemini API key to talk to tamale.".into());
                return;
            };
            let page = match &topic {
                Some(topic) => Some(artifact_for_voice(app, topic).await),
                None => None,
            };
            if self.session().borrow().generation != generation {
                return audio.close();
            }
            let instruction = instruction(app, page.as_ref());
            if let Err(e) = self
                .run(
                    app,
                    generation,
                    audio,
                    key,
                    settings.model.clone(),
                    settings.voice.clone(),
                    instruction,
                )
                .await
            {
                self.end(generation, &e);
            }
        });
    }

    pub async fn load_settings(mut self, app: App) -> Result<GeminiSettings, String> {
        let m = app.request_reply("gemini", json!({}), 20_000).await?;
        let s: GeminiSettings = serde_json::from_value(m).unwrap_or_default();
        if let Some(e) = s.error.clone() {
            return Err(e);
        }
        self.settings.set(Some(s.clone()));
        Ok(s)
    }

    pub async fn save_settings(mut self, app: App, body: Value) -> Result<GeminiSettings, String> {
        let m = app.request_reply("gemini_save", body, 20_000).await?;
        let s: GeminiSettings = serde_json::from_value(m).unwrap_or_default();
        if let Some(e) = s.error.clone() {
            return Err(e);
        }
        self.settings.set(Some(s.clone()));
        Ok(s)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        mut self,
        app: App,
        generation: u64,
        audio: Rc<Audio>,
        key: String,
        model: Option<String>,
        voice: Option<String>,
        instruction: String,
    ) -> Result<(), String> {
        // The microphone.
        let runtime = dioxus::core::Runtime::current();
        let me = self;
        let rate = audio.sample_rate();
        let on_samples = Box::new(move |samples: Vec<f32>| {
            let sum: f32 = samples.iter().map(|s| s * s).sum();
            let muted = me.talk.peek().muted;
            let level = if muted || samples.is_empty() {
                0.0
            } else {
                (f64::from((sum / samples.len() as f32).sqrt()) * 8.0).min(1.0)
            };
            host::in_runtime(&runtime, || {
                let mut level_signal = me.level;
                level_signal.set(level);
            });
            let session = me.session();
            let s = session.borrow();
            if s.generation != generation || muted || !me.talk.peek().ready {
                return;
            }
            let pcm = downsample(&samples, rate, 16000.0);
            let bytes: Vec<u8> = pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
            let message = json!({ "realtimeInput": { "audio": { "mimeType": "audio/pcm;rate=16000", "data": crate::e2e::encode(&bytes) } } });
            if let Some(socket) = &s.socket {
                socket.send(&message.to_string());
            }
        });
        if let Err(why) = audio.start_mic(on_samples).await {
            self.talk.write().mic_off = why;
        }
        if self.session().borrow().generation != generation {
            return Ok(());
        }
        // Chosen automatically, a model Google turns away at setup gives way to the next.
        let models = match model.filter(|m| !m.is_empty()) {
            Some(m) => vec![m],
            None => self.live_models(&key).await?.into_iter().take(3).collect(),
        };
        if models.is_empty() {
            return Err("This key can't use any Gemini Live model.".into());
        }
        let count = models.len();
        for (i, name) in models.into_iter().enumerate() {
            if self.session().borrow().generation != generation
                || self.talk.peek().phase != Phase::Connecting
            {
                return Ok(());
            }
            self.talk.write().model = name.clone();
            match self
                .connect(app, generation, &key, &name, voice.as_deref(), &instruction)
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) if i + 1 == count => return Err(e),
                Err(_) => {}
            }
        }
        Ok(())
    }

    /// The models `key` can hold a live conversation with, the best guess for voice first.
    pub async fn live_models(mut self, key: &str) -> Result<Vec<String>, String> {
        if let Some(got) = self.models.read().get(key) {
            return Ok(got.clone());
        }
        let mut names = Vec::new();
        let mut token = String::new();
        loop {
            let mut url = format!("https://{GEMINI_HOST}/v1beta/models?pageSize=1000");
            if !token.is_empty() {
                url.push_str(&format!("&pageToken={token}"));
            }
            let (status, body) = host::fetch_json(&url, &[("x-goog-api-key", key)]).await?;
            if !(200..300).contains(&status) {
                let why = body["error"]["message"].as_str().unwrap_or("").to_string();
                return Err(if status == 400 || status == 403 {
                    format!("Gemini refused the key: {why}")
                } else {
                    format!("Gemini answered {status}: {why}")
                });
            }
            for m in body["models"].as_array().into_iter().flatten() {
                let methods = m["supportedGenerationMethods"].as_array();
                if !methods.is_some_and(|ms| ms.iter().any(|x| x == "bidiGenerateContent")) {
                    continue;
                }
                let name = m["name"]
                    .as_str()
                    .unwrap_or("")
                    .trim_start_matches("models/")
                    .to_string();
                // Gemini's own conversational models: the list also has world models and ones
                // for transcribing, translating or robots, which can't hold a conversation.
                if name.starts_with("gemini")
                    && !["transcribe", "translate", "robotics"]
                        .iter()
                        .any(|w| name.contains(w))
                {
                    names.push(name);
                }
            }
            token = body["nextPageToken"].as_str().unwrap_or("").to_string();
            if token.is_empty() {
                break;
            }
        }
        let version = |name: &str| -> f64 {
            let start = name
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or(name.len());
            let digits: String = name[start..]
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            digits.trim_end_matches('.').parse().unwrap_or(0.0)
        };
        // Within a version, the plain conversational model first: thinking ones answer slower.
        let penalty = |name: &str| {
            (name.contains("thinking") as u8) * 2
                + (name.contains("preview") || name.contains("exp")) as u8
        };
        names.sort_by(|a, b| {
            version(b)
                .partial_cmp(&version(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(penalty(a).cmp(&penalty(b)))
                .then(a.cmp(b))
        });
        let mut models = self.models;
        models.write().insert(key.to_string(), names.clone());
        Ok(names)
    }

    /// Resolves once Gemini has taken the setup; fails if it closes before that.
    async fn connect(
        mut self,
        app: App,
        generation: u64,
        key: &str,
        model: &str,
        voice: Option<&str>,
        instruction: &str,
    ) -> Result<(), String> {
        let url = format!(
            "wss://{GEMINI_HOST}/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={}",
            urlencode(key)
        );
        let (socket, mut events) = host::Socket::connect(&url)?;
        let socket = Rc::new(socket);
        let (tx, rx) = oneshot::channel();
        {
            let session = self.session();
            let mut s = session.borrow_mut();
            s.socket = Some(socket.clone());
            s.setup = Some(tx);
        }
        let mut generation_config = json!({ "responseModalities": ["AUDIO"] });
        if let Some(voice) = voice.filter(|v| !v.is_empty()) {
            generation_config["speechConfig"] =
                json!({ "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice } } });
        }
        let setup = json!({ "setup": {
            "model": format!("models/{model}"),
            "generationConfig": generation_config,
            "systemInstruction": { "parts": [{ "text": instruction }] },
            "inputAudioTranscription": {},
            "outputAudioTranscription": {},
            // Past the Live API's usual 15 minutes of audio, the oldest turns are dropped.
            "contextWindowCompression": { "slidingWindow": {} },
            "tools": [{ "functionDeclarations": tools() }],
        }});
        let model = model.to_string();
        spawn_forever(async move {
            while let Some(event) = events.next().await {
                let current = {
                    let session = self.session();
                    let s = session.borrow();
                    s.generation == generation
                        && s.socket.as_ref().is_some_and(|x| Rc::ptr_eq(x, &socket))
                };
                if !current {
                    return;
                }
                match event {
                    SocketEvent::Open => {
                        socket.send(&setup.to_string());
                    }
                    SocketEvent::Text(text) => {
                        if let Ok(m) = serde_json::from_str::<Value>(&text) {
                            self.handle(app, generation, m);
                        }
                    }
                    SocketEvent::Closed { reason, .. } => {
                        let pending = self.session().borrow_mut().setup.take();
                        if let Some(setup) = pending {
                            self.session().borrow_mut().socket = None;
                            let _ = setup.send(Err(if reason.is_empty() {
                                "Couldn't reach Gemini. Check the key, the model and the network."
                                    .into()
                            } else {
                                format!("Gemini refused {model}: {reason}")
                            }));
                        } else if self.talk.peek().ready {
                            let why = if reason.is_empty() {
                                "Gemini ended the conversation.".to_string()
                            } else {
                                format!("Gemini ended the conversation: {reason}")
                            };
                            self.end(generation, &why);
                        }
                        return;
                    }
                }
            }
        });
        let _ = &mut self;
        rx.await.unwrap_or_else(|_| Err("Stopped.".into()))
    }

    fn handle(mut self, app: App, generation: u64, m: Value) {
        if m.get("setupComplete").is_some() {
            {
                let mut talk = self.talk.write();
                talk.ready = true;
                talk.phase = Phase::Listening;
            }
            if let Some(setup) = self.session().borrow_mut().setup.take() {
                let _ = setup.send(Ok(()));
            }
            self.flush();
        }
        if let Some(calls) = m["toolCall"]["functionCalls"].as_array().cloned() {
            spawn_forever(async move {
                let mut responses = Vec::new();
                for call in calls {
                    let name = call["name"].as_str().unwrap_or("").to_string();
                    let args = call.get("args").cloned().unwrap_or(json!({}));
                    let response = tool(app, &name, &args).await;
                    responses.push(json!({ "id": call["id"], "name": name, "response": response }));
                }
                if self.session().borrow().generation == generation {
                    self.send_json(&json!({ "toolResponse": { "functionResponses": responses } }));
                }
            });
        }
        if let Some(away) = m.get("goAway") {
            let left = away["timeLeft"]
                .as_str()
                .map(|t| format!(" ({t})"))
                .unwrap_or_default();
            self.talk.write().notice = format!("Gemini will end this conversation soon{left}.");
        }
        let Some(content) = m.get("serverContent") else {
            return;
        };
        if let Some(text) = content["inputTranscription"]["text"].as_str() {
            self.append(Who::You, text);
        }
        if let Some(text) = content["outputTranscription"]["text"].as_str() {
            self.close_line(Who::You);
            self.append(Who::Gemini, text);
        }
        if content["interrupted"].as_bool() == Some(true) {
            self.interrupt_playback();
            self.close_line(Who::Gemini);
        }
        for part in content["modelTurn"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let data = &part["inlineData"];
            let mime = data["mimeType"].as_str().unwrap_or("");
            if let (Some(b64), true) = (data["data"].as_str(), mime.starts_with("audio/")) {
                let rate = mime
                    .split("rate=")
                    .nth(1)
                    .and_then(|r| r.split(';').next())
                    .and_then(|r| r.parse().ok())
                    .unwrap_or(24000.0);
                self.play(generation, b64, rate);
            }
        }
        if content["turnComplete"].as_bool() == Some(true) {
            self.close_line(Who::You);
            self.close_line(Who::Gemini);
            let playing = self
                .session()
                .borrow()
                .audio
                .as_ref()
                .is_some_and(|a| a.playing());
            if !playing {
                self.flush();
            }
        }
    }

    fn play(mut self, generation: u64, b64: &str, rate: f32) {
        let Some(pcm) = crate::e2e::decode(b64) else {
            return;
        };
        let Some(audio) = self.session().borrow().audio.clone() else {
            return;
        };
        let runtime = dioxus::core::Runtime::current();
        let me = self;
        let ended = Rc::new(move || {
            host::in_runtime(&runtime, || {
                if me.session().borrow().generation != generation {
                    return;
                }
                let mut talk = me.talk;
                if talk.peek().phase == Phase::Speaking {
                    talk.write().phase = Phase::Listening;
                    me.flush();
                }
            });
        });
        if audio.play(&pcm, rate, ended) && self.talk.peek().phase == Phase::Listening {
            self.talk.write().phase = Phase::Speaking;
        }
    }

    /// Whatever Gemini was saying stops now: the user spoke over it.
    fn interrupt_playback(mut self) {
        if let Some(audio) = self.session().borrow().audio.clone() {
            audio.interrupt();
        }
        if self.talk.peek().phase == Phase::Speaking {
            self.talk.write().phase = Phase::Listening;
        }
    }

    fn send_json(&self, value: &Value) {
        if let Some(socket) = self.session().borrow().socket.clone() {
            socket.send(&value.to_string());
        }
    }

    pub fn set_muted(mut self, muted: bool) {
        self.talk.write().muted = muted;
        if muted {
            self.send_json(&json!({ "realtimeInput": { "audioStreamEnd": true } }));
        }
    }

    /// Typed instead of spoken.
    pub fn send_text(mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() || !self.talk.peek().ready {
            return false;
        }
        self.interrupt_playback();
        self.append(Who::You, text);
        self.close_line(Who::You);
        self.send_json(&json!({ "realtimeInput": { "text": text } }));
        true
    }

    /// Tells Gemini something happened that it should act on or tell the user about, once it
    /// has finished what it's saying. `shown` goes in the transcript.
    fn inform(mut self, text: String, shown: &str) {
        if self.session().borrow().socket.is_none() {
            return;
        }
        self.note_line(shown);
        self.session().borrow_mut().pending.push(text);
        self.flush();
    }

    fn flush(mut self) {
        if self.talk.peek().phase != Phase::Listening {
            return;
        }
        let text = {
            let session = self.session();
            let mut s = session.borrow_mut();
            if s.pending.is_empty() {
                return;
            }
            std::mem::take(&mut s.pending).join("\n\n")
        };
        self.send_json(&json!({ "realtimeInput": { "text": format!("[A message from the app, not the user]\n{text}") } }));
    }

    /// A line in the transcript for something the page did.
    pub fn note_line(mut self, text: &str) {
        self.talk.write().lines.push(Line {
            who: Who::Action,
            text: text.to_string(),
            closed: true,
        });
    }

    fn append(mut self, who: Who, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut talk = self.talk.write();
        match talk.lines.last_mut() {
            Some(last) if last.who == who && !last.closed => last.text.push_str(text),
            _ => talk.lines.push(Line {
                who,
                text: text.trim_start().to_string(),
                closed: false,
            }),
        }
    }

    fn close_line(mut self, who: Who) {
        let mut talk = self.talk.write();
        if let Some(line) = talk.lines.iter_mut().rev().find(|l| l.who == who) {
            line.closed = true;
        }
    }

    pub fn stop(mut self) {
        let generation = self.session().borrow().generation;
        if self.talk.peek().phase.active() {
            self.end(generation, "");
        }
    }

    fn end(mut self, generation: u64, reason: &str) {
        let (socket, audio, setup) = {
            let session = self.session();
            let mut s = session.borrow_mut();
            if s.generation != generation {
                return;
            }
            s.generation += 1;
            s.pending.clear();
            (s.socket.take(), s.audio.take(), s.setup.take())
        };
        if let Some(setup) = setup {
            let _ = setup.send(Err("Stopped.".into()));
        }
        if let Some(socket) = socket {
            socket.close();
        }
        if let Some(audio) = audio {
            audio.close();
        }
        self.level.set(0.0);
        let mut talk = self.talk.write();
        talk.ready = false;
        talk.reason = reason.to_string();
        talk.phase = Phase::Ended;
    }

    pub fn close(mut self) {
        self.stop();
        self.open.set(false);
        self.settings_view.set(false);
    }

    // ---- events the page tells Gemini about

    pub fn turn_ended(mut self, app: App) {
        if !self.active() {
            return;
        }
        // The reply's last entries can come just after the status that says it's done.
        spawn_forever(async move {
            host::sleep(600).await;
            if !self.active() || app.running() {
                return;
            }
            let entries = app.entries.peek().clone();
            let video = entries
                .iter()
                .rev()
                .find_map(|e| match &*e.0 {
                    Entry::Assistant { md, .. } => Some(md.contains("```video\n")),
                    _ => None,
                })
                .unwrap_or(false);
            let video = if video {
                "\nIts reply has a video, which is playing in the chat now."
            } else {
                ""
            };
            self.inform(
                format!(
                    "tamale finished its turn. Tell the user how it went.{video}\n{}",
                    last_turn_text(app)
                ),
                "tamale finished",
            );
        });
    }

    pub fn approval(mut self, _app: App, a: &Approval) {
        self.session().borrow_mut().answering_approval = false;
        if !self.active() {
            return;
        }
        self.inform(
            format!(
                "tamale is waiting for permission: {}. Ask the user whether to allow it.",
                describe_approval(a)
            ),
            &format!("tamale asks permission: {}", a.summary),
        );
    }

    pub fn questions(mut self, _app: App, q: &Questions) {
        self.session().borrow_mut().answering_question = false;
        if !self.active() {
            return;
        }
        self.inform(
            format!(
                "tamale is asking the user: {}. Read the questions and options to them.",
                describe_questions(q)
            ),
            "tamale has questions",
        );
    }

    /// A card was answered: by the user on screen, unless Gemini just did.
    pub fn answered(mut self, _app: App, kind: &str) {
        let by_gemini = {
            let session = self.session();
            let mut s = session.borrow_mut();
            let flag = if kind == "approval" {
                &mut s.answering_approval
            } else {
                &mut s.answering_question
            };
            std::mem::replace(flag, false)
        };
        if by_gemini || !self.active() {
            return;
        }
        if kind == "approval" {
            self.inform(
                "The permission request is settled: the user answered it on screen (or tamale stopped waiting). Don't ask about it any more.".into(),
                "Permission answered on screen",
            );
        } else {
            self.inform(
                "tamale's questions are settled: the user answered them on screen (or tamale stopped waiting). Don't ask about them any more.".into(),
                "Questions answered on screen",
            );
        }
    }
}

/// 16-bit PCM at `to` Hz from float samples at `from`, averaging each output sample's span.
fn downsample(input: &[f32], from: f64, to: f64) -> Vec<i16> {
    let ratio = from / to;
    let len = (input.len() as f64 / ratio).floor() as usize;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let start = (i as f64 * ratio).floor() as usize;
        let end = (start + 1).max(input.len().min(((i + 1) as f64 * ratio).floor() as usize));
        let sum: f32 = input[start..end.min(input.len())].iter().sum();
        let v = (sum / (end - start) as f32).clamp(-1.0, 1.0);
        out.push(if v < 0.0 {
            (v * 32768.0) as i16
        } else {
            (v * 32767.0) as i16
        });
    }
    out
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

/// The functions Gemini may call.
fn tools() -> Value {
    json!([
        {
            "name": "send_prompt",
            "description": "Sends a message to tamale, the coding agent, as if the user typed it. Returns at once; tamale works on it and the app reports when the turn ends. If tamale is busy, the message is queued.",
            "parameters": { "type": "OBJECT", "properties": { "text": { "type": "STRING", "description": "The prompt, written as an instruction or question to the coding agent." } }, "required": ["text"] }
        },
        { "name": "get_status", "description": "Whether tamale is working, for how long, its model, project and directory, queued prompts, and anything it's waiting on." },
        {
            "name": "read_conversation",
            "description": "The last entries of the session's conversation: the user's prompts, tamale's replies, tool calls with their output, and errors.",
            "parameters": { "type": "OBJECT", "properties": { "last": { "type": "INTEGER", "description": "How many entries, from the end. Default 8, at most 40." } } }
        },
        { "name": "get_changes", "description": "The uncommitted changes in tamale's working directory (git diff): files, lines added and removed, and the changed lines." },
        { "name": "interrupt", "description": "Stops what tamale is doing now." },
        {
            "name": "answer_permission",
            "description": "Answers a permission request tamale is waiting on, which the app will have told you about. Only with the user's explicit yes or no to that request. Not for asking tamale to do something: that's send_prompt.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "allow": { "type": "BOOLEAN", "description": "true if the user said yes." },
                    "reason": { "type": "STRING", "description": "When refusing, what the user said to do instead, if anything." }
                },
                "required": ["allow"]
            }
        },
        {
            "name": "answer_questions",
            "description": "Answers tamale's waiting questions, one answer per question in order.",
            "parameters": {
                "type": "OBJECT",
                "properties": {
                    "answers": {
                        "type": "ARRAY",
                        "items": {
                            "type": "OBJECT",
                            "properties": {
                                "chosen": { "type": "ARRAY", "items": { "type": "STRING" }, "description": "Labels of the options the user picked." },
                                "other": { "type": "STRING", "description": "Anything the user said instead of, or as well as, an option." }
                            }
                        }
                    }
                },
                "required": ["answers"]
            }
        }
    ])
}

/// Runs one of [`tools`] for Gemini.
async fn tool(app: App, name: &str, args: &Value) -> Value {
    let voice = app.voice;
    let busy = app.running();
    let not_connected = json!({ "error": "The session isn't connected right now." });
    match name {
        "send_prompt" => {
            let text = args["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                return json!({ "error": "Empty prompt." });
            }
            if !app.send(json!({ "t": "send", "text": text }), true) {
                return not_connected;
            }
            voice.note_line(&format!("Sent to tamale: {text}"));
            json!({
                "sent": true,
                "queued": busy,
                "next": if busy {
                    "tamale is busy; this runs after the current turn. Tell the user so in a few words."
                } else {
                    "tamale is working on it. Tell the user in a few words what you asked; you'll be told when it's done."
                },
            })
        }
        "get_status" => status(app),
        "read_conversation" => {
            let last = args["last"].as_f64().map_or(8, |n| n as usize).clamp(1, 40);
            voice.note_line("Read the conversation");
            json!({ "conversation": conversation_text(app, last) })
        }
        "get_changes" => {
            voice.note_line("Looked at the changes");
            match changes_text(app).await {
                Ok(v) => v,
                Err(e) => json!({ "error": e }),
            }
        }
        "interrupt" => {
            if !busy {
                return json!({ "error": "tamale isn't doing anything." });
            }
            if !app.send(json!({ "t": "interrupt" }), true) {
                return not_connected;
            }
            voice.note_line("Interrupted tamale");
            json!({ "interrupted": true, "next": "Confirm it to the user in a few words." })
        }
        "answer_permission" => {
            let Some(approval) = app.approval.peek().clone() else {
                return json!({ "error": "tamale isn't waiting for permission. To ask tamale to do something, use send_prompt." });
            };
            let allow = args["allow"] == json!(true) || args["allow"] == json!("true");
            let mut message = json!({ "t": "approve", "choice": if allow { "yes" } else { "no" } });
            let reason = args["reason"].as_str().unwrap_or("").trim().to_string();
            if !allow && !reason.is_empty() {
                message["reason"] = json!(reason);
            }
            // Never "always": a standing rule is the user's to tap, not a voice model's to infer.
            if !app.send(message, true) {
                return not_connected;
            }
            voice.session().borrow_mut().answering_approval = true;
            voice.note_line(&format!(
                "{}: {}",
                if allow { "Allowed" } else { "Refused" },
                approval.summary
            ));
            json!({ "answered": if allow { "yes" } else { "no" }, "next": "Confirm it to the user in a few words." })
        }
        "answer_questions" => {
            let Some(q) = app.question.peek().clone() else {
                return json!({ "error": "tamale isn't asking anything." });
            };
            let answers: Vec<wire::Answer> = args["answers"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| wire::Answer {
                    chosen: a["chosen"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| c.as_str().map_or_else(|| c.to_string(), str::to_string))
                        .collect(),
                    other: a["other"]
                        .as_str()
                        .map(|o| o.trim().to_string())
                        .filter(|o| !o.is_empty()),
                })
                .collect();
            if answers.len() != q.questions.len() {
                return json!({ "error": format!("tamale asked {} questions; give one answer for each.", q.questions.len()) });
            }
            if answers
                .iter()
                .any(|a| a.chosen.is_empty() && a.other.is_none())
            {
                return json!({ "error": "Every question needs an answer." });
            }
            if !app.send(json!({ "t": "answer", "answers": answers }), true) {
                return not_connected;
            }
            voice.session().borrow_mut().answering_question = true;
            voice.note_line("Answered tamale's questions");
            json!({ "answered": true })
        }
        _ => json!({ "error": format!("No function called {name}.") }),
    }
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{} …(cut)", s.chars().take(n).collect::<String>())
    } else {
        s.to_string()
    }
}

/// The text of a reply as Gemini reads it: artifacts and videos as one line each.
fn reply_text(md: &str, html: &str, artifacts: &[wire::Artifact]) -> String {
    let mut text = if md.is_empty() {
        crate::html::text(html)
    } else {
        md.to_string()
    };
    for (n, a) in artifacts.iter().enumerate() {
        let title = if a.title.is_empty() { "page" } else { &a.title };
        text = text.replace(
            &format!("<!-- artifact {n} -->"),
            &format!("[artifact: {title}]"),
        );
    }
    while let Some(at) = text.find("```video\n") {
        let rest = &text[at + 9..];
        let Some(end) = rest.find("```") else { break };
        let name = rest[..end]
            .trim()
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("")
            .to_string();
        text = format!("{}[video: {name}]{}", &text[..at], &rest[end + 3..]);
    }
    text
}

fn entry_text(e: &Entry, room: usize) -> String {
    match e {
        Entry::User { text } => format!("User: {}", cut(text, room)),
        Entry::Assistant {
            md,
            html,
            artifacts,
        } => format!("tamale: {}", cut(&reply_text(md, html, artifacts), room)),
        Entry::Note { md, html } => format!("Note: {}", cut(&reply_text(md, html, &[]), room)),
        Entry::Tool {
            name,
            summary,
            description,
            output,
            error,
            ..
        } => {
            let what = format!(
                "{} {}",
                if name.is_empty() { "tool" } else { name },
                if summary.is_empty() {
                    description
                } else {
                    summary
                }
            );
            match output {
                None => format!("Tool {what}: running"),
                Some(o) => format!(
                    "Tool {what}{}: {}",
                    if *error { " FAILED" } else { "" },
                    cut(o, room.min(if *error { 1500 } else { 500 }))
                ),
            }
        }
        Entry::Info { text } => format!("Info: {}", cut(text, room)),
        Entry::Error { text } => format!("Error: {}", cut(text, room)),
        Entry::Unknown => String::new(),
    }
}

fn conversation_text(app: App, last: usize) -> String {
    let entries = app.entries.peek();
    let start = entries.len().saturating_sub(last);
    let lines: Vec<String> = entries[start..]
        .iter()
        .map(|e| entry_text(&e.0, 1500))
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        "(nothing yet)".into()
    } else {
        lines.join("\n")
    }
}

fn connection_text(app: App) -> &'static str {
    if app.conn.read().locked {
        "unreachable"
    } else if app.is_ready() {
        "connected"
    } else {
        "reconnecting"
    }
}

fn status_line(app: App) -> String {
    let Some(s) = app.status.peek().clone() else {
        return format!("Session {}; no status yet.", connection_text(app));
    };
    let mut line = if s.running {
        "tamale is working".to_string()
    } else {
        "tamale is idle".to_string()
    };
    let now = (host::now_ms() / 1000.0) as u64;
    if let (true, Some(since)) = (s.running, s.since) {
        line.push_str(&format!(
            " (for {})",
            fmt_elapsed(now.saturating_sub(since))
        ));
        if !s.step.is_empty() {
            line.push_str(&format!(
                "; now: {}, for {}",
                s.step,
                fmt_elapsed(now.saturating_sub(s.step_since.unwrap_or(since)))
            ));
            if s.steps > 0 {
                line.push_str(&format!(", {} steps so far", s.steps));
            }
        }
    }
    format!(
        "{line}. Project {}, model {}. Session {}.",
        s.project,
        s.model,
        connection_text(app)
    )
}

fn describe_approval(a: &Approval) -> String {
    let mut s = format!("{}: {}", a.name, a.summary);
    if !a.description.is_empty() && a.description != a.summary {
        s.push('\n');
        s.push_str(&a.description.chars().take(1500).collect::<String>());
    }
    s
}

fn describe_questions(q: &Questions) -> String {
    q.questions
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let options: Vec<String> = item
                .options
                .iter()
                .map(|o| {
                    if o.description.is_empty() {
                        o.label.clone()
                    } else {
                        format!("{} ({})", o.label, o.description)
                    }
                })
                .collect();
            format!(
                "{}. {}{} Options: {}",
                i + 1,
                item.question,
                if item.multi_select { " (pick any)" } else { "" },
                options.join("; ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn status(app: App) -> Value {
    let mut out = json!({ "connection": connection_text(app), "summary": status_line(app) });
    if let Some(s) = app.status.peek().clone() {
        out["busy"] = json!(s.running);
        out["compacting"] = json!(s.compacting);
        out["project"] = json!(s.project);
        out["directory"] = json!(s.cwd);
        out["model"] = json!(s.model);
        out["mode"] = json!(s.mode);
        out["queued_prompts"] = json!(s.queued);
        if let Some(goal) = s.goal {
            out["goal"] = json!(goal);
        }
        if let Some(state) = s.goal_state {
            out["goal_state"] = json!(state);
        }
        if s.window > 0 {
            out["context_used_percent"] = json!((s.context * 100 + s.window / 2) / s.window);
        }
    }
    if let Some(a) = app.approval.peek().as_ref() {
        out["waiting_for_permission"] = json!(describe_approval(a));
    }
    if let Some(q) = app.question.peek().as_ref() {
        out["waiting_for_answers"] = json!(describe_questions(q));
    }
    let running = app.entries.peek().iter().rev().find_map(|e| match &*e.0 {
        Entry::Tool {
            name,
            summary,
            description,
            output: None,
            ..
        } => Some(format!(
            "{name}: {}",
            if summary.is_empty() {
                description
            } else {
                summary
            }
        )),
        _ => None,
    });
    if let Some(running) = running {
        out["running_now"] = json!(running);
    }
    out
}

/// What happened since the last prompt: replies, errors, and how the tools went.
fn last_turn_text(app: App) -> String {
    let entries = app.entries.peek();
    let start = entries
        .iter()
        .rposition(|e| matches!(&*e.0, Entry::User { .. }))
        .unwrap_or_else(|| entries.len().saturating_sub(6));
    let turn = &entries[start..];
    let tools: Vec<&Entry> = turn.iter().map(|e| &*e.0).filter(|e| e.is_tool()).collect();
    let failed = tools
        .iter()
        .filter(|e| matches!(e, Entry::Tool { error: true, .. }))
        .count();
    let mut out = format!(
        "It ran {} tool call{}{}.\n",
        tools.len(),
        if tools.len() == 1 { "" } else { "s" },
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        }
    );
    for e in turn {
        if !e.0.is_tool() || matches!(&*e.0, Entry::Tool { error: true, .. }) {
            out.push_str(&entry_text(&e.0, 2500));
            out.push('\n');
        }
    }
    let skip = out.chars().count().saturating_sub(6000);
    out.chars().skip(skip).collect()
}

async fn changes_text(app: App) -> Result<Value, String> {
    let diff = app.request_reply("diff", json!({}), 20_000).await?;
    if let Some(error) = diff["error"].as_str() {
        return Ok(json!({ "error": error }));
    }
    let files = diff["files"].as_array().cloned().unwrap_or_default();
    let n = |f: &Value, k: &str| f[k].as_u64().unwrap_or(0);
    let added: u64 = files.iter().map(|f| n(f, "added")).sum();
    let removed: u64 = files.iter().map(|f| n(f, "removed")).sum();
    let mut out = format!(
        "{}: {} file{}, +{added} −{removed}\n",
        diff["label"].as_str().unwrap_or("Changes"),
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    'files: for f in &files {
        out.push_str(&format!(
            "\n{} {} +{} −{}\n",
            f["status"].as_str().unwrap_or(""),
            f["path"].as_str().unwrap_or(""),
            n(f, "added"),
            n(f, "removed")
        ));
        if f["binary"].as_bool() == Some(true) || f["omitted"].as_bool() == Some(true) {
            continue;
        }
        for hunk in f["hunks"].as_array().into_iter().flatten() {
            let lines = hunk.as_array().or_else(|| hunk["lines"].as_array());
            for line in lines.into_iter().flatten() {
                let k = line["k"].as_str().unwrap_or("");
                if k == "+" || k == "-" {
                    out.push_str(k);
                    out.push_str(line["t"].as_str().unwrap_or(""));
                    out.push('\n');
                }
            }
            if out.len() > 12_000 {
                out.push_str("…(cut)\n");
                break 'files;
            }
        }
    }
    Ok(json!({ "changes": out.chars().take(12_000).collect::<String>() }))
}

/// An artifact's text and document, as Gemini reads them.
pub struct Page {
    pub title: String,
    pub text: String,
    pub html: String,
}

async fn artifact_for_voice(app: App, topic: &Topic) -> Page {
    let html = app.artifact_html(topic.url.clone()).await;
    // Without tamale's own scripts, which it adds to every artifact (see artifacts.rs).
    let document = match html.find("\n<script>") {
        Some(at) => html[..at].to_string(),
        None => html.clone(),
    };
    let mut body = document.clone();
    for tag in ["script", "style", "noscript", "template"] {
        while let Some(start) = body.to_ascii_lowercase().find(&format!("<{tag}")) {
            let close = format!("</{tag}>");
            let end = body.to_ascii_lowercase()[start..]
                .find(&close)
                .map_or(body.len(), |e| start + e + close.len());
            body.replace_range(start..end, "");
        }
    }
    let text = crate::html::text(&body);
    let mut tidy = String::new();
    for line in text
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
    {
        if !line.is_empty() {
            tidy.push_str(&line);
            tidy.push('\n');
        }
    }
    Page {
        title: topic.title.clone(),
        text: tidy.trim().to_string(),
        html: document,
    }
}

fn instruction(app: App, page: Option<&Page>) -> String {
    let place = app
        .status
        .peek()
        .as_ref()
        .map(|s| format!("{} ({})", s.project, s.cwd))
        .unwrap_or_else(|| "a project".into());
    let mut out = format!(
        "You're a voice assistant for the user's coding agent, tamale, which is running on their computer in {place}. The user has tamale's web page open and is talking to you; you control that tamale session through your functions, as if typing into it.

How to work:
- For anything about the code or the machine (does it build, run the tests, what's in a file, fix something), ask tamale with send_prompt. Write the prompt as a clear instruction, as the user would type it. Say briefly what you asked, then stop talking: tamale takes a while, and the app will tell you when it's done.
- To answer what tamale did or said, use read_conversation, get_status or get_changes first rather than guessing.
- When the app tells you a turn finished, tell the user the gist in a sentence or two. Mention errors. Don't read code, paths or long output aloud unless asked.
- When tamale asks permission, say what it wants to do in plain words and ask yes or no. Call answer_permission only after the user clearly answers that request. Never approve on your own.
- When tamale asks questions, read them and their options, and answer with answer_questions only with what the user chose.
- If the user wants tamale to stop, call interrupt.
Keep it spoken: short sentences, no markdown, no lists of symbols.

Right now: {}",
        status_line(app)
    );
    if let Some(a) = app.approval.peek().as_ref() {
        out.push_str(&format!(
            "\ntamale is waiting for permission: {}",
            describe_approval(a)
        ));
    }
    if let Some(q) = app.question.peek().as_ref() {
        out.push_str(&format!(
            "\ntamale is waiting for answers to: {}",
            describe_questions(q)
        ));
    }
    out.push_str(&format!(
        "\n\nThe end of the conversation so far:\n{}",
        conversation_text(app, 6)
    ));
    if let Some(page) = page {
        out.push_str(&format!(
            "\n\nThe user started this conversation from a page tamale made, titled \"{0}\", to talk about it. Answer questions about it from the page, the way you would in a spoken conversation; if the page doesn't say, tell them, then answer from what you know if you can. To change the page, ask tamale with send_prompt to revise the \"{0}\" artifact.\n\n=== Page text ===\n{1}",
            page.title,
            page.text.chars().take(150_000).collect::<String>()
        ));
        if page.html.len() <= 120_000 {
            out.push_str(&format!("\n\n=== Page HTML ===\n{}", page.html));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsamples_by_averaging() {
        let out = downsample(&[0.5; 48], 48000.0, 16000.0);
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|v| (*v - 16383).abs() <= 1));
    }

    #[test]
    fn replies_read_with_their_artifacts_named() {
        let a = vec![wire::Artifact {
            title: "Plan".into(),
            ..Default::default()
        }];
        assert_eq!(
            reply_text("See <!-- artifact 0 -->.", "", &a),
            "See [artifact: Plan]."
        );
        assert_eq!(
            reply_text("```video\n/tmp/a/b.mp4\n```", "", &[]),
            "[video: b.mp4]"
        );
    }
}
