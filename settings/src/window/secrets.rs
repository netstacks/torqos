//! The Secrets page (ADR-052 step 5): `show system secrets`, and each `request system secret …` the command
//! line has, run as that line by torqcli's own session, so the broker asks what it asks of torqcli. One thing
//! here is not a line: a value, typed twice in masked fields, goes to the broker as torqcli's own does
//! (`app::secret_set`), and is no part of any command, log or message. A value is never shown here:
//! `request system secret show` stays a terminal's.

use super::{blocking, Message, Settings};
use cosmic::iced::Length;
use cosmic::prelude::*;
use cosmic::widget::{self, settings};
use torq::app::{self, At};

/// The list, as the command line prints it: names, kinds, whose, and what each may be used for. Never a value.
pub(super) const SHOWN: &str = "show system secrets";

/// What a masked field holds, on its way from the field to the page: never printed, as a `Message` may be.
#[derive(Clone)]
pub struct Masked(String);

impl std::fmt::Debug for Masked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Masked")
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Name(String),
    Mine(bool),
    Host(String),
    Value(Masked),
    Again(Masked),
    Keep,
    Kept(Result<String, String>),
}

/// What the page's fields hold.
pub(super) struct State {
    /// Who is at the machine, for a secret of their own (None: `id -un` did not say).
    me: Option<String>,
    /// The window's memory is closed to other programs (`app::secret_guard`): before that, no value is taken.
    guarded: bool,
    name: String,
    /// The person's own secret, not the system's.
    mine: bool,
    host: String,
    // The value, typed twice. The toolkit's text input keeps a copy of its own of what a field shows, until the
    // field is next drawn empty; and memory given back is not overwritten, here or in the toolkit. What this
    // page can do is hold them no longer than it must: both are taken out when Keep is pressed, kept or not.
    value: String,
    again: String,
}

impl Default for State {
    fn default() -> Self {
        State { me: app::who().ok(), guarded: false, name: String::new(), mine: false, host: String::new(), value: String::new(), again: String::new() }
    }
}

impl Settings {
    pub(super) fn secrets_update(&mut self, m: Msg) -> Task<cosmic::Action<Message>> {
        // Before the page holds anything typed on it: a value is not kept in a memory another program can read.
        if !self.secrets.guarded {
            if let Err(e) = app::secret_guard() {
                self.note = Some((app::said(&e), true));
                return Task::none();
            }
            self.secrets.guarded = true;
        }
        let s = &mut self.secrets;
        match m {
            Msg::Name(n) => s.name = n,
            Msg::Mine(on) => s.mine = on,
            Msg::Host(h) => s.host = h,
            Msg::Value(v) => s.value = v.0,
            Msg::Again(v) => s.again = v.0,
            Msg::Keep => {
                // Out of the page, and so out of both fields, before anything is sent: whatever the broker
                // answers, they are empty. The two are dropped when the call returns.
                let (value, again) = (std::mem::take(&mut s.value), std::mem::take(&mut s.again));
                let name = s.name.trim().to_string();
                let user = if s.mine { s.me.clone() } else { None };
                // The command, as a terminal would be given it: the value is no part of it.
                self.busy = Some(command(&["set", &name], user.as_deref()));
                self.note = None;
                return cosmic::task::future(async move { Message::Secrets(Msg::Kept(blocking(move || app::secret_set(&name, user.as_deref(), &value, &again)).await)) });
            }
            Msg::Kept(r) => {
                self.busy = None;
                self.note = Some(match r {
                    Ok(said) => (said.trim().to_string(), false),
                    Err(e) => (e, true),
                });
                return self.load();
            }
        }
        Task::none()
    }

