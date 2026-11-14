//! The Secrets page (ADR-052 step 5): the secrets `show system secrets` lists, one a row, and for the one
//! chosen each `request system secret …` the command line has, run as that line by torqcli's own session, so
//! the broker asks what it asks of torqcli. One thing here is not a line: a value, typed twice in masked fields,
//! goes to the broker as torqcli's own does (`app::secret_set`), and is no part of any command, log or message.
//! A value is never shown here: `request system secret show` stays a terminal's.
//!
//! It also shows what agents are let use without asking for now (the broker's grants, `app::allowed`): all of the
//! person's own at the top, and under the chosen secret every grant that covers it, each
//! with the torqcli line that ends it, and marks a secret whose rule never asks, whatever its session read.

use super::{blocking, failed, waiting, Loaded, Message, Settings};
use cosmic::iced::Length;
use cosmic::prelude::*;
use cosmic::widget::{self, settings};
use torq::app::{self, Allowed, Approve, At, Kind, Secret};

/// What a masked field holds, on its way from the field to the page: never printed, as a `Message` may be.
#[derive(Clone)]
pub struct Masked(String);

impl std::fmt::Debug for Masked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Masked")
    }
}

/// A secret on the list: whose it is (None: the system's), and its name.
type Which = (Option<String>, String);

#[derive(Debug, Clone)]
pub enum Msg {
    /// What `show system secrets` lists, and each rules file that could not be read.
    Listed(Result<(Vec<Secret>, Vec<String>), String>),
    /// What `show system secrets allowed` lists: what agents are let use without asking, for now.
    Allowed(Result<Vec<Allowed>, String>),
    /// A secret of the list chosen (None: a new one, named on the page).
    Choose(Option<Which>),
    Name(String),
    Mine(bool),
    Host(String),
    Value(Masked),
    Again(Masked),
    Keep,
    Kept(Result<String, String>),
    /// Delete pressed once (true), or thought better of (false): the second press is the command itself.
    Deleting(bool),
}

/// What the page holds.
pub(super) struct State {
    /// Who is at the machine, for a secret of their own (None: `id -un` did not say).
    me: Option<String>,
    /// The window's memory is closed to other programs (`app::secret_guard`): before that, nothing typed is taken.
    guarded: bool,
    list: Loaded<(Vec<Secret>, Vec<String>)>,
    /// What agents are let use without asking, as it stood when the page last read it.
    allowed: Loaded<Vec<Allowed>>,
    /// The secret the sections below act on (None: a new one, named by `name` and `mine`).
    chosen: Option<Which>,
    name: String,
    /// The new secret is the person's own, not the system's.
    mine: bool,
    host: String,
    // The value, typed twice. The toolkit's text input keeps a copy of its own of what a field shows, until the
    // field is next drawn empty; and memory given back is not overwritten, here or in the toolkit. What this
    // page can do is hold them no longer than it must: both are taken out when Keep is pressed, kept or not.
    value: String,
    again: String,
    /// Delete was pressed once for the chosen secret.
    deleting: bool,
}

impl Default for State {
    fn default() -> Self {
        State {
            me: app::who().ok(),
            guarded: false,
            list: Loaded::Waiting,
            allowed: Loaded::Waiting,
            chosen: None,
            name: String::new(),
            mine: false,
            host: String::new(),
            value: String::new(),
            again: String::new(),
            deleting: false,
        }
    }
}

impl State {
    /// The secret the page acts on: whose, and its name (empty: a new one not named yet).
    fn which(&self) -> Which {
        app::secret_target(self.chosen.as_ref(), self.mine, self.me.as_deref(), &self.name)
    }

    /// The chosen secret as the list has it.
    fn listed(&self) -> Option<&Secret> {
        let (Loaded::Got((list, _)), Some((user, name))) = (&self.list, &self.chosen) else { return None };
        list.iter().find(|s| &s.user == user && &s.name == name)
    }

    /// "the system's secret ci", "your secret gh", "sam's secret gh".
    fn whose(&self, (user, name): &Which) -> String {
        app::secret_whose(user.as_deref(), name, self.me.as_deref())
    }
}

