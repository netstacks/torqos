//! `torq-applet`: TorqOS in COSMIC's panel (ADR-049). The machine's state at a glance and the one action
//! that settles it: a commit waiting for confirmation, a restart or log-out a switch needs, an alarm,
//! proposals waiting. TorqOS Settings holds the detail; this only points there. It changes nothing itself:
//! it confirms through torqcli in a terminal (a password, when one is asked, is typed there), and asks
//! COSMIC's own dialogs to log out or restart.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use cosmic::iced::window::Id;
use cosmic::iced::{Length, Rectangle, Subscription};
use cosmic::prelude::*;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget;
use std::path::PathBuf;
use std::time::Duration;
use torq::app::{Glance, Needs};

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

/// How often the broker is asked, when nothing else says to ask sooner.
const EVERY: Duration = Duration::from_secs(30);
/// How often the running system is looked at: a switch is seen within this, and the broker asked then.
const SWITCH: Duration = Duration::from_secs(3);

#[derive(Default)]
struct Applet {
    core: cosmic::Core,
    popup: Option<Id>,
    glance: Option<Result<Glance, String>>,
    /// The system the session started on (ADR-049 1): what a log-out would take into use is the difference.
    session: Option<PathBuf>,
    /// The running system when last looked at: a change is a switch.
    running: Option<PathBuf>,
}

#[derive(Debug, Clone)]
enum Message {
    Ask,
    Looked,
    Answer(Result<Glance, String>),
    Confirm,
    LogOut,
    Restart,
    Open,
    PopupClosed(Id),
    Surface(cosmic::surface::Action<Message>),
}

/// What the panel shows: the first that holds (ADR-049 2), its icon and its one line.
enum State<'a> {
    Confirm(u64, &'a str),
    Needs(&'a Needs),
    Alarm(&'a str),
    Waiting(usize, usize),
    Well,
    Unknown,
}

impl Applet {
    fn state(&self) -> State<'_> {
        let Some(Ok(g)) = &self.glance else { return State::Unknown };
        if let Some((seq, deadline)) = &g.confirm {
            return State::Confirm(*seq, deadline);
        }
        if g.needs != Needs::Nothing {
            return State::Needs(&g.needs);
        }
        if let Some(a) = &g.alarm {
            return State::Alarm(a);
        }
        if g.waiting.0 > 0 {
            return State::Waiting(g.waiting.0, g.waiting.1);
        }
        State::Well
    }

    fn icon_name(&self) -> &'static str {
        match self.state() {
            State::Confirm(..) => "alarm-symbolic",
            State::Needs(Needs::Restart(_)) => "system-reboot-symbolic",
            State::Needs(_) => "system-log-out-symbolic",
            State::Alarm(_) => "dialog-warning-symbolic",
            State::Waiting(..) => "mail-unread-symbolic",
            State::Well => "emblem-ok-symbolic",
            State::Unknown => "content-loading-symbolic",
        }
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut col = widget::column::with_capacity(6).spacing(spacing.space_s).padding(spacing.space_s);
        let version = match &self.glance {
            Some(Ok(g)) => format!("TorqOS {}", g.version),
            _ => "TorqOS".into(),
        };
        col = col.push(widget::text::heading(version));
        let line = |s: String| widget::text::body(s).width(Length::Fill);
        match self.state() {
            State::Confirm(seq, deadline) => {
                col = col.push(line(format!("Change #{seq} is waiting for you to confirm it. It rolls back at {deadline} unless you do.")));
                let both = matches!(&self.glance, Some(Ok(g)) if matches!(g.needs, Needs::Restart(_)));
                col = col.push(widget::button::suggested(if both { "Confirm, then restart" } else { "Confirm" }).on_press(Message::Confirm));
            }
            State::Needs(n) => {
                col = col.push(line(n.line().unwrap_or_default()));
                col = col.push(match n {
                    Needs::Restart(_) => widget::button::suggested("Restart…").on_press(Message::Restart),
                    _ => widget::button::suggested("Log out…").on_press(Message::LogOut),
                });
            }
            State::Alarm(a) => col = col.push(line(a.to_string())),
            State::Waiting(n, tainted) => {
                let what = if n == 1 { "A proposal is".to_string() } else { format!("{n} proposals are") };
                let mut s = format!("{what} waiting for you to approve or refuse.");
                if tainted > 0 {
                    s.push_str(&format!(" {tainted} came from a session that read untrusted content."));
                }
                col = col.push(line(s));
            }
            State::Well => {
                col = col.push(line("This machine is exactly as its history says.".into()));
                if let Some(Ok(Glance { last: Some(last), .. })) = &self.glance {
                    col = col.push(widget::text::caption(format!("Last change: {last}")));
                }
            }
            State::Unknown => {
                let s = match &self.glance {
                    Some(Err(e)) => format!("The broker did not answer: {e}"),
                    _ => "Asking the broker…".into(),
                };
                col = col.push(line(s));
            }
        }
        col = col.push(widget::divider::horizontal::default());
        col = col.push(widget::button::text("Open TorqOS Settings").on_press(Message::Open));
        self.core.applet.popup_container(col).into()
    }
}

