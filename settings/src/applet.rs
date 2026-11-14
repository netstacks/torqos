//! `torq-applet`: TorqOS in COSMIC's panel (ADR-049). The machine's state at a glance and the one action
//! that settles it: a commit waiting for confirmation, a restart or log-out a switch needs, an alarm,
//! proposals waiting, an uncommitted draft. TorqOS Settings holds the detail; this only points there. It
//! changes nothing itself: it confirms through torqcli, approved in COSMIC's own dialog, and asks
//! COSMIC's own dialogs to log out or restart.
//!
//! It is also where a person answers their agent's use of a secret that waits for them (ADR-051, ADR-052 4):
//! its popup is the dialog, one use at a time. What the dialog says, which buttons it has and what each sends
//! are torqcli's library's (`torq::app`); this lays them out.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use cosmic::cosmic_theme::palette::Srgba;
use cosmic::iced::window::Id;
use cosmic::iced::{Length, Rectangle, Subscription};
use cosmic::prelude::*;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget::{self, icon};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use torq::app::{self, Console, Glance, Needs};

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

/// How often the broker is asked, when nothing else says to ask sooner.
const EVERY: Duration = Duration::from_secs(60);
/// How often the running system is looked at: a switch is seen within this, and the broker asked then.
const SWITCH: Duration = Duration::from_secs(3);
/// How often a person's desktop is saved, when saving is automatic.
const DESKTOP: Duration = Duration::from_secs(15 * 60);
/// A notification this long before an unconfirmed change rolls back.
const WARN_SECONDS: i64 = 120;
/// How often the broker is asked whether a use of a secret waits for the person (only for a person who has an
/// agent). Each asking is one request to the broker, so one short-lived broker process; a use waits two minutes,
/// and is seen at most this long after it began to.
const USES: Duration = Duration::from_secs(10);
/// How often while one is shown: its dialog closes this soon after it is answered elsewhere or its time is over.
const USES_SHOWN: Duration = Duration::from_secs(2);
/// How often the dialog is drawn again while a use is shown: the time left, and its buttons coming alive.
const USE_DRAWN: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Applet {
    core: cosmic::Core,
    popup: Option<Id>,
    console: Option<Console>,
    glance: Option<Result<Glance, String>>,
    /// The system this login started on (ADR-049 1): what a log-out would take into use is the difference.
    session: Option<PathBuf>,
    /// The running system when last looked at: a change is a switch.
    running: Option<PathBuf>,
    /// The system a switch has just left: once it has settled, what it changed is started again.
    switched_from: Option<PathBuf>,
    /// The start of a polkit COSMIC's approval dialog was older than, seen at the last look, and the one acted on
    /// (the dialog started again): once per polkit, one look after it is seen, so the new one is on the bus. Lost
    /// with the applet, harmlessly: what is looked at is the processes themselves, never a remembered pid.
    agent_outlived: Option<u64>,
    agent_restarted_for: Option<u64>,
    /// The latest change, when last looked at: another (an undo, a pull) may change the person's desktop.
    last_change: Option<String>,
    /// What was last notified, so each state is said once.
    told: Option<String>,
    /// Why the last save of the person's desktop failed or was refused, why some of the configuration's settings could
    /// not be put on it, and why their apps could not be brought back: each told once, and shown until a try works.
    unsaved: app::Told,
    unapplied: app::Told,
    apps: app::Told,
    /// The confirmation already warned of as about to expire.
    warned: Option<u64>,
    /// Now, for the countdown.
    now: i64,
    /// Who is at the machine: a secret of their own is said to be theirs.
    me: Option<String>,
    /// The agent's uses of secrets that wait for the person: which one the dialog shows.
    asking: app::Asking,
    /// A look at what waits is under way: no second while it is.
    uses_looking: bool,
    /// Why the last look at what waits failed: told once, and shown until a look works.
    uses_unread: app::Told,
    /// The use whose "Allow for…" choices are open.
    allowing: Option<u64>,
    /// The use a notification is up for, and the flag that takes the notification down when the use is over.
    noticed: Option<(u64, Arc<AtomicBool>)>,
    /// The use whose answer is on its way to the broker: no other button acts until it is back.
    replying: Option<u64>,
}