/// Ask for the list again, and for what is allowed without asking.
pub(super) fn load() -> Task<cosmic::Action<Message>> {
    Task::batch([
        cosmic::task::future(async { Message::Secrets(Msg::Listed(blocking(app::secrets_status).await)) }),
        cosmic::task::future(async { Message::Secrets(Msg::Allowed(blocking(app::allowed).await)) }),
    ])
}

impl Settings {
    pub(super) fn secrets_update(&mut self, m: Msg) -> Task<cosmic::Action<Message>> {
        // Before the page takes anything a person does on it: a value is not kept in a memory another program
        // can read. (The lists arriving are not that, and hold no value.)
        if !self.secrets.guarded && !matches!(m, Msg::Listed(_) | Msg::Allowed(_)) {
            if let Err(e) = app::secret_guard() {
                self.note = Some((app::said(&e), true));
                return Task::none();
            }
            self.secrets.guarded = true;
        }
        let s = &mut self.secrets;
        // Whatever else is done, a Delete pressed once is no longer waiting for its second press.
        s.deleting = matches!(m, Msg::Deleting(true));
        match m {
            Msg::Listed(r) => {
                if let Ok((list, _)) = &r {
                    let on_it = |w: &Which| list.iter().any(|x| x.user == w.0 && x.name == w.1);
                    // The chosen one deleted: nothing is chosen. A new one just made: it is the one chosen.
                    if s.chosen.as_ref().is_some_and(|w| !on_it(w)) {
                        s.chosen = None;
                    } else if s.chosen.is_none() && on_it(&s.which()) {
                        s.chosen = Some(s.which());
                        s.name.clear();
                    }
                }
                s.list = r.into();
            }
            Msg::Allowed(r) => s.allowed = r.into(),
            Msg::Choose(w) => {
                // Another secret: what was typed was for the one before. Left in the masked fields, where it
                // cannot be seen, a value meant for one secret could be kept on another; a host likewise.
                if s.chosen != w {
                    drop((std::mem::take(&mut s.value), std::mem::take(&mut s.again)));
                    s.host.clear();
                    s.chosen = w;
                }
            }
            Msg::Name(n) => s.name = n,
            Msg::Mine(on) => s.mine = on,
            Msg::Host(h) => s.host = h,
            Msg::Value(v) => s.value = v.0,
            Msg::Again(v) => s.again = v.0,
            Msg::Keep => {
                // Out of the page, and so out of both fields, before anything is sent: whatever the broker
                // answers, they are empty. The two are dropped when the call returns.
                let (value, again) = (std::mem::take(&mut s.value), std::mem::take(&mut s.again));
                let (user, name) = s.which();
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
            Msg::Deleting(_) => {}
        }
        Task::none()
    }

    /// One allowance: whose agent, what it covers and until when; in the destructive style, that it holds even
    /// after untrusted text; and (`with_button`) for the person's own, the line that ends it and its button.
    fn allowance(&self, g: &Allowed, with_button: bool) -> Element<'_, Message> {
        let me = self.secrets.me.as_deref();
        let mut said = widget::column::with_capacity(3).width(Length::Fill).push(widget::text::body(g.said(me)));
        if g.even_tainted {
            let theme = cosmic::theme::active();
            said = said.push(widget::text::caption(app::EVEN_TAINTED).class(cosmic::theme::Text::Color(theme.cosmic().destructive_color().into())));
        }
        let (how, ends) = if with_button { g.ending(me) } else { ("Ended at the top of this page.".to_string(), None) };
        let mut row = vec![said.push(widget::text::caption(how)).into()];
        if let Some((label, line)) = ends {
            row.push(widget::button::standard(label).on_press_maybe(self.busy.is_none().then(|| Message::Act(At::Read, line))).into());
        }
        settings::item_row(row).into()
    }

    pub(super) fn secrets_page(&self) -> Element<'_, Message> {
        let s = &self.secrets;
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(10).spacing(spacing.space_m).push(widget::text::body(
            "Tokens and SSH keys that a program, or the agent, uses through TorqOS and never reads. Not a password manager: everyday passwords belong in an app made for them.",
        ));

