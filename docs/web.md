# Browser and web UI

## Browser

`/browser` lets you use the session from a browser tab as well. Everything works there
as it does in the terminal, because the browser shows the same screen: tamale serves a
page running its browser client, written in Rust and compiled to WebAssembly, and sends
it the cells each frame changes. tamale prints the address and opens it in your default
browser.

The tab and the terminal take turns. Opening the tab moves the session to it, and the
terminal shows a notice; pressing a key in the terminal moves the session back, and a key
or click in the tab moves it there again. One tab is connected at a time: opening the
address in another tab disconnects the first. `/browser` again shows the address, and
`/browser off` stops serving (closing tamale does too).

- Cmd shortcuts stay with the browser (Cmd+V pastes into the prompt); other keys go to
  tamale. Browsers keep a few Ctrl shortcuts, like Ctrl+W, for themselves. The mouse
  wheel scrolls and clicks work as in the terminal; dragging selects text and copies it.
- `/login` still signs in through the terminal tamale runs in.
- `/copy` and `/paste` use the clipboard of the machine tamale runs on.

The server only listens on 127.0.0.1, and each address has a random token. Requests
without it are refused, as are pages from other sites (checked with the `Origin` and
`Host` headers), so only you can drive the session. Treat the address like a password:
anyone with it and access to your machine can run the agent.

