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

  It works through a hub too: pullspace's files come through the tunnel like the page's
  (the module gzipped, about 2 MB, cached by the browser), and the repository's files over
  the session's socket, end-to-end encrypted like everything else on it.
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
- Commands that open a picker in the terminal (`/model`, `/effort`, `/resume`,
  `/backend` with no arguments, `/rewind`) explain their argument forms
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

**The code view** is pullspace's own WebAssembly build, from the `pullspace-embed` crate
in [pullspace's repository](https://github.com/bigmah/pullspace) (`crates/embed`). The crate
carries the built bundle, checked in there, and the types of the messages between the frame
and its page, so building tamale needs no wasm toolchain. tamale serves the bundle at
`/pullspace/` (to this server's page only, with the crate's CSP, which keeps it from reaching
anything but this server; a hub tunnels these paths too, with the module's bytes sent as
base64 in the `got` frame). It runs in a same-origin frame as `/pullspace/?embed` and asks the
page for `snapshot` (each side's files as git blob hashes) and `blobs` (their bytes) over
`postMessage`; the page forwards them as `ps_snapshot` and `ps_blobs` (`src/web/pullspace.rs`),
which run git as `/diff` does and hash working-tree files themselves. The frame says `ready`
once it is listening; from then the page posts it `changed` (read the files again) and
`open` (show this file, at this line), and it posts back `quote` (Ask) and `missing` (an
`open` that named no file). The page has no chrome around the frame: pullspace's own bar is
the pane's header, and it lays itself out for whatever width it is given. The count on the
switch is `diff_stat` (`src/web/files.rs`). pullspace keeps the files it reads in the browser's
own filesystem, which a browser offers only to a secure context; a page opened at this
machine's network address (`/web lan`) is not one, and there it holds them in memory instead.
To change it, change pullspace, run its `crates/embed/build.sh` (it needs the `dx`
CLI) and commit the bundle there, then point tamale's `pullspace-embed` dependency at that
commit. `cargo test --bin tamale fake_review_page -- --ignored --nocapture` serves a page
with a sample conversation and the code view on a made-up repository (or `TAMALE_FAKE_ROOT`)
to try it in a browser; `TAMALE_FAKE_LAN=1` serves it at this machine's network address
instead, which is how a phone sees it and where the browser withholds storage and the
clipboard; with `TAMALE_HUB_URL` and `TAMALE_HUB_KEY` set it also connects to
that hub and prints a link in. To try a pullspace you haven't pushed, add
`--config 'patch."https://github.com/bigmah/pullspace".pullspace-embed.path="../pullspace/crates/embed"'`
to the `cargo` command (it rewrites the crate's entry in `Cargo.lock`; `git checkout Cargo.lock`
afterwards).