        // What agents are let use without asking, for now. A list that could not be read is said: no allowance
        // is then shown, and that is not the same as there being none.
        let allowed: &[Allowed] = match &s.allowed {
            Loaded::Got(allowed) => allowed,
            Loaded::Waiting => &[],
            Loaded::Failed(why) => {
                page = page.push(widget::text::caption(format!("What your agent may use without asking could not be read: {why}")));
                &[]
            }
        };
        let theme = cosmic::theme::active();
        let red = cosmic::theme::Text::Color(theme.cosmic().destructive_color().into());
        let me = s.me.as_deref();
        // Everything the person allowed, here, where it is seen on opening the page whatever secret is chosen:
        // the grant of all their own secrets first, then each named one, each with what it holds under (in the
        // destructive style where that is even untrusted text) and the line that ends it (`Allowed::ending`).
        let mut mine: Vec<&Allowed> = allowed.iter().filter(|g| me == Some(g.person.as_str())).collect();
        mine.sort_by_key(|g| g.secret.is_some());
        if !mine.is_empty() {
            let mut section = settings::section().title("What you allowed without asking, for now");
            for g in mine {
                section = section.add(self.allowance(g, true));
            }
            page = page.push(section);
        }

        // The list: what `show system secrets` says of each, and the one the rest of the page is about.
        let mut list = settings::section().title("The secrets");
        match &s.list {
            Loaded::Waiting => list = list.add(waiting()),
            Loaded::Failed(why) => list = list.add(failed(why)),
            Loaded::Got((secrets, unread)) => {
                for why in unread {
                    list = list.add(widget::text::caption(format!("Not read: {why}")));
                }
                for x in secrets {
                    let which = (x.user.clone(), x.name.clone());
                    let whose = match &x.user {
                        None => "the system's".to_string(),
                        Some(u) if Some(u) == s.me.as_ref() => "yours".to_string(),
                        Some(u) => format!("{u}'s"),
                    };
                    list = list.add(settings::item::builder(format!("{} ({whose})", x.name)).description(x.said.clone()).control(choose(s.chosen.as_ref() == Some(&which), Some(which))));
                    // Marked wherever the secret is listed: this one is used with no one asked, whatever was read.
                    if x.approve == Approve::NeverEvenTainted {
                        list = list.add(widget::text::caption(format!("{}: the agent uses it without ever asking. {}", x.name, app::EVEN_TAINTED)).class(red));
                    }
                }
            }
        }
        page = page.push(list.add(settings::item::builder("A new secret").description("Named below; it is on this list once it has a value or a use").control(choose(s.chosen.is_none(), None))));

        if s.chosen.is_none() {
            let mut new = settings::section().title("The new secret").add(
                settings::item::builder("Name").description("Letters, digits, - and _").control(widget::text_input("github-ci", &s.name).on_input(|n| Message::Secrets(Msg::Name(n))).width(Length::Fixed(280.0))),
            );
            new = match &s.me {
                Some(me) => new.add(
                    settings::item::builder(format!("One of your own ({me})"))
                        .description("Yours alone: no password, and no one else sets or reads it. Off: the system's, an administrator's, approved in the dialog.")
                        .control(widget::toggler(s.mine).on_toggle(|on| Message::Secrets(Msg::Mine(on)))),
                ),
                None => new.add(widget::text::caption("Who you are could not be told (id -un): only the system's secrets can be changed here.")),
            };
            page = page.push(new);
        }

        let which = s.which();
        let listed = s.listed();
        let user = which.0.as_deref();
        // As the lines below show them; with either still to be typed, nothing that needs it can be run.
        let name = if which.1.is_empty() { "<name>" } else { which.1.as_str() };
        let host = if s.host.trim().is_empty() { "<host>" } else { s.host.trim() };
        let named = self.busy.is_none() && !which.1.is_empty();
        let hosted = named && !s.host.trim().is_empty();
        let whose = if which.1.is_empty() { "the new secret".to_string() } else { s.whose(&which) };
        // One of torqcli's commands for this secret: what it does, the line itself, and the button that runs it.
        let run = |label: String, words: &[&str], ready: bool| {
            let line = command(words, user);
            settings::item::builder(label).description(line.clone()).control(widget::button::standard("Run").on_press_maybe(ready.then(|| Message::Act(At::Read, line))))
        };