#[derive(Debug, Clone)]
enum Message {
    Ask,
    Looked,
    Tick,
    SyncDesktop,
    /// The settings that could not be put on the desktop, or why the sync failed.
    Synced(Result<Vec<String>, String>),
    /// The ids of the apps that could not be installed, or why the reconcile could not run.
    Reconciled(Result<Vec<String>, String>),
    Answer(Result<Glance, String>),
    Confirm,
    Closed,
    LogOut,
    Restart,
    Open(&'static str),
    AskAgent(String),
    /// What became of opening the agent's window for an alarm.
    AskedAgent(Result<(), String>),
    /// Look at the agent's uses of secrets that wait for the person, and what the broker said.
    Uses,
    UsesHeard(Result<(Vec<app::WaitingUse>, Vec<app::Allowed>), String>),
    /// The notification of a waiting use was clicked (true), or went unclicked.
    UseNoticed(bool),
    /// The dialog's buttons. Each carries the number of the use it was drawn for, and acts on no other.
    /// "Allow for…" opened (true) or closed.
    AllowFor(u64, bool),
    UseReply(u64, app::Reply),
    UseReplied(u64, Result<String, String>),
    /// What became of a use was read.
    UseNoteRead,
    PopupClosed(Id),
    Surface(cosmic::surface::Action<Message>),
}

/// What the panel shows: the first that holds (ADR-049 2).
enum State<'a> {
    Confirm(&'a app::Confirm),
    Needs(&'a Needs),
    Alarm(&'a str),
    Waiting(usize, usize),
    Draft,
    Well,
    Unknown,
}

/// A colour as SVG writes it.
fn hex(c: Srgba) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.red), b(c.green), b(c.blue))
}

/// TorqOS's mark in the panel's own text colour, and a badge in the corner when something waits: drawn
/// here, because a symbolic icon has one colour and the badge is the news.
fn mark(badge: Option<Srgba>) -> icon::Handle {
    let theme = cosmic::theme::active();
    let ink = hex(theme.cosmic().on_bg_color().into());
    let (mask, dot) = match badge {
        Some(c) => (
            "<mask id='m'><rect width='16' height='16' fill='white'/><circle cx='12.4' cy='12.4' r='4.4' fill='black'/></mask>".to_string(),
            format!("<circle cx='12.4' cy='12.4' r='3.2' fill='{}'/>", hex(c)),
        ),
        None => (String::new(), String::new()),
    };
    let masked = if badge.is_some() { " mask='url(#m)'" } else { "" };
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' width='16' height='16'>{mask}<g{masked}><g transform='scale(0.1333)'><path d='M97.59 46.32 A40 40 0 1 1 73.68 22.41' fill='none' stroke='{ink}' stroke-width='18'/><polygon points='115,40 80,53 91,26' fill='{ink}'/></g></g>{dot}</svg>"
    );
    icon::from_svg_bytes(svg.into_bytes())
}

/// A notification, and what clicking it opens (ADR-049 4): on a thread of its own, which waits for it to
/// be clicked or closed.
fn notify(summary: String, body: String, page: &'static str) {
    std::thread::spawn(move || {
        let _ = send(&summary, &body, page);
    });
}

fn send(summary: &str, body: &str, page: &str) -> zbus::Result<()> {
    if clicked(summary, body, -1, None)? {
        if let Err(e) = app::open_settings(page) {
            eprintln!("torq-applet: opening TorqOS Settings from a notification: {e:#}");
            notify(SETTINGS_UNOPENED.to_string(), app::said(&e), "overview");
        }
    }
    Ok(())
}

/// A notification, waited for until it is clicked or goes: whether it was clicked. `expires`: after how many
/// milliseconds the notification daemon is asked to take it down (-1: when it sees fit). `over`: set by the
/// caller when what the notification is about is over, and the notification is then taken down, so that it does
/// not go on offering what can no longer be done.
fn clicked(summary: &str, body: &str, expires: i32, over: Option<Arc<AtomicBool>>) -> zbus::Result<bool> {
    let connection = zbus::blocking::Connection::session()?;
    let bus = zbus::blocking::Proxy::new(&connection, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications")?;
    let signals = bus.receive_all_signals()?;
    let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
    let id: u32 = bus.call("Notify", &("TorqOS", 0u32, "torqos", summary, body, vec!["default", "Open"], hints, expires))?;
    if let Some(over) = over.clone() {
        // On a thread of its own, which looks twice a second: the wait below is for the daemon's signals, and the
        // daemon's word that it closed the notification is what ends it. The flag is set on the way out of here
        // too, so this thread ends with the notification however it went.
        std::thread::spawn(move || {
            while !over.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(500));
            }
            let closed: zbus::Result<()> = zbus::blocking::Proxy::new(&connection, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications").and_then(|bus| bus.call("CloseNotification", &(id,)));
            // Clicked or expired already, there is none to close, and the daemon may say so: as wanted.
            drop(closed);
        });
    }
    let was = (|| {
        for signal in signals {
            let header = signal.header();
            let member = header.member().map(|m| m.to_string()).unwrap_or_default();
            if member == "ActionInvoked" {
                let (of, _action): (u32, String) = signal.body().deserialize()?;
                if of == id {
                    return Ok(true);
                }
            } else if member == "NotificationClosed" {
                let (of, _why): (u32, u32) = signal.body().deserialize()?;
                if of == id {
                    break;
                }
            }
        }
        Ok(false)
    })();
    if let Some(over) = over {
        over.store(true, Ordering::Relaxed);
    }
    was
}