impl cosmic::Application for Applet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.Applet";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: ()) -> (Self, Task<cosmic::Action<Message>>) {
        let mut app = Applet { core, session: Some(session_system()), running: running(), ..Default::default() };
        let ask = app.update(Message::Ask);
        (app, ask)
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            cosmic::iced::time::every(EVERY).map(|_| Message::Ask),
            cosmic::iced::time::every(SWITCH).map(|_| Message::Looked),
        ])
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Ask => {
                let session = self.session.clone();
                return cosmic::task::future(async move { Message::Answer(blocking(move || torq::app::glance(session.as_deref())).await) });
            }
            Message::Looked => {
                let now = running();
                if now != self.running {
                    self.running = now;
                    return self.update(Message::Ask);
                }
            }
            Message::Answer(g) => self.glance = Some(g),
            Message::Confirm => {
                // In a terminal: the broker may ask the person's password, never typed in a window TorqOS draws.
                let script = format!("{}; echo; read -r -p 'Press Enter to close. ' _", torq::app::CONFIRM);
                spawn("cosmic-term", &["-e", "sh", "-c", &script]);
            }
            Message::LogOut => spawn("cosmic-osd", &["log-out"]),
            Message::Restart => spawn("cosmic-osd", &["restart"]),
            Message::Open => spawn("torq-settings", &[]),
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::Surface(a) => return cosmic::task::message(cosmic::Action::Surface(a)),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let open = self.popup;
        let button = self.core.applet.icon_button(self.icon_name()).on_press_with_rectangle(move |offset, bounds| {
            if let Some(id) = open {
                return Message::Surface(destroy_popup(id));
            }
            Message::Surface(app_popup::<Applet>(
                |_| Default::default(),
                move |state: &mut Applet| {
                    let id = Id::unique();
                    state.popup = Some(id);
                    let mut settings = state.core.applet.get_popup_settings(state.core.main_window_id().unwrap(), id, None, None, None);
                    settings.positioner.anchor_rect = Rectangle {
                        x: (bounds.x - offset.x) as i32,
                        y: (bounds.y - offset.y) as i32,
                        width: bounds.width as i32,
                        height: bounds.height as i32,
                    };
                    settings
                },
                Some(Box::new(|state: &Applet| state.popup_view().map(cosmic::Action::App))),
            ))
        });
        self.core.applet.applet_tooltip::<Message>(button, "TorqOS", self.popup.is_some(), Message::Surface, None).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        self.popup_view()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

/// The running system.
fn running() -> Option<PathBuf> {
    std::fs::read_link("/run/current-system").ok()
}

/// The system this session started on: recorded once per login in the session's runtime directory (which
/// the login creates and the logout removes), so an applet restarted mid-session still knows it.
fn session_system() -> PathBuf {
    let file = std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("torq-session-system"));
    if let Some(f) = &file {
        if let Ok(s) = std::fs::read_link(f) {
            return s;
        }
    }
    let now = running().unwrap_or_else(|| PathBuf::from("/run/current-system"));
    if let Some(f) = &file {
        let _ = std::os::unix::fs::symlink(&now, f);
    }
    now
}

fn spawn(program: &str, args: &[&str]) {
    let _ = std::process::Command::new(program).args(args).spawn();
}

/// A call to the broker (a blocking socket call) on a thread of its own, so the panel keeps drawing.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f().map_err(|e| format!("{e:#}")));
    });
    rx.await.unwrap_or_else(|_| Err("the call to the broker stopped before it answered".to_string()))
}