        let keep = if listed.is_some_and(|x| x.has_value) { "Keep it in place of the value it has" } else { "Keep it" };
        page = page
            .push(
                settings::section()
                    .title(format!("The value of {whose}"))
                    .add(settings::item::builder("Value").description("Not shown as you type").control(masked(&s.value, Msg::Value)))
                    .add(settings::item::builder("Again").control(masked(&s.again, Msg::Again)))
                    .add(settings::item::builder(format!("{keep}: encrypted, outside the configuration, never in the history")).description(command(&["set", name], user)).control(widget::button::suggested("Keep").on_press_maybe(named.then_some(Message::Secrets(Msg::Keep))))),
            )
            .push(widget::text::caption(format!(
                "What a secret can do at its host is whatever that token or key can do there: give a narrow one (a fine-grained token, a deploy key). A value is never shown here: {}, in a terminal, prints it. A value pasted here stays in the clipboard until something else is copied. A key of several lines is given from its file, in a terminal: request system secret set <name> file <path>.",
                command(&["show", name], user)
            )));

        // What it may be used for now, each with the line that takes it back; then a host to add.
        let mut uses = settings::section().title(format!("What {whose} may be used for"));
        for h in listed.iter().flat_map(|x| &x.https) {
            uses = uses.add(run(format!("Sent to {h} in an HTTPS header. Stop sending it there"), &["disallow", name, "https", h.as_str()], named));
        }
        for h in listed.iter().flat_map(|x| &x.ssh) {
            uses = uses.add(run(format!("Signs for SSH to {h}. Stop signing for it"), &["disallow", name, "ssh", h.as_str()], named));
        }
        if listed.is_some_and(|x| x.https.is_empty() && x.ssh.is_empty()) {
            uses = uses.add(widget::text::caption("Nothing yet."));
        }
        uses = uses.add(settings::item::builder("Another host").description("A DNS name").control(widget::text_input("api.github.com", &s.host).on_input(|h| Message::Secrets(Msg::Host(h))).width(Length::Fixed(280.0))));
        // A token is sent, an SSH key signs: one that is already the one is not offered the other.
        let kind = listed.and_then(|x| x.kind);
        if kind != Some(Kind::SshKey) {
            uses = uses.add(run("Send it to this host, in an HTTPS header (a token)".into(), &["allow", name, "https", host], hosted));
        }
        if kind != Some(Kind::Token) {
            uses = uses.add(run("Sign for SSH to this host (an SSH key)".into(), &["allow", name, "ssh", host], hosted));
        }
        page = page.push(uses);