**How it works.** The client (`browser/client`) draws with [Ratzilla](https://github.com/ratatui/ratzilla)'s
WebGL2 backend. tamale draws the browser's screen with an ordinary ratatui `Terminal`
whose backend records the changed cells instead of writing escape codes, and sends them
over a WebSocket; the client keeps a copy of the screen and redraws it, and sends back its
size and input (keys, text from input methods, pastes, wheel and clicks). Both directions
use the types in `browser/protocol`, encoded with postcard.

**Changing the client.** `cargo build` builds it (see *Building the browser apps* in
[Development](development.md#building-the-browser-apps)), so change `browser/client` or
`browser/protocol` and rebuild tamale. `cargo test --workspace` also runs the protocol's tests.

## Web UI

To reach a session from a phone or another computer, see [Remote access](remote-access.md).

`/web` moves the session into a browser tab, as a web page rather than a copy of the
terminal. From then on the terminal shows only the address, whether the agent is working,
how many tabs are open and the cost. There, `o` opens the page again, `c` copies its
address, `Esc` (or `/web off` in the page) ends the web session and brings the conversation
back to the terminal, and `Ctrl+C` quits. Any number of tabs can be open.

- **Chat.** Replies are rendered as markdown, with highlighted code blocks and copy
  buttons. Tool calls appear as compact steps that open to show their output; long runs
  fold. Type `/` for a menu of commands. `Enter` sends (or queues while the agent works),
  `Shift+Enter` adds a new line, `↑`/`↓` recall earlier prompts and `Esc` interrupts.
  Approvals appear above the prompt: answer with the buttons or `y`, `a` and `n`.
  The agent's questions do too, as a card: click options (or press `1`–`9`), type your own
  answer if none fit, and press **Next** / **Send answers** or `⏎`; `←`/`→` switch questions.
  The top bar shows the permission mode (click to cycle), the model, context use and
  cost.
- **Code.** The switch in the top bar puts the chat, the code or both on screen; the
  telescope on it is the code, with a count of the files that have uncommitted changes.
  `⌘B` or `/review` (or `/code`, `/diff`) shows and hides the code, and the page comes back
  to the two side by side after a reload. Drag the line between them to resize them
  (double-click resets it). A window too narrow for both shows one at a time.
  The code is [pullspace](https://github.com/bigmah/pullspace), an IDE-style code browser
  and reviewer, on your uncommitted changes: the working tree (staged, unstaged and
  untracked files) against `HEAD`. It opens on **Changes**, the files that differ, with the
  first of them as a diff; **Files** is the whole repository. It has search across the
  repository, go to definition and find references, and reads the files again after each
  tool result. It runs in the page as WebAssembly and never talks to GitHub; tamale hands
  it the files over the page's socket.
  - *From the chat to the code.* Steps that read or changed a file have an **open** or
    **diff** link, and file paths in replies (`src/web.rs`, `src/web.rs:120`) are links:
    each opens that file there, at that line. `/review <path>` does too.
  - *From the code to the chat.* **Ask** (`⌘⏎`) quotes what you're looking at into the
    prompt: the code you selected, or the line whose number you clicked, or just the file.
  - *On a phone.* The files are behind the button at the left of pullspace's bar, diffs
    are one column, and the search box takes the bar while you type in it.
  - While you're on the code alone and the agent asks for an approval or an answer, the
    chat comes back.

  It works through a hub too: pullspace is part of the page's module, which comes through
  the tunnel (gzipped, cached by the browser), and the repository's files come over the
  session's socket, end-to-end encrypted like everything else on it.
- **Artifacts.** During a web session replies are brief: a few sentences, or a list of at
  most 10 items. A longer answer (more than 10 items, or tables, diagrams or long code) comes
  as a sentence or three of summary followed by an HTML artifact that carries all of it: a
  designed page, interactive when that helps (diagrams, step-throughs, tabs, diffs, charts).
  The model writes it as a fenced
  ```` ```html artifact ```` block. While the block streams in, the page shows a
  placeholder, and when it is done the page loads it in a frame sized to fit. The bar
  above it can **revise** it (puts `Revise the "<title>" artifact: ` in the prompt; the
  model answers with the whole updated document), download it, reload it, expand it
  (`Esc` closes) or open it in a new tab. Back in the terminal, each artifact is
  collapsed to one line with its title. The sparkle button beside Send asks for one in a
  tap ("Can you generate an artifact for this?"), as in the iOS app; it hides while you
  type.
- **Artifact guide.** How artifacts should look and behave lives in a design guide
  (`src/web/artifact.md`: output contract, which layout fits which kind of answer, the
  colour and type tokens, component recipes and a quality checklist). While `/web` is on
  it is added to the system prompt, and each prompt only carries a short reminder. To
  use your own, put it in `~/.config/tamale/artifacts.md`; it replaces the built-in one.
- **Saving and resuming.** Every finished artifact is also saved as a standalone HTML
  file in `~/.local/share/tamale/sessions/<session-id>/artifacts/`, named after its title
  plus a short content hash, so revisions sit side by side. Artifacts are part of the
  conversation, so `tamale --continue` (or `--resume`) followed by `/web` shows them all
  again, ready to revise.
- **Artifacts gallery.** `/artifacts` in the page (or its button in the top bar) opens
  every saved artifact of this project: this session's first, then
  earlier sessions' by their last activity. Pick one on the left (or filter and use
  `↑`/`↓`; `Enter` opens it in a tab; drag the line between the two to resize the list)
  to see it on the right, where it can be shown in
  the chat, revised, downloaded or opened in a tab. Revising one from an earlier session
  points the model at its saved file. In the terminal, `/artifacts` lists this session's
  files.
- **Model picker.** `/model` in the page (or the model in the top bar) lists the models
  the session's backend offers, the same list as the terminal's picker (a provider's
  `/models` endpoint, so all of OpenRouter's for OpenRouter), with the one in use ticked.
  Type to filter by id or name, `↑`/`↓` and `Enter` (or tap) to switch; an id that isn't
  listed can still be typed and used. `/model <id>` switches directly.
- **Talking to the session with Gemini.** As in the [iOS app](ios.md), Gemini can drive
  the session for you by voice. The waveform button in the top bar (or `/voice`) opens a
  card above the prompt and starts a conversation through Gemini's Live API: ask "is there
  a compilation error?" and it sends tamale a prompt, says so, and tells you how it went
  when the turn ends. It can read the conversation, check the status, look at the
  uncommitted changes, interrupt, and answer tamale's questions. When tamale asks
  permission, Gemini reads the request out and answers only after you say yes or no; it
  never picks "always". You can also type to it, mute the microphone, or fold the card
  (tap its header) to give the chat the room. The approval and question cards stay on
  screen, so a tap still answers when that's quicker. The waveform on an artifact's bar
  starts the same conversation about that artifact: Gemini gets the page's text (and its
  HTML when that isn't long), and can still have tamale revise it.
  - *The key.* The page uses `GEMINI_API_KEY` from tamale's environment, or a key pasted
    into the card's settings (the gear), which tamale keeps in
    `~/.config/tamale/gemini.json`, readable only by you, and which wins over the
    environment's. Get one at [AI Studio](https://aistudio.google.com/apikey). The settings
    also pick a model (by default the newest Gemini Live model your key can use) and a
    voice. The key, your voice and what Gemini reads of the session go only to Google,
    straight from the browser.
  - *The microphone.* Browsers only give it to secure pages: through a hub (https) or at
    this machine's own address (`127.0.0.1`), not at a network address (`/web lan`), where
    you type to Gemini instead.
- **Animated videos with Manim.** `/video <what>` ("/video how a hash map handles
  collisions"), or just asking for an animation in your own words, has tamale write a
  [Manim](https://www.manim.community/) scene, renders it in a scratch directory from
  `mktemp -d` (not in your project), fixes it if rendering fails, and ends its reply with the
  video, which plays in the chat (with a download button). The how-to is part of the artifact
  guide, so every backend knows it. The film button beside the artifact button (or `/video`
  alone) asks for a video of whatever you were just discussing; like that button, it hides
  while you type. Manim must be installed on tamale's machine
  (`uv tool install manim`, plus ffmpeg, and LaTeX for equations; [TinyTeX](https://yihui.org/tinytex/) needs no admin rights); if it isn't, tamale says so
  rather than install it. Any reply can show a video this way: a fenced block whose info
  string is `video`, holding the file's path. The page asks tamale for the file over its
  socket, so it plays through a hub too; it must be an `.mp4`, `.mov`, `.webm` or `.gif` of at
  most 32 MB in the project or the temporary directory.
  - *Narration.* With a Gemini key (the same one the page's voice uses: one saved in the
    page, else `GEMINI_API_KEY`), videos have sound. The guide gives the backend a small
    Python helper to paste into the scene: each step is wrapped in
    `with narrate(self, "…"):`, which has Gemini speak the line, adds it with Manim's
    `add_sound` and stretches the step to fit, so the speech lines up with what's on
    screen. The key stays out of the prompt: tamale hands it (and the chosen voice) to the
    agent's commands as `TAMALE_GEMINI_KEY` and `TAMALE_GEMINI_VOICE`. Ask for a silent
    video to skip it; if the speech fails, the video renders silently at the same pace.
- Commands that open a picker in the terminal (`/effort`, `/resume`, `/backend` with no
  arguments, `/rewind`) explain their argument forms instead. `/login` still runs in the
  terminal.

The address works the same way as `/browser`'s: the server listens only on 127.0.0.1
with a random token, and checks the `Host` and `Origin` headers. The page also sends a
strict Content Security Policy, and model output is never treated as HTML in the page
itself; the only site besides tamale it may reach is Gemini's API
(`generativelanguage.googleapis.com`), for the voice conversation. Artifacts are the exception: each one is served from its own unguessable
`/artifact/<id>` URL with a `sandbox` CSP that leaves out `allow-same-origin`. Its scripts
run in an opaque origin that can't read the page, the token or the socket, even when the
artifact is opened in a tab of its own. Scripts and styles may load from https CDNs.

**How it works.** `src/web.rs` sends JSON over a WebSocket. Assistant markdown is turned
into HTML in tamale (`src/web/render.rs`, which uses pulldown-cmark), and code is
highlighted with syntect into token classes that the stylesheet colours. Each sync sends
only the entries that changed since the last one, at most every 40 ms. Reads of the
workspace's files and changes are answered by the server task itself (`src/web/pullspace.rs`
and `src/web/diff.rs`).

**The page** is a [Dioxus](https://dioxuslabs.com) app written in Rust (`web/`, a crate of
its own outside tamale's workspace) and compiled to WebAssembly: `src/web/index.html` loads
`/app.js`, which loads the module, `/app_bg.wasm`; `src/web/app.css` styles it. It holds the
session as signals (`web/src/app.rs`) that the components (`web/src/ui/`) draw, keeps the
connection (`web/src/conn.rs`), seals it end to end on a session reached through a hub
(`web/src/e2e.rs`, the `snow` crate, checked byte for byte against tamale's vectors), and
reaches the browser through web-sys (`web/src/host/web.rs`) rather than script. The CSP adds
`wasm-unsafe-eval`, and for pullspace (below) `unsafe-eval` and inline styles; inline script
stays shut. Replies keep coming as HTML rendered by tamale; the page splits
them into their top-level blocks, so streaming redraws only the last one and artifacts above
it don't reload. `cargo build` builds it into tamale (the module gzipped, about 2.3 MB with
pullspace in it; see *Building the browser apps* in
[Development](development.md#building-the-browser-apps)), so after changing `web/`, rebuild
tamale. `cd web && cargo test` runs its own tests. A hub passes `/app_bg.wasm` through; a hub from before the page was WebAssembly
doesn't, and needs updating.

**The desktop app** (`web/src/shell.rs`, `--features desktop`) is a window onto your
sessions, a wrapper around tamale: `cd web && cargo run --release --no-default-features
--features desktop`, or `web/bundle-desktop.sh` for `TamaleWeb.app`. It opens on a list of
the sessions running in the background on this machine (`tamale ls --json`), with what each
is doing; clicking one asks it to serve its page (`tamale web <id>`, below) and opens that,
the same Dioxus app a browser runs, in the system's webview. **New session** starts one in a
folder you pick (`tamale web new` there). They're ordinary background sessions: closing the
window leaves them running, and `tamale attach` picks them up in a terminal. In a session,
🫔 in the top bar (or ⌘O) goes back to the list. **Another machine** opens an address
instead, such as your hub's, and so does an address given as an argument.

It runs the `tamale` on your search path, or in `~/.cargo/bin`, `~/.local/bin`,
`/opt/homebrew/bin` or `/usr/local/bin`, or `TAMALE_BIN`; opened from the Finder, it runs it
in your login shell's environment, so the session finds `claude` and your keys as it would in
a terminal. A session from a tamale older than this can't be opened this way; `/web` in it
still works. Links and tabs the page opens go to your browser, downloads to `~/Downloads`.
`TamaleWeb.app` may use the microphone to talk to tamale with Gemini; run bare, macOS doesn't
let it, and the voice card takes typing. `--check` prints what the window shows as JSON and
quits, for checking it from a script; `--check-open=<id>` opens that session from the list
first, then goes back to it.

`tamale web <id>` asks a background session, over its socket, to serve its page if it isn't
already and prints the address; `tamale web new` starts a session in the current directory
first. Unlike `/web`, the session's terminal goes on showing the conversation, and no browser
opens.

**The code view** is pullspace, a Dioxus app too, drawn in the page by the same module:
`web/` depends on the `pullspace` crate and renders `pullspace::Embedded` with a
`pullspace::Link` to a `pullspace::Host` (`web/src/review.rs`). pullspace asks it for
`snapshot` (each side's files as git blob hashes) and `blobs` (their bytes), which go over
the socket as `ps_snapshot` and `ps_blobs` (`src/web/pullspace.rs`, which runs git as `/diff`
does and hashes working-tree files itself); the page tells it `changed` (read the files
again) and `open` (show this file, at this line) through the link, and it tells the page
`quote` (Ask) and `missing` (an `open` that named no file). The page has no chrome around
it: pullspace's own bar is the pane's header, and it lays itself out for the width of its
box. The two stylesheets stop at that box, `.pullspace`: pullspace scopes its own to it, and
the server serves `app.css` with every rule kept out of it (`pullspace_embed::css`, from the
`pullspace-embed` crate in pullspace's repository, which also has the message types). Keys
pressed in the code are pullspace's, as they were in a page of its own, except `⌘B`. The
count on the switch is `diff_stat` (`src/web/diff.rs`). pullspace keeps the files it reads
in the browser's own filesystem, which a browser offers only to a secure context; a page
opened at this machine's network address (`/web lan`) is not one, and there it holds them in
memory instead. To change it, change pullspace and push it, point the `pullspace` dependency
in `web/Cargo.toml` and `pullspace-embed` in `Cargo.toml` at that commit, and rebuild
tamale. To try a pullspace you haven't pushed, point both at `path = "../pullspace"` (and
`"../pullspace/crates/embed"`) for the time being.
`cargo test --bin tamale fake_review_page -- --ignored --nocapture` serves a page with a
sample conversation and the code view on a made-up repository (or `TAMALE_FAKE_ROOT`)
to try it in a browser; `TAMALE_FAKE_LAN=1` serves it at this machine's network address
instead, which is how a phone sees it and where the browser withholds storage and the
clipboard; with `TAMALE_HUB_URL` and `TAMALE_HUB_KEY` set it also connects to
that hub and prints a link in. `TAMALE_FAKE_VIDEO=<path>` adds a reply that shows that video,
`TAMALE_FAKE_ARTIFACT=1` one with an artifact, and prompts the page sends are printed as
`SEND=…`.
