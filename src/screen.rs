//! Where the TUI is shown: the terminal tamale runs in, or with `--server`, whichever
//! terminal is attached to this process (see `background`).

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream,
};
use crossterm::execute;
use futures::StreamExt;

use crate::app::App;
use crate::backend::Backend;
#[cfg(unix)]
use crate::background::{Host, HostEvent, LoginCommand};

/// What happened on a screen.
pub enum ScreenEvent {
    Input(Event),
    /// A terminal attached, which should take over from a browser tab.
    Attached,
    /// Nothing to act on, except for redrawing.
    Changed,
    Quit,
}

pub enum Screen {
    Local {
        terminal: ratatui::DefaultTerminal,
        /// `None` during a sign-in, so it can't steal the sign-in's keystrokes.
        events: Option<EventStream>,
    },
    #[cfg(unix)]
    Hosted(Host),
}

/// Restores terminal input modes even if we panic.
pub fn install_panic_hook() {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(
            std::io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture
        );
        hook(info);
    }));
}

pub fn suspend_terminal() {
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture
    );
    ratatui::restore();
}

pub fn resume_terminal(mouse: bool) -> Result<ratatui::DefaultTerminal> {
    let terminal = ratatui::init();
    execute!(std::io::stdout(), EnableBracketedPaste)?;
    if mouse {
        execute!(std::io::stdout(), EnableMouseCapture)?;
    }
    Ok(terminal)
}

impl Screen {
    /// Takes over the terminal tamale runs in.
    pub fn local(mouse: bool) -> Result<Self> {
        install_panic_hook();
        Ok(Self::Local {
            terminal: resume_terminal(mouse)?,
            events: Some(EventStream::new()),
        })
    }

    /// The next thing to happen. Cancel safe.
    pub async fn next(&mut self) -> Result<ScreenEvent> {
        match self {
            Self::Local { events, .. } => match events.as_mut() {
                Some(events) => match events.next().await {
                    Some(event) => Ok(ScreenEvent::Input(event?)),
                    None => std::future::pending().await,
                },
                None => std::future::pending().await,
            },
            #[cfg(unix)]
            Self::Hosted(host) => Ok(match host.next().await {
                HostEvent::Input(event) => ScreenEvent::Input(event),
                HostEvent::Attached => ScreenEvent::Attached,
                HostEvent::Changed => ScreenEvent::Changed,
                HostEvent::Quit => ScreenEvent::Quit,
            }),
        }
    }

    pub fn draw(&mut self, render: impl FnOnce(&mut ratatui::Frame)) -> Result<()> {
        match self {
            Self::Local { terminal, .. } => {
                terminal.draw(render)?;
                Ok(())
            }
            #[cfg(unix)]
            Self::Hosted(host) => host.draw(render),
        }
    }

    /// Runs a sign-in in the terminal. The outer error means the terminal couldn't be
    /// taken back; the inner one is how the sign-in went.
    pub async fn login(&mut self, login: &mut Backend, mouse: bool) -> Result<Result<()>> {
        match self {
            Self::Local { terminal, events } => {
                *events = None;
                suspend_terminal();
                let result = login.login().await;
                *terminal = resume_terminal(mouse)?;
                *events = Some(EventStream::new());
                Ok(result)
            }
            #[cfg(unix)]
            Self::Hosted(host) => Ok(host_login(host, login).await),
        }
    }

    /// Keeps a server's client and listing up to date with the app. Called every time
    /// round the event loop.
    pub fn sync(&mut self, app: &mut App) {
        #[cfg(unix)]
        if let Self::Hosted(host) = self {
            if app.take_detach_request() {
                host.detach();
            }
            if let Some(to) = app.take_switch_request() {
                if !host.switch(to) {
                    app.on_web(Err(format!(
                        "Attach in a terminal to go to that session: tamale attach {to}"
                    )));
                } else if app.is_blank_session() {
                    // Nothing's happened here, so nothing's left behind to come back to.
                    app.should_quit = true;
                }
            }
            host.set_mouse(app.mouse_enabled());
            let (cwd, session_id, title, state) = app.background_status();
            host.publish(&cwd, &session_id, title, state);
        }
        #[cfg(not(unix))]
        let _ = app;
    }

    /// Whether a server has sat unused for so long that it should exit.
    pub fn expired(&mut self, in_use: bool) -> bool {
        match self {
            Self::Local { .. } => {
                let _ = in_use;
                false
            }
            #[cfg(unix)]
            Self::Hosted(host) => host.expired(in_use),
        }
    }

    /// Gives the terminal back, or tells the attached client that the session is over.
    pub async fn close(self, error: Option<String>) {
        match self {
            Self::Local { .. } => {
                let _ = error;
                suspend_terminal();
            }
            #[cfg(unix)]
            Self::Hosted(host) => host.finish(error).await,
        }
    }
}

#[cfg(unix)]
async fn host_login(host: &mut Host, login: &mut Backend) -> Result<()> {
    let command = login.login_command()?;
    let command = command.as_std();
    let text = |s: &std::ffi::OsStr| s.to_string_lossy().into_owned();
    let command = LoginCommand {
        program: text(command.get_program()),
        args: command.get_args().map(text).collect(),
        envs: command
            .get_envs()
            .map(|(name, value)| (text(name), value.map(text)))
            .collect(),
    };
    let (success, status) = host.login(command).await?;
    login.finish_login(success, &status).await
}