    pub(super) fn secrets_page(&self) -> Element<'_, Message> {
        let s = &self.secrets;
        let spacing = cosmic::theme::spacing();
        // As the lines below show them; with either still to be typed, nothing that needs it can be run.
        let name = if s.name.trim().is_empty() { "<name>" } else { s.name.trim() };
        let host = if s.host.trim().is_empty() { "<host>" } else { s.host.trim() };
        let user = if s.mine { s.me.as_deref() } else { None };
        let named = self.busy.is_none() && !s.name.trim().is_empty();
        let hosted = named && !s.host.trim().is_empty();
        // One of torqcli's commands for this secret: what it does, the line itself, and the button that runs it.
        let run = |label: &'static str, words: &[&str], ready: bool| {
            let line = command(words, user);
            settings::item::builder(label).description(line.clone()).control(widget::button::standard("Run").on_press_maybe(ready.then(|| Message::Act(At::Read, line))))
        };

        let mut which = settings::section().title("Which secret").add(
            settings::item::builder("Name").description("Letters, digits, - and _").control(widget::text_input("github-ci", &s.name).on_input(|n| Message::Secrets(Msg::Name(n))).width(Length::Fixed(280.0))),
        );
        which = match &s.me {
            Some(me) => which.add(
                settings::item::builder(format!("One of your own ({me})"))
                    .description("Yours alone: no password, and no one else sets or reads it. Off: the system's, an administrator's, approved in the dialog.")
                    .control(widget::toggler(s.mine).on_toggle(|on| Message::Secrets(Msg::Mine(on)))),
            ),
            None => which.add(widget::text::caption("Who you are could not be told (id -un): only the system's secrets can be changed here.")),
        };

        let value = settings::section()
            .title("Its value")
            .add(settings::item::builder("Value").description("Not shown as you type").control(masked(&s.value, Msg::Value)))
            .add(settings::item::builder("Again").control(masked(&s.again, Msg::Again)))
            .add(settings::item::builder("Keep it: encrypted, outside the configuration, never in the history").description(command(&["set", name], user)).control(widget::button::suggested("Keep").on_press_maybe(named.then_some(Message::Secrets(Msg::Keep)))));

        let uses = settings::section()
            .title("What it may be used for")
            .add(settings::item::builder("Host").description("A DNS name, for the four below").control(widget::text_input("api.github.com", &s.host).on_input(|h| Message::Secrets(Msg::Host(h))).width(Length::Fixed(280.0))))
            .add(run("Send it to this host, in an HTTPS header (a token)", &["allow", name, "https", host], hosted))
            .add(run("Stop sending it to this host", &["disallow", name, "https", host], hosted))
            .add(run("Sign for SSH to this host (an SSH key)", &["allow", name, "ssh", host], hosted))
            .add(run("Stop signing for this host", &["disallow", name, "ssh", host], hosted))
            .add(run("Let the agent use it, never reading it", &["allow", name, "agent"], named))
            .add(run("Stop the agent using it", &["disallow", name, "agent"], named));

        let approve = settings::section()
            .title("Before the agent uses it, a person is asked")
            .add(run("Never", &["approve", name, "never"], named))
            .add(run("The first time in each of the agent's sessions", &["approve", name, "first-use"], named))
            .add(run("Every time", &["approve", name, "each-use"], named));

        let delete_line = command(&["delete", name], user);
        let delete = settings::section().title("Delete").add(
            settings::item::builder("Delete its value and its rule: the value cannot be brought back")
                .description(delete_line.clone())
                .control(widget::button::destructive("Delete").on_press_maybe(named.then(|| Message::Act(At::Read, delete_line)))),
        );

        widget::column::with_capacity(9)
            .spacing(spacing.space_m)
            .push(widget::text::body(
                "Tokens and SSH keys that a program, or the agent, uses through TorqOS and never reads. Not a password manager: everyday passwords belong in an app made for them.",
            ))
            .push(settings::section().title("The secrets").add(settings::item_row(vec![self.output(SHOWN)])))
            .push(which)
            .push(value)
            .push(widget::text::caption(format!(
                "What a secret can do at its host is whatever that token or key can do there: give a narrow one (a fine-grained token, a deploy key). A value is never shown here: {}, in a terminal, prints it. A value pasted here stays in the clipboard until something else is copied. A key of several lines is given from its file, in a terminal: request system secret set <name> file <path>.",
                command(&["show", name], user)
            )))
            .push(uses)
            .push(approve)
            .push(widget::text::caption(
                "The asking itself is not built yet. Until it is, the agent may use only a secret set to never, and only from a session that has read nothing from outside (a web page, an email, a downloaded file).",
            ))
            .push(delete)
            .into()
    }
}

/// `request system secret <words> [user <person>]`, as a line of torqcli's.
fn command(words: &[&str], user: Option<&str>) -> String {
    let mut line = vec!["request", "system", "secret"];
    line.extend(words);
    if let Some(u) = user {
        line.extend(["user", u]);
    }
    app::line(&line)
}

/// A field that shows dots for what is typed in it, with no way to show it (no eye to press), and nothing to
/// copy or cut from (the toolkit's own rule for such a field).
fn masked(value: &str, to: fn(Masked) -> Msg) -> Element<'_, Message> {
    widget::secure_input("", value, None, true).on_input(move |v| Message::Secrets(to(Masked(v)))).width(Length::Fixed(280.0)).into()
}