/// Tell the person that a use of a secret waits for them (`summary`, `body`: `WaitingUse::notice`), for as long
/// as it waits (`seconds`, or until `over` is set: the use answered, or its time gone). Clicked, the dialog
/// opens. A notification that could not be shown is in the journal: the panel's mark and its popup say that a
/// use waits all the same.
fn notify_use(summary: String, body: String, seconds: i64, over: Arc<AtomicBool>) -> Task<cosmic::Action<Message>> {
    let expires = i32::try_from(seconds.clamp(1, 600) * 1000).unwrap_or(i32::MAX);
    cosmic::task::future(async move {
        Message::UseNoticed(match off(move || clicked(&summary, &body, expires, Some(over))).await {
            Ok(Ok(was)) => was,
            Ok(Err(e)) => {
                eprintln!("torq-applet: the notification that a use of a secret waits: {e}");
                false
            }
            Err(e) => {
                eprintln!("torq-applet: the notification that a use of a secret waits: {e}");
                false
            }
        })
    })
}

/// The popup, opened: from a press on the panel's button (`anchor`: where the button is), or by the applet
/// itself for a notification that was clicked.
fn open_popup(anchor: Option<Rectangle<i32>>) -> cosmic::surface::Action<Message> {
    app_popup::<Applet>(
        |_| Default::default(),
        move |state: &mut Applet| {
            let id = Id::unique();
            state.popup = Some(id);
            // A dialog in it is only now in front of the person: its buttons wait a moment from here.
            state.asking.drawn(Instant::now());
            let mut settings = state.core.applet.get_popup_settings(state.core.main_window_id().unwrap(), id, None, None, None);
            match anchor {
                Some(at) => settings.positioner.anchor_rect = at,
                // No press on the panel came with it, and a popup may take the pointer only with one: it is
                // opened without taking it, and closed by the panel's button.
                None => settings.grab = false,
            }
            settings
        },
        Some(Box::new(|state: &Applet| state.popup_view().map(cosmic::Action::App))),
    )
}

/// COSMIC's app library and launcher watch this folder, not the system's, whose path a switch replaces: a
/// file come and gone makes them read their menu again, so a new application appears (ADR-049 5).
fn refresh_menu() {
    let Some(home) = std::env::var_os("HOME") else { return };
    let dir = PathBuf::from(home).join(".local/share/applications");
    let _ = std::fs::create_dir_all(&dir);
    let touch = dir.join(".torq-switched");
    if std::fs::write(&touch, "").is_ok() {
        let _ = std::fs::remove_file(&touch);
    }
}