        // Who, and when a person is asked: both need a rule to be in, and a use is what makes one.
        match listed.filter(|x| x.kind.is_some()) {
            Some(x) => {
                // A person's own secret: their own agent alone (`agent`, then `user <you>`). A system secret names
                // each person's agent that may use it (`agent <person>`): yours is offered, anyone else's is typed.
                let mut agent = settings::section().title("The agent");
                if user.is_some() {
                    agent = agent.add(if x.agents.is_empty() {
                        run("The agent may not use it. Let the agent use it, never reading it".into(), &["allow", name, "agent"], named)
                    } else {
                        run("The agent may use it, never reading it. Stop the agent using it".into(), &["disallow", name, "agent"], named)
                    });
                } else {
                    for p in &x.agents {
                        agent = agent.add(run(format!("{p}'s agent may use it, never reading it. Stop it using it"), &["disallow", name, "agent", p.as_str()], named));
                    }
                    if let Some(me) = s.me.as_ref().filter(|me| !x.agents.contains(me)) {
                        agent = agent.add(run("Let your agent use it, never reading it".into(), &["allow", name, "agent", me.as_str()], named));
                    }
                    agent = agent.add(widget::text::caption(format!("Another person's agent, on the Command Line page: {}", command(&["allow", name, "agent", "<person>"], None))));
                }
                let mut approve = settings::section().title("Before the agent uses it, a person is asked");
                for (rule, label) in [(Approve::Never, "Never"), (Approve::FirstUse, "The first time in each of the agent's sessions"), (Approve::EachUse, "Every time")] {
                    approve = if x.approve == rule {
                        approve.add(settings::item::builder(label).description("As it is now").control(widget::text::caption("Now")))
                    } else {
                        approve.add(run(label.into(), &["approve", name, rule.word()], named))
                    };
                }
                // Never, even after untrusted text: a person's own secret only, theirs to say, with their password.
                // In the destructive style, set or offered.
                if user.is_some() && user == me {
                    if x.approve == Approve::NeverEvenTainted {
                        approve = approve.add(settings::item::builder("Never, even from a session that read untrusted text").description("As it is now").control(widget::text::caption("Now").class(red)));
                        approve = approve.add(widget::text::caption(format!("Your agent uses {whose} without ever asking you, whatever it has read: text it reads can make it use the secret. It still goes only to the hosts above. Any other line here ends that.")).class(red));
                    } else {
                        approve = approve.add(run("Never, even from a session that read untrusted text. Your password is asked".into(), &["approve", name, Approve::NeverEvenTainted.word()], named));
                        approve = approve.add(widget::text::caption("With that, text your agent reads can make it use the secret without you being asked.").class(red));
                    }
                }
                page = page.push(agent).push(approve).push(widget::text::caption(
                    "A use that asks waits two minutes for the person whose agent it is: they answer in TorqOS's item in the panel, or in a terminal (show system secrets waiting). With no answer it is refused. A session that read untrusted text (a web page, an email, a downloaded file) is asked every time, whatever is chosen above, and so is a coding tool run as the agent, which TorqOS cannot see into.",
                ));
                // What lets its uses through without asking, for now: each allowance that covers this secret, and
                // for the person's own the line that ends it.
                let covering: Vec<&Allowed> = allowed.iter().filter(|g| g.covers(user, name)).collect();
                if !covering.is_empty() {
                    let mut section = settings::section().title(format!("Allowed without asking, for now: {whose}"));
                    for g in covering {
                        // The person's own has its button once, at the top of the page.
                        let at_top = me == Some(g.person.as_str());
                        section = section.add(self.allowance(g, !at_top));
                    }
                    page = page.push(section);
                }
            }
            None => page = page.push(widget::text::caption("Whether the agent may use it, and when a person is asked first, can be said once it has a use (a host, above).")),
        }

        // Delete, in two presses: the first only asks, and anything else done on the page takes the question back.
        if s.chosen.is_some() {
            let line = command(&["delete", name], user);
            let delete = if s.deleting {
                settings::item::builder(format!("Really delete {whose}? Its value cannot be brought back")).description(line.clone()).control(
                    widget::row::with_capacity(2)
                        .spacing(spacing.space_xs)
                        .push(widget::button::standard("Keep it").on_press(Message::Secrets(Msg::Deleting(false))))
                        .push(widget::button::destructive("Delete it").on_press_maybe(named.then(|| Message::Act(At::Read, line)))),
                )
            } else {
                settings::item::builder("Delete its value and its rule").description(line).control(widget::button::destructive("Delete…").on_press_maybe(named.then_some(Message::Secrets(Msg::Deleting(true)))))
            };
            page = page.push(settings::section().title("Delete").add(delete));
        }
        page.into()
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

/// A row's way of being the one the page is about: said, if it is; a button, if it is not.
fn choose(chosen: bool, which: Option<Which>) -> Element<'static, Message> {
    if chosen {
        widget::text::caption("Chosen").into()
    } else {
        widget::button::standard("Choose").on_press(Message::Secrets(Msg::Choose(which))).into()
    }
}

/// A field that shows dots for what is typed in it, with no way to show it (no eye to press), and nothing to
/// copy or cut from (the toolkit's own rule for such a field).
fn masked(value: &str, to: fn(Masked) -> Msg) -> Element<'_, Message> {
    widget::secure_input("", value, None, true).on_input(move |v| Message::Secrets(to(Masked(v)))).width(Length::Fixed(280.0)).into()
}
