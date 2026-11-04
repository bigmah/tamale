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

**How it works.** The client (`web/client`) draws with [Ratzilla](https://github.com/ratatui/ratzilla)'s
WebGL2 backend. tamale draws the browser's screen with an ordinary ratatui `Terminal`
whose backend records the changed cells instead of writing escape codes, and sends them
over a WebSocket; the client keeps a copy of the screen and redraws it, and sends back its
size and input (keys, text from input methods, pastes, wheel and clicks). Both directions
use the types in `web/protocol`, encoded with postcard.

**Changing the client.** The built client is checked in under `src/browser/dist`, so
building tamale doesn't need the WebAssembly toolchain. After changing `web/client` or
`web/protocol`, rebuild it with `web/build.sh` (it needs the `wasm32-unknown-unknown`
target and the `wasm-bindgen` CLI version pinned in `web/client/Cargo.toml`; it says how to
install them), then rebuild tamale. `cargo test --workspace` also runs the protocol's tests.

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
- **Code.** `/code` (or `⌘B`) opens the code browser beside the chat, with a file tree,
  a filter box, a text search (`/search <text>`) and tabs. `⌘P` (or `/files`) goes to a
  file by name, and `/code <path>` opens one directly. File paths in replies and in
  read/edit steps link to it. Click line numbers to select lines (shift-click or drag
  for a range), then press **Ask** (`⌘⏎`) to quote them into the prompt. The open file
  and the tree reload as the agent changes files. The browser reads only files inside
  the workspace (the working directory and `/add-dir` directories), and skips
  ignored files and `.git`.
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
  collapsed to one line with its title.
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
- **Artifacts gallery.** `/artifacts` in the page (or the button left of the code
  browser's) opens every saved artifact of this project: this session's first, then
  earlier sessions' by their last activity. Pick one on the left (or filter and use
  `↑`/`↓`; `Enter` opens it in a tab) to see it on the right, where it can be shown in
  the chat, revised, downloaded or opened in a tab. Revising one from an earlier session
  points the model at its saved file. In the terminal, `/artifacts` lists this session's
  files.
- Commands that open a picker in the terminal (`/model`, `/effort`, `/resume`,
  `/backend` with no arguments, `/rewind`, `/diff`) explain their argument forms
  instead. `/login` still runs in the terminal.

The address works the same way as `/browser`'s: the server listens only on 127.0.0.1
with a random token, and checks the `Host` and `Origin` headers. The page also sends a
strict Content Security Policy, and model output is never treated as HTML in the page
itself. Artifacts are the exception: each one is served from its own unguessable
`/artifact/<id>` URL with a `sandbox` CSP that leaves out `allow-same-origin`. Its scripts
run in an opaque origin that can't read the page, the token or the socket, even when the
artifact is opened in a tab of its own. Scripts and styles may load from https CDNs.

**How it works.** `src/web.rs` sends JSON over a WebSocket. Assistant markdown is turned
into HTML in tamale (`src/web/render.rs`, which uses pulldown-cmark), and code is
highlighted with syntect into token classes that the stylesheet colours. Each sync sends
only the entries that changed since the last one, at most every 40 ms. File reads, the tree
and searches are answered by the server task itself (`src/web/files.rs`). The page is
plain HTML, CSS and JavaScript (`src/web/index.html`, `app.css`, `app.js`). It is
embedded in the binary, so there is no build step.