impl Applet {
    fn state(&self) -> State<'_> {
        let Some(Ok(g)) = &self.glance else { return State::Unknown };
        if let Some(c) = &g.confirm {
            return State::Confirm(c);
        }
        if matches!(g.needs, Needs::LogOut(_) | Needs::Restart(_)) {
            return State::Needs(&g.needs);
        }
        if let Some(a) = &g.alarm {
            return State::Alarm(a);
        }
        if g.waiting.0 > 0 {
            return State::Waiting(g.waiting.0, g.waiting.1);
        }
        if g.draft {
            return State::Draft;
        }
        State::Well
    }

    /// The badge: nothing when all is well; the theme's colours for what waits.
    fn icon(&self) -> icon::Handle {
        let theme = cosmic::theme::active();
        let c = theme.cosmic();
        // A use of a secret that waits is answered within two minutes or not at all: it comes first.
        if self.asking.shown().is_some() {
            return mark(Some(c.warning_color()));
        }
        mark(match self.state() {
            State::Confirm(_) | State::Needs(Needs::Restart(_)) => Some(c.warning_color()),
            State::Alarm(_) => Some(c.destructive_color()),
            State::Needs(_) | State::Waiting(..) | State::Draft => Some(c.accent_color()),
            State::Well | State::Unknown => None,
        })
    }

    /// What a state says when it starts: its words, and the page that has the detail.
    fn tell(&self) -> Option<(String, String, &'static str)> {
        match self.state() {
            State::Confirm(c) => Some((format!("Confirm change #{}", c.seq), format!("It rolls back at {} unless you confirm it.", c.deadline), "overview")),
            State::Needs(n) => Some((match n { Needs::Restart(_) => "Restart needed".to_string(), _ => "Log out needed".to_string() }, n.line().unwrap_or_default(), "overview")),
            State::Alarm(a) => Some(("TorqOS needs a look".to_string(), a.to_string(), "overview")),
            State::Waiting(n, t) => Some((
                if n == 1 { "A proposal is waiting".to_string() } else { format!("{n} proposals are waiting") },
                if t > 0 { format!("{t} came from a session that read untrusted content.") } else { "Review it in TorqOS Settings.".to_string() },
                "proposals",
            )),
            State::Draft | State::Well | State::Unknown => None,
        }
    }

    /// What became of an answer given here, or of a use that ended without one: a failure in the destructive style.
    fn note_line(note: &app::Note) -> Element<'static, Message> {
        let said = widget::text::caption(note.text.clone()).width(Length::Fill);
        let theme = cosmic::theme::active();
        if note.failed { said.class(cosmic::theme::Text::Color(theme.cosmic().destructive_color().into())).into() } else { said.into() }
    }

    /// The notification of a use that is no longer the one in front (answered, or its time over) is taken down:
    /// it would go on saying "Open this to answer".
    fn withdraw_notice(&mut self) {
        if let Some((n, over)) = &self.noticed {
            if self.asking.shown().map(|(u, _)| u.n) != Some(*n) {
                over.store(true, Ordering::Relaxed);
                self.noticed = None;
            }
        }
    }

    /// The dialog for the agent's use of a secret that waits (ADR-051, ADR-052 4): the broker's facts first, then,
    /// apart and marked as the agent's, the agent's own words; then the buttons. Laid out from `d`, which
    /// `torq::app` made: nothing is decided here. `note`: why an answer to this very use was not taken.
    /// No button here is the highlighted one: nothing in a prompt like this is the default.
    fn use_view(&self, d: app::UseDialog, note: Option<&app::Note>) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let theme = cosmic::theme::active();
        let red = cosmic::theme::Text::Color(theme.cosmic().destructive_color().into());
        let n = d.n;
        // No button acts in the first moment after what is drawn changed, nor while an answer is on its way.
        let live = self.replying.is_none() && self.asking.takes(n, Instant::now());
        let mut col = widget::column::with_capacity(12).spacing(spacing.space_xs).padding(spacing.space_s).width(Length::Fixed(340.0));
        col = col.push(widget::text::heading("Your agent asks to use a secret"));
        // Always a line, whether or not another use waits: one coming or going moves nothing below it.
        col = col.push(widget::text::caption(d.more));
        for (label, value, marked) in d.facts {
            let value = widget::text::body(value).width(Length::Fill);
            let value = if marked { value.class(red) } else { value };
            col = col.push(widget::row::with_capacity(2).spacing(spacing.space_xs).push(widget::text::caption(label).width(Length::Fixed(76.0))).push(value));
        }
        col = col.push(widget::text::caption(d.left).width(Length::Fill));
        if !d.agent_says.is_empty() {
            col = col.push(widget::divider::horizontal::default());
            col = col.push(widget::text::caption(app::AGENT_SAYS).width(Length::Fill));
            // Plain text in a fixed-width font, a line as `torq::app` broke it, never wrapped again here: each
            // line drawn keeps its mark.
            let mut words = widget::column::with_capacity(d.agent_says.len());
            for line in d.agent_says {
                words = words.push(widget::text::monotext(line).size(12).wrapping(cosmic::iced::widget::text::Wrapping::None));
            }
            col = col.push(widget::container(words).padding(spacing.space_xs).width(Length::Fill).class(cosmic::theme::Container::Secondary));
        }
        col = col.push(widget::divider::horizontal::default());
        if let Some(note) = note {
            col = col.push(Self::note_line(note));
        }
        if self.replying == Some(n) {
            col = col.push(widget::text::caption("Sending your answer…"));
        }
        if self.allowing == Some(n) {
            col = col.push(widget::text::body("Let your agent use this secret without asking you. Your password is asked first.").width(Length::Fill));
            if let Some(note) = d.offers_note {
                col = col.push(widget::text::caption(note).width(Length::Fill));
            }
            if let Some(replaces) = d.replaces {
                col = col.push(widget::text::caption(replaces).width(Length::Fill));
            }
            let warn = d.offers.iter().any(|o| o.red);
            for o in d.offers {
                let reply = live.then_some(Message::UseReply(n, app::Reply::For(o.allowance)));
                col = col.push(if o.red { widget::button::destructive(o.label).on_press_maybe(reply) } else { widget::button::standard(o.label).on_press_maybe(reply) });
            }
            if warn {
                col = col.push(widget::text::caption(app::EVEN_TAINTED_WARNING).class(red).width(Length::Fill));
            }
            col = col.push(widget::button::text("Back").on_press_maybe(live.then_some(Message::AllowFor(n, false))));
        } else {
            col = col.push(
                widget::row::with_capacity(2)
                    .spacing(spacing.space_xs)
                    .push(widget::button::standard("Allow once").on_press_maybe(live.then_some(Message::UseReply(n, app::Reply::Once))))
                    .push(widget::button::standard("Allow for…").on_press_maybe(live.then_some(Message::AllowFor(n, true)))),
            );
            col = col.push(widget::button::destructive("Deny").on_press_maybe(live.then_some(Message::UseReply(n, app::Reply::Deny))));
            if d.deny_session {
                col = col.push(widget::button::destructive("Deny, and don't ask again this session").on_press_maybe(live.then_some(Message::UseReply(n, app::Reply::DenySession))));
            }
        }
        self.core.applet.popup_container(col).into()
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        match self.asking.showing() {
            // One dialog at a time, and before anything else the popup has to say.
            app::Showing::Use(u, more, note) => return self.use_view(u.dialog(self.me.as_deref().unwrap_or_default(), more, self.now, self.asking.held()), note),
            // What became of a use, alone until it is read (or, with another use waiting behind it and no
            // failure to read, for a few seconds): a press meant for the dialog's buttons, as it goes, lands
            // on none of the next dialog's, and on none of the popup's own.
            app::Showing::Note(note) => {
                let col = widget::column::with_capacity(3)
                    .spacing(spacing.space_s)
                    .padding(spacing.space_s)
                    .width(Length::Fixed(340.0))
                    .push(widget::text::heading("Your agent's use of a secret"))
                    .push(Self::note_line(note))
                    .push(widget::button::standard("OK").on_press(Message::UseNoteRead));
                return self.core.applet.popup_container(col).into();
            }
            app::Showing::Nothing => {}
        }
        // The popup's own buttons take no press in the first moment after a note or a dialog stood here.
        let settled = self.asking.settled(Instant::now());
        let mut col = widget::column::with_capacity(6).spacing(spacing.space_s).padding(spacing.space_s).width(Length::Fixed(340.0));
        let version = match &self.glance {
            Some(Ok(g)) => format!("TorqOS {}", g.version),
            _ => "TorqOS".into(),
        };
        col = col.push(widget::text::heading(version));
        let line = |s: String| widget::text::body(s).width(Length::Fill);
        let mut open = "overview";
        match self.state() {
            State::Confirm(c) => {
                let left = c.at.map(|at| (at - self.now).max(0)).map(|s| format!("{}:{:02} left. ", s / 60, s % 60)).unwrap_or_default();
                col = col.push(line(format!("{left}Change #{} rolls back at {} unless you confirm it.", c.seq, c.deadline)));
                let both = matches!(&self.glance, Some(Ok(g)) if matches!(g.needs, Needs::Restart(_)));
                col = col.push(widget::button::suggested(if both { "Confirm, then restart" } else { "Confirm" }).on_press_maybe(settled.then_some(Message::Confirm)));
            }
            State::Needs(n) => {
                col = col.push(line(n.line().unwrap_or_default()));
                col = col.push(match n {
                    Needs::Restart(_) => widget::button::suggested("Restart…").on_press_maybe(settled.then_some(Message::Restart)),
                    _ => widget::button::suggested("Log out…").on_press_maybe(settled.then_some(Message::LogOut)),
                });
            }
            State::Alarm(a) => {
                col = col.push(line(a.to_string()));
                // The agent can look at it with the person: the alarm attached (ADR-051 4).
                col = col.push(widget::button::text("Ask the agent").on_press_maybe(settled.then(|| Message::AskAgent(a.to_string()))));
            }
            State::Waiting(n, tainted) => {
                open = "proposals";
                let what = if n == 1 { "A proposal is".to_string() } else { format!("{n} proposals are") };
                let mut s = format!("{what} waiting for you to approve or refuse.");
                if tainted > 0 {
                    s.push_str(&format!(" {tainted} came from a session that read untrusted content."));
                }
                col = col.push(line(s));
            }
            State::Draft => {
                open = "changes";
                col = col.push(line("You have changes not yet committed.".into()));
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
        for (told, what) in [(&self.unsaved, UNSAVED), (&self.unapplied, UNAPPLIED), (&self.apps, APPS), (&self.uses_unread, USES_UNREAD)] {
            if let Some(why) = told.why() {
                col = col.push(widget::text::caption(format!("{what}: {why}")));
            }
        }
        col = col.push(widget::divider::horizontal::default());
        col = col.push(widget::button::text("Open TorqOS Settings").on_press_maybe(settled.then_some(Message::Open(open))));
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
        let mut app = Applet { core, console: Some(Console::watcher()), session: app::session_system(), running: running(), now: app::now(), me: app::who().ok(), ..Default::default() };
        // A rebuilt machine's apps and desktop come back at the first login.
        let ask = app.update(Message::Ask);
        // A use that already waits is shown without waiting for the first beat.
        let uses = app.update(Message::Uses);
        (app, Task::batch([reconcile().chain(sync()), ask, uses]))
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut all = vec![cosmic::iced::time::every(EVERY).map(|_| Message::Ask), cosmic::iced::time::every(SWITCH).map(|_| Message::Looked), cosmic::iced::time::every(DESKTOP).map(|_| Message::SyncDesktop)];
        // The countdown, only while there is one.
        if matches!(self.state(), State::Confirm(_)) {
            all.push(cosmic::iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick));
        }
        // The agent's uses of secrets that wait: sooner, and its dialog drawn again, while one is shown; drawn
        // again too while a change of what is drawn settles, so that buttons come alive.
        let shown = self.asking.shown().is_some();
        all.push(cosmic::iced::time::every(if shown { USES_SHOWN } else { USES }).map(|_| Message::Uses));
        if shown || self.asking.settling() {
            all.push(cosmic::iced::time::every(USE_DRAWN).map(|_| Message::Tick));
        }
        Subscription::batch(all)
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Ask => {
                let session = self.session.clone();
                let console = self.console.clone();
                return cosmic::task::future(async move {
                    Message::Answer(blocking(move || app::glance(session.as_deref(), console.is_some_and(|c| c.uncommitted()))).await)
                });
            }
            Message::Looked => {
                // polkit replaced (a switch can): COSMIC's agent registered with the old one and must start again,
                // or no approval can be asked. Seen from the processes' start times, so also when this applet itself
                // started after the replacement (a panel restarted, a crash, a login while polkit restarts).
                match app::approval_agent_outlived() {
                    Some(polkit) if self.agent_restarted_for != Some(polkit) => {
                        if self.agent_outlived == Some(polkit) {
                            app::restart_approval_agent();
                            self.agent_restarted_for = Some(polkit);
                            self.agent_outlived = None;
                        } else {
                            self.agent_outlived = Some(polkit);
                        }
                    }
                    _ => self.agent_outlived = None,
                }
                let now = running();
                if now != self.running {
                    // The first system left, if a switch follows a switch: what is running came from it.
                    self.switched_from = self.switched_from.take().or_else(|| self.running.clone());
                    self.running = now;
                    refresh_menu();
                    return self.update(Message::Ask);
                }
                // Settled: the panel and the rest, where they changed, without a new login. Open windows stay.
                if let (Some(old), Some(new)) = (self.switched_from.take(), now) {
                    // The configuration may name other apps now: an undo, or a pull from the remote.
                    app::restart_parts(&app::stale_parts(&old, &new));
                    return reconcile();
                }
            }
            // Not while a commit confirmed waits: the broker takes no change until it is confirmed or rolled back.
            Message::SyncDesktop if matches!(self.state(), State::Confirm(_)) => {}
            Message::SyncDesktop => return sync(),
            // What failed is said in its own words, once for each reason: the reasons differ (a change waiting to be
            // confirmed, the broker not answering, a refusal), so no one piece of advice fits them all.
            Message::Synced(outcome) => {
                // A sync that failed says nothing of what it could put on the desktop: that stands as it was.
                if let Ok(failed) = &outcome {
                    let words = (!failed.is_empty()).then(|| app::desktop_words(failed));
                    if let Some(words) = self.unapplied.heard(words.map_or(Ok(()), Err)) {
                        notify(UNAPPLIED.to_string(), words, "desktop");
                    }
                }
                if let Some(why) = self.unsaved.heard(outcome.map(|_| ())) {
                    notify(UNSAVED.to_string(), why, "desktop");
                }
            }
            Message::Reconciled(outcome) => {
                if let Some(why) = self.apps.heard(app::apps_outcome(outcome)) {
                    notify(APPS.to_string(), why, "apps");
                }
            }
            Message::Tick => {
                self.asking.tick(Instant::now());
                self.now = app::now();
                if let State::Confirm(c) = self.state() {
                    let (seq, deadline, left) = (c.seq, c.deadline.clone(), c.at.map(|at| at - self.now));
                    if left.is_some_and(|l| l <= WARN_SECONDS) && self.warned != Some(seq) {
                        self.warned = Some(seq);
                        notify(format!("Change #{seq} is about to roll back"), format!("Confirm it before {deadline}, or the machine undoes it."), "overview");
                    }
                }
            }
            Message::Answer(g) => {
                let first = self.glance.is_none();
                let failed = g.is_err();
                let mut then = Task::none();
                // A change since the last look may be to the person's desktop (an undo of one): put it on the desktop.
                if let Ok(now) = &g {
                    if self.last_change.is_some() && self.last_change != now.last && now.confirm.is_none() {
                        then = sync();
                    }
                    self.last_change = now.last.clone();
                }
                self.glance = Some(g);
                self.now = app::now();
                // A failed ask says nothing of the state: what was said stands, and is not said again.
                if !failed {
                    let tell = self.tell();
                    let key = tell.as_ref().map(|(s, b, _)| format!("{s}{b}"));
                    // Each state said once, when it starts: not the one the session opened on.
                    if key != self.told {
                        if let (Some((summary, body, page)), false) = (tell, first) {
                            notify(summary, body, page);
                        }
                        self.told = key;
                    }
                }
                return then;
            }
            // The popup's own buttons, in the first moment after a note or a dialog stood where they are: a press
            // meant for that is not one for them. (They are drawn dead for that moment too.)
            Message::Confirm | Message::LogOut | Message::Restart | Message::Open(_) | Message::AskAgent(_) if !self.asking.settled(Instant::now()) => {}
            Message::Confirm => {
                let restart = matches!(&self.glance, Some(Ok(g)) if matches!(g.needs, Needs::Restart(_)));
                return cosmic::task::future(async move {
                    let confirmed = blocking(app::confirm).await.is_ok();
                    if restart && confirmed {
                        told(app::ask_restart());
                    }
                    Message::Closed
                });
            }
            Message::Closed => return self.update(Message::Ask),
            Message::LogOut => told(app::ask_log_out()),
            Message::Restart => told(app::ask_restart()),
            Message::Open(page) => told(app::open_settings(page)),
            Message::AskAgent(alarm) => {
                // Off the panel's thread (step 5's second review): systemd-run is waited for, bounded.
                let h = torq::agent::handoff::About::Alarm(&alarm).handoff();
                return cosmic::task::future(async move { Message::AskedAgent(blocking(move || torq::agent::handoff::open_window(&h, None)).await) });
            }
            // The person pressed the button and nothing opened: said, as the applet's other failures are.
            Message::AskedAgent(outcome) => {
                if let Err(why) = outcome {
                    notify(AGENT_UNOPENED.to_string(), why, "agent");
                }
            }
            Message::Uses => {
                if self.uses_looking {
                    return Task::none();
                }
                self.uses_looking = true;
                // Asked only for a person who has an agent: no one else has a use waiting for them.
                return cosmic::task::future(async { Message::UsesHeard(blocking(|| if app::has_agent() { app::uses_waiting() } else { Ok((vec![], vec![])) }).await) });
            }
            Message::UsesHeard(Err(why)) => {
                self.uses_looking = false;
                // The person would not learn of a use that waits: said, once for each reason.
                if let Some(why) = self.uses_unread.heard(Err(why)) {
                    notify(USES_UNREAD.to_string(), why, "secrets");
                }
            }
            Message::UsesHeard(Ok((waiting, allowed))) => {
                self.uses_looking = false;
                // (Nothing to tell: a look that worked only forgets why the last one failed.)
                let _ = self.uses_unread.heard(Ok(()));
                self.now = app::now();
                // A use that ended without an answer given here leaves a note in the dialog's place (`Asking`).
                let tell = self.asking.heard(waiting, self.now, Instant::now());
                let me = self.me.clone().unwrap_or_default();
                // Whether the person already allows the secret the use shown asks for: the dialog says so.
                self.asking.allows(&allowed, &me, Instant::now());
                self.withdraw_notice();
                // The choices of "Allow for…" are of the use they were opened for, and of no other.
                if self.allowing != self.asking.shown().map(|(u, _)| u.n) {
                    self.allowing = None;
                }
                if let Some(u) = tell {
                    // Its number alone: the journal is not where a secret's name or an agent's words belong.
                    eprintln!("torq-applet: use {} of a secret waits for an answer", u.n);
                    let (summary, body) = u.notice(&me);
                    let over = Arc::new(AtomicBool::new(false));
                    self.noticed = Some((u.n, over.clone()));
                    return notify_use(summary, body, u.until.map_or(120, |t| t - self.now), over);
                }
            }
            Message::UseNoticed(false) => {}
            // The dialog, or, if the use is over by now, what became of it.
            Message::UseNoticed(true) => {
                if self.popup.is_none() {
                    return cosmic::task::message(cosmic::Action::Surface(open_popup(None)));
                }
            }
            // Only for the use whose dialog is drawn, and not in the first moment after what is drawn changed
            // (`Asking::takes`): a press that was meant for what the popup showed before does nothing. Opening or
            // closing the choices changes what is drawn, so the buttons wait again: a second click of a
            // double-click lands on none of them.
            Message::AllowFor(n, open) => {
                let now = Instant::now();
                if self.replying.is_none() && self.asking.takes(n, now) {
                    self.allowing = open.then_some(n);
                    self.asking.drawn(now);
                }
            }
            Message::UseReply(n, reply) => {
                let Some((u, _)) = self.asking.shown() else { return Task::none() };
                if self.replying.is_some() || !self.asking.takes(n, Instant::now()) {
                    return Task::none();
                }
                let (u, held) = (u.clone(), self.asking.held().cloned());
                self.replying = Some(n);
                return cosmic::task::future(async move { Message::UseReplied(n, blocking(move || app::answer_use(&u, reply, held.as_ref())).await) });
            }
            // What was done, or why not, is `Asking`'s to keep and to show: a failure until it is read.
            Message::UseReplied(n, outcome) => {
                self.replying = None;
                self.allowing = None;
                self.asking.replied(n, outcome, Instant::now());
                self.withdraw_notice();
                return self.update(Message::Uses);
            }
            Message::UseNoteRead => self.asking.read(Instant::now()),
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.asking.closed();
                }
            }
            Message::Surface(a) => return cosmic::task::message(cosmic::Action::Surface(a)),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let open = self.popup;
        // At a symbolic icon's size, as the panel's other status icons are: the mark is drawn SVG (its badge has its
        // own colour), which `icon_button_from_handle` would size as a full-colour app icon, a size larger.
        let (w, h) = self.core.applet.suggested_size(true);
        let mark = widget::icon(self.icon()).width(Length::Fixed(w as f32)).height(Length::Fixed(h as f32));
        let button = self.core.applet.button_from_element(mark, true).on_press_with_rectangle(move |offset, bounds| {
            if let Some(id) = open {
                return Message::Surface(destroy_popup(id));
            }
            Message::Surface(open_popup(Some(Rectangle {
                x: (bounds.x - offset.x) as i32,
                y: (bounds.y - offset.y) as i32,
                width: bounds.width as i32,
                height: bounds.height as i32,
            })))
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

/// What the panel says when a try of its own fails, before the reason.
const UNSAVED: &str = "Your desktop was not saved";
const UNAPPLIED: &str = "Not put on your desktop";
const APPS: &str = "Your apps were not brought back";
const USES_UNREAD: &str = "Could not ask whether your agent waits for your answer";
/// What a notification says when an Ask the agent button opened nothing.
const AGENT_UNOPENED: &str = "The agent's window did not open";
/// What a notification says when TorqOS Settings, a log-out or a restart could not be started from the panel.
const SETTINGS_UNOPENED: &str = "TorqOS could not open that";

/// A program the panel starts that could not start: said in a notification.
fn told(r: anyhow::Result<()>) {
    if let Err(e) = r {
        notify(SETTINGS_UNOPENED.to_string(), app::said(&e), "overview");
    }
}

/// Bring the person's desktop and configuration together, and answer what could not be put on the desktop, or why
/// the sync (a save among it) failed or was refused.
fn sync() -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async { Message::Synced(blocking(|| app::sync_desktop(false).map(|s| s.failed)).await) })
}

/// Bring the person's apps in line with the configuration, and answer why that failed. An app that could not be
/// installed is told once for each set of them (`app::apps_outcome`), and tried again at the next reconcile.
fn reconcile() -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async { Message::Reconciled(blocking(app::reconcile_apps).await) })
}

/// A call that blocks on a thread of its own, so the panel keeps drawing.
async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.map_err(|_| "the call stopped before it answered".to_string())
}

/// `off`, for a call of the library's: its error as torqcli would say it.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    off(move || f().map_err(|e| app::said(&e))).await.and_then(|r| r)
}
