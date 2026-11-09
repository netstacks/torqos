//! The window: each page torqcli's output, laid out as COSMIC lays out settings, and each button a torqcli
//! command, run by torqcli's own session (`torq::app::Console`). What asks a person (a commit, an undo, a
//! password) goes to a terminal: a password is never typed in a window TorqOS draws (ADR-045 5).

use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget::{self, icon, nav_bar, settings};
use std::collections::HashMap;
use torq::app::{self, Action, At, Choice, Console, Glance, How, Machine, Mode, Needs, Proposal, Typed, View, VIEWS};
use torq::history::Row;

/// How many changes History shows.
const HISTORY: usize = 200;
/// The most settings listed at one place: the filter narrows the rest.
const LISTED: usize = 200;

#[derive(Clone, Copy)]
enum Page {
    Overview,
    Configuration,
    Changes,
    History,
    Proposals,
    View(&'static View),
    Commands,
}

impl PartialEq for Page {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Page::View(a), Page::View(b)) => a.id == b.id,
            (a, b) => std::mem::discriminant(a) == std::mem::discriminant(b),
        }
    }
}

impl Page {
    /// The command a person would type for what the page shows.
    fn command(self) -> Option<&'static str> {
        match self {
            Page::Overview => Some("show system alarms"),
            Page::History => Some("show system ledger"),
            Page::Proposals => Some("show system proposals"),
            Page::Changes => Some("configure; show | compare"),
            _ => None,
        }
    }
}


/// What a page has to show: not yet asked, the answer, or why there is none.
enum Loaded<T> {
    Waiting,
    Got(T),
    Failed(String),
}

impl<T> From<Result<T, String>> for Loaded<T> {
    fn from(r: Result<T, String>) -> Self {
        match r {
            Ok(v) => Loaded::Got(v),
            Err(e) => Loaded::Failed(e),
        }
    }
}

pub struct Settings {
    core: cosmic::Core,
    nav: nav_bar::Model,
    console: Console,
    glance: Loaded<Glance>,
    machine: Loaded<Machine>,
    history: Loaded<Vec<Row>>,
    proposals: Loaded<Vec<Proposal>>,
    /// Each command's output, by the command.
    outputs: HashMap<String, Loaded<String>>,
    /// What the last action did: its words, and whether it failed.
    note: Option<(String, bool)>,
    /// What is running now.
    busy: Option<String>,
    // Configuration:
    path: Vec<String>,
    options: Loaded<Vec<Choice>>,
    filter: String,
    entry: String,
    search: String,
    // Changes:
    comment: String,
    confirmed: bool,
    minutes: String,
    at: String,
    // Commands:
    line: String,
    typed_cf: bool,
    edit: Vec<String>,
    offers: Vec<Choice>,
    asked: u64,
    log: Vec<(String, String)>,
    /// Ask to restart once the terminal has confirmed the change (`Confirm, then restart`).
    restart_after: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Glance(Result<Glance, String>),
    Machine(Result<Machine, String>),
    History(Result<Vec<Row>, String>),
    Proposals(Result<Vec<Proposal>, String>),
    Output(String, Result<String, String>),
    /// Do this command, here or in a terminal.
    Act(At, String),
    Did(String, Result<String, String>),
    Closed(String, Result<bool, String>),
    Confirm,
    Restart,
    LogOut,
    Go(Page2),
    Fill(How, String),
    Filled(How, String, Typed),
    // Configuration:
    To(Vec<String>),
    Pick(Vec<String>, String),
    Picked(Vec<String>, String, Vec<Choice>),
    Options(Vec<String>, Vec<Choice>),
    Filter(String),
    Entry(String),
    SetEntry,
    Remove,
    Search(String),
    Find,
    // Changes:
    Comment(String),
    Confirmed(bool),
    Minutes(String),
    AtTime(String),
    Commit,
    // Commands:
    Line(String),
    Offers(u64, Vec<Choice>),
    Offer(String),
    RunLine,
    Typed(String, Typed),
}

/// A page, as a message carries it.
#[derive(Debug, Clone, Copy)]
pub enum Page2 {
    Changes,
    Proposals,
}

impl cosmic::Application for Settings {
    type Executor = cosmic::executor::Default;
    type Flags = Option<String>;
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.Settings";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, start: Option<String>) -> (Self, Task<cosmic::Action<Message>>) {
        let mut nav = nav_bar::Model::default();
        let start = start.unwrap_or_default();
        let mut add = |name: &str, id: &str, page: Page, glyph: &'static str| {
            let item = nav.insert().text(name.to_string()).data(page).icon(icon::from_name(glyph));
            if id == start || (start.is_empty() && id == "overview") {
                item.activate();
            }
        };
        add("Overview", "overview", Page::Overview, "emblem-ok-symbolic");
        add("Configuration", "configuration", Page::Configuration, "preferences-system-symbolic");
        add("Changes", "changes", Page::Changes, "edit-symbolic");
        add("History", "history", Page::History, "document-open-recent-symbolic");
        add("Proposals", "proposals", Page::Proposals, "mail-message-new-symbolic");
        for v in VIEWS {
            add(v.name, v.id, Page::View(v), v.icon);
        }
        add("Commands", "commands", Page::Commands, "utilities-terminal-symbolic");
        let mut app = Settings {
            core,
            nav,
            console: Console::start(),
            glance: Loaded::Waiting,
            machine: Loaded::Waiting,
            history: Loaded::Waiting,
            proposals: Loaded::Waiting,
            outputs: HashMap::new(),
            note: None,
            busy: None,
            path: vec![],
            options: Loaded::Waiting,
            filter: String::new(),
            entry: String::new(),
            search: String::new(),
            comment: String::new(),
            confirmed: false,
            minutes: "10".into(),
            at: String::new(),
            line: String::new(),
            typed_cf: false,
            edit: vec![],
            offers: vec![],
            asked: 0,
            log: vec![],
            restart_after: false,
        };
        if app.nav.active_data::<Page>().is_none() {
            let first = app.nav.iter().next();
            if let Some(first) = first {
                app.nav.activate(first);
            }
        }
        let title = match app.core.main_window_id() {
            Some(id) => app.set_window_title("TorqOS Settings".into(), id),
            None => Task::none(),
        };
        let load = app.update(Message::Refresh);
        (app, Task::batch([title, load]))
    }

    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav)
    }

    /// A page chosen in the navigation: libcosmic leaves switching to it to the application.
    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        self.nav.activate(id);
        self.note = None;
        self.load()
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        vec![widget::button::icon(icon::from_name("view-refresh-symbolic")).tooltip("Ask the broker again").on_press(Message::Refresh).into()]
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Refresh => return self.load(),
            Message::Glance(r) => self.glance = r.into(),
            Message::Machine(r) => self.machine = r.into(),
            Message::History(r) => self.history = r.into(),
            Message::Proposals(r) => self.proposals = r.into(),
            Message::Output(c, r) => {
                // Nothing left to commit: what was typed for the last commit must not name the next.
                if c == "show | compare" && matches!(&r, Ok(t) if t.starts_with("No changes")) {
                    self.comment.clear();
                    self.at.clear();
                    self.confirmed = false;
                }
                self.outputs.insert(c, r.into());
            }
            Message::Act(at, line) => return self.act(at, line),
            Message::Did(what, r) => {
                self.busy = None;
                self.note = Some(match r {
                    Ok(out) => (if out.trim().is_empty() { format!("Done: {what}") } else { out.trim().to_string() }, false),
                    Err(e) => (e, true),
                });
                return self.load();
            }
            Message::Closed(what, r) => {
                self.busy = None;
                let restart = std::mem::take(&mut self.restart_after);
                self.note = Some(match r {
                    Ok(true) => (format!("Done: {what}"), false),
                    Ok(false) => (format!("`{what}` did not finish: the terminal said why."), true),
                    Err(e) => (e, true),
                });
                if restart && self.note.as_ref().is_some_and(|(_, failed)| !failed) {
                    app::ask_restart();
                }
                return self.load();
            }
            // A private candidate, always: `commit` of an empty one is what confirms, and the draft is not part of it.
            Message::Confirm => {
                self.restart_after = matches!(&self.glance, Loaded::Got(g) if matches!(g.needs, Needs::Restart(_)));
                return self.terminal(app::CONFIRM.to_string(), false, "commit".to_string());
            }
            Message::Restart => app::ask_restart(),
            Message::LogOut => app::ask_log_out(),
            Message::Go(p) => {
                let want = match p {
                    Page2::Changes => Page::Changes,
                    Page2::Proposals => Page::Proposals,
                };
                let id = self.nav.iter().find(|&i| self.nav.data::<Page>(i).is_some_and(|p| *p == want));
                if let Some(id) = id {
                    self.nav.activate(id);
                }
                return self.load();
            }
            Message::Fill(how, command) => {
                // The Commands page's session, in the mode the command is of.
                let console = self.console.clone();
                let step = match (how == How::FillCf, self.typed_cf) {
                    (true, false) => "configure",
                    (true, true) => "top",
                    (false, true) => "exit configuration-mode",
                    (false, false) => "",
                };
                return cosmic::task::future(async move {
                    let t = off(move || console.typed(step)).await;
                    Message::Filled(how, command, t.unwrap_or_else(|e| Typed { output: Err(e), configuration: false, edit: vec![] }))
                });
            }
            Message::Filled(_how, command, t) => {
                self.typed_cf = t.configuration;
                self.edit = t.edit;
                self.line = command;
                let id = self.nav.iter().find(|&i| self.nav.data::<Page>(i).is_some_and(|p| *p == Page::Commands));
                if let Some(id) = id {
                    self.nav.activate(id);
                }
                return self.offer();
            }

            Message::To(path) => {
                self.path = path;
                self.filter.clear();
                self.entry.clear();
                return self.load();
            }
            Message::Pick(path, word) => {
                // A word that ends the line is a value to set; any other is a place to go down to.
                let console = self.console.clone();
                let mut words: Vec<&str> = vec!["set"];
                words.extend(path.iter().map(String::as_str));
                words.push(&word);
                let before = format!("{} ", app::line(&words));
                return cosmic::task::future(async move {
                    let got = off(move || console.choices(At::Draft, &before)).await.unwrap_or_default();
                    Message::Picked(path, word, got)
                });
            }
            Message::Picked(path, word, next) => {
                // The person has moved on: this word was for a place they have left.
                if path != self.path {
                    return Task::none();
                }
                if next.is_empty() {
                    self.note = Some((format!("Could not read what may follow `{word}`."), true));
                    return Task::none();
                }
                // `<[Enter]>` alone: the line is whole. With other words (`enable`: true, false) there is a choice to show.
                if next.iter().all(|c| c.word == "<[Enter]>") {
                    let line = self.at_path("set", Some(&word));
                    return self.act(At::Draft, line);
                }
                self.path.push(word);
                self.filter.clear();
                self.entry.clear();
                return self.load();
            }
            Message::Options(path, got) => {
                if path == self.path {
                    self.options = Loaded::Got(got);
                }
            }
            Message::Filter(s) => self.filter = s,
            Message::Entry(s) => self.entry = s,
            Message::SetEntry => {
                let value = self.entry.trim().to_string();
                if !value.is_empty() {
                    self.entry.clear();
                    let line = self.at_path("set", Some(&value));
                    return self.act(At::Draft, line);
                }
            }
            Message::Remove => {
                let line = self.at_path("delete", None);
                return self.act(At::Draft, line);
            }
            Message::Search(s) => self.search = s,
            Message::Find => {
                let terms = self.search.trim().to_string();
                if !terms.is_empty() {
                    return self.read(At::Read, format!("help {terms}"));
                }
            }

            Message::Comment(s) => self.comment = s,
            Message::Confirmed(b) => self.confirmed = b,
            Message::Minutes(s) => self.minutes = s,
            Message::AtTime(s) => self.at = s,
            Message::Commit => {
                let mut line = "commit".to_string();
                if !self.comment.trim().is_empty() {
                    line.push_str(&format!(" comment {}", app::line(&[self.comment.trim()])));
                }
                if self.confirmed {
                    line.push_str(&format!(" confirmed {}", self.minutes.trim()));
                }
                if !self.at.trim().is_empty() {
                    line.push_str(&format!(" at {}", app::line(&[self.at.trim()])));
                }
                return self.act(At::Draft, line);
            }

            Message::Line(s) => {
                self.line = s;
                return self.offer();
            }
            Message::Offers(n, got) => {
                if n == self.asked {
                    self.offers = got;
                }
            }
            Message::Offer(word) => {
                let keep = if self.line.ends_with(char::is_whitespace) || self.line.is_empty() { self.line.len() } else { self.line.rfind(char::is_whitespace).map_or(0, |i| i + 1) };
                self.line.truncate(keep);
                self.line.push_str(&word);
                self.line.push(' ');
                return self.offer();
            }
            Message::RunLine => {
                let line = self.line.trim().to_string();
                if line.is_empty() {
                    return Task::none();
                }
                let mode = if self.typed_cf { Mode::Configuration } else { Mode::Operational };
                if app::needs_terminal(mode, &line) {
                    self.line.clear();
                    let (script, draft) = app::terminal_script(mode, &self.edit, &line);
                    return self.terminal(script, draft, line);
                }
                self.busy = Some(line.clone());
                let console = self.console.clone();
                return cosmic::task::future(async move {
                    let shown = line.clone();
                    let t = off(move || console.typed(&line)).await;
                    Message::Typed(shown, t.unwrap_or_else(|e| Typed { output: Err(e), configuration: false, edit: vec![] }))
                });
            }
            Message::Typed(line, t) => {
                self.busy = None;
                self.typed_cf = t.configuration;
                self.edit = t.edit;
                self.log.insert(0, (line, t.output.unwrap_or_else(|e| format!("error: {e}"))));
                self.log.truncate(50);
                self.line.clear();
                return self.offer();
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let page = self.page();
        let spacing = cosmic::theme::spacing();
        let body = match page {
            Page::Overview => self.overview(),
            Page::Configuration => self.configuration(),
            Page::Changes => self.changes(),
            Page::History => self.history(),
            Page::Proposals => self.proposals(),
            Page::View(v) => self.command_view(v),
            Page::Commands => self.commands(),
        };
        let mut content = widget::column::with_capacity(4).spacing(spacing.space_m).max_width(920);
        if let Some(what) = &self.busy {
            content = content.push(widget::text::body(format!("Working: {what}…")));
        }
        if let Some((text, failed)) = &self.note {
            content = content.push(note(text, *failed));
        }
        content = content.push(body);
        if let Some(c) = page.command() {
            content = content.push(widget::text::caption(format!("The same as: torqcli {c}")));
        }
        widget::scrollable(widget::container(content).width(Length::Fill).center_x(Length::Fill).padding(spacing.space_m)).into()
    }
}

impl Settings {
    fn page(&self) -> Page {
        self.nav.active_data::<Page>().copied().unwrap_or(Page::Overview)
    }

    /// Ask again for what this page shows (and the state every page's actions depend on).
    fn load(&mut self) -> Task<cosmic::Action<Message>> {
        let console = self.console.clone();
        let mut tasks = vec![cosmic::task::future(async move {
            Message::Glance(blocking(move || app::glance(app::session_recorded().as_deref(), console.uncommitted())).await)
        })];
        match self.page() {
            Page::Overview => tasks.push(cosmic::task::future(async { Message::Machine(blocking(app::machine).await) })),
            Page::History => tasks.push(cosmic::task::future(async { Message::History(blocking(|| app::history(HISTORY)).await) })),
            Page::Proposals => {
                tasks.push(cosmic::task::future(async { Message::Proposals(blocking(app::proposals).await) }));
                tasks.push(self.read(At::Read, "show system proposals".into()));
            }
            Page::Changes => {
                tasks.push(self.read(At::Draft, "show | compare".into()));
                tasks.push(self.read(At::Read, "show system commit".into()));
            }
            Page::View(v) => tasks.extend(v.sections.iter().map(|(_, c)| self.read(At::Read, c.to_string()))),
            Page::Configuration => {
                let console = self.console.clone();
                let path = self.path.clone();
                let before = format!("{} ", self.at_path("set", None));
                tasks.push(cosmic::task::future(async move {
                    let got = off(move || console.choices(At::Draft, &before)).await.unwrap_or_default();
                    Message::Options(path, got)
                }));
                if !self.path.is_empty() {
                    tasks.push(self.read(At::Draft, self.at_path("show", None)));
                }
            }
            Page::Commands => tasks.push(self.offer()),
        }
        Task::batch(tasks)
    }

    fn read(&self, at: At, command: String) -> Task<cosmic::Action<Message>> {
        let console = self.console.clone();
        cosmic::task::future(async move {
            let c = command.clone();
            let got = off(move || console.run(at, &c)).await.and_then(|r| r);
            Message::Output(command, got)
        })
    }

    /// `verb` at the configuration's place, with one more word.
    fn at_path(&self, verb: &str, last: Option<&str>) -> String {
        let mut w: Vec<&str> = vec![verb];
        w.extend(self.path.iter().map(String::as_str));
        w.extend(last);
        app::line(&w)
    }

    /// Run a command: in this window, or, if it asks a person, in a terminal.
    fn act(&mut self, at: At, line: String) -> Task<cosmic::Action<Message>> {
        // Slow, and only loads the draft: with the draft's session, so that the pages' reads go on.
        let (at, line) = if at == At::Read && app::loads_draft(&line) { (At::Draft, format!("run {line}")) } else { (at, line) };
        let mode = if at == At::Read { Mode::Operational } else { Mode::Configuration };
        if app::needs_terminal(mode, &line) {
            let (script, draft) = app::terminal_script(mode, &[], &line);
            return self.terminal(script, draft, line);
        }
        self.busy = Some(line.clone());
        self.note = None;
        let console = self.console.clone();
        cosmic::task::future(async move {
            let c = line.clone();
            let got = off(move || console.run(at, &c)).await.and_then(|r| r);
            Message::Did(line, got)
        })
    }

    fn terminal(&mut self, script: String, draft: bool, what: String) -> Task<cosmic::Action<Message>> {
        self.busy = Some(format!("{what} (in the terminal)"));
        self.note = None;
        cosmic::task::future(async move {
            let got = off(move || app::in_terminal(&script, draft)).await.and_then(|r| r);
            Message::Closed(what, got)
        })
    }

    /// What may follow the Commands page's line.
    fn offer(&mut self) -> Task<cosmic::Action<Message>> {
        self.asked += 1;
        let n = self.asked;
        let console = self.console.clone();
        let before = self.line.clone();
        cosmic::task::future(async move {
            let got = off(move || console.choices(At::Typed, &before)).await.unwrap_or_default();
            Message::Offers(n, got)
        })
    }

    fn output(&self, command: &str) -> Element<'_, Message> {
        match self.outputs.get(command) {
            None | Some(Loaded::Waiting) => waiting(),
            Some(Loaded::Failed(why)) => failed(why),
            Some(Loaded::Got(text)) => widget::text::monotext(text.trim_end().to_string()).width(Length::Fill).into(),
        }
    }

    // ── The pages ────────────────────────────────────────────────────────────────────────────────

    fn overview(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let g = match &self.glance {
            Loaded::Waiting => return waiting(),
            Loaded::Failed(why) => return failed(why),
            Loaded::Got(g) => g,
        };
        let (mark, colour, headline) = if g.well {
            ("emblem-ok-symbolic", Colour::Good, "This machine is exactly as its history says")
        } else {
            ("dialog-warning-symbolic", Colour::Warn, "This machine needs a look")
        };
        let banner = widget::row::with_capacity(2)
            .push(colour.icon(mark, 32))
            .push(widget::column::with_capacity(2).push(widget::text::title3(headline)).push(widget::text::body(format!("TorqOS {}", g.version))))
            .spacing(spacing.space_s)
            .align_y(Alignment::Center);
        let mut page = widget::column::with_capacity(3).push(banner).spacing(spacing.space_l);

        let mut waiting_on = settings::section().title("Waiting for you");
        let mut any = false;
        if let Some(c) = &g.confirm {
            any = true;
            let label = if matches!(g.needs, Needs::Restart(_)) { "Confirm, then restart" } else { "Confirm" };
            waiting_on = waiting_on.add(attention(format!("Change #{} rolls back at {} unless you confirm it.", c.seq, c.deadline), widget::button::suggested(label).on_press(Message::Confirm)));
        }
        match (&g.needs, g.needs.line()) {
            (Needs::Restart(_), Some(l)) => {
                any = true;
                waiting_on = waiting_on.add(attention(l, widget::button::standard("Restart…").on_press(Message::Restart)));
            }
            (Needs::LogOut(_), Some(l)) => {
                any = true;
                waiting_on = waiting_on.add(attention(l, widget::button::standard("Log out…").on_press(Message::LogOut)));
            }
            _ => {}
        }
        if g.draft {
            any = true;
            waiting_on = waiting_on.add(attention("You have changes not yet committed.".to_string(), widget::button::standard("Review").on_press(Message::Go(Page2::Changes))));
        }
        if g.waiting.0 > 0 {
            any = true;
            let mut s = if g.waiting.0 == 1 { "A proposal is waiting for you.".to_string() } else { format!("{} proposals are waiting for you.", g.waiting.0) };
            if g.waiting.1 > 0 {
                s.push_str(&format!(" {} came from a session that read untrusted content.", g.waiting.1));
            }
            waiting_on = waiting_on.add(attention(s, widget::button::standard("Review").on_press(Message::Go(Page2::Proposals))));
        }
        if any {
            page = page.push(waiting_on);
        }

        // `show system alarms`, line by line, as it prints them.
        let mut alarms = settings::section().title("Alarms");
        match &self.machine {
            Loaded::Got(m) => {
                let lines = m.alarms.lines().map(str::trim).filter(|l| !l.is_empty() && !(m.well && l.trim_start_matches("✓ ").trim_end_matches('.') == headline));
                for l in lines {
                    alarms = alarms.add(settings::item_row(vec![widget::text::body(l.to_string()).width(Length::Fill).into()]));
                }
            }
            Loaded::Failed(why) => alarms = alarms.add(failed(why)),
            Loaded::Waiting => alarms = alarms.add(waiting()),
        }
        page.push(alarms).into()
    }

    fn configuration(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(6).spacing(spacing.space_m);

        // Search every setting and command, as `help <word>` does.
        page = page.push(
            widget::row::with_capacity(2)
                .push(widget::search_input("Search settings and commands", &self.search).on_input(Message::Search).on_submit(|_| Message::Find).width(Length::Fill))
                .push(widget::button::standard("Search").on_press(Message::Find))
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center),
        );
        let terms = self.search.trim();
        if !terms.is_empty() {
            let c = format!("help {terms}");
            if self.outputs.contains_key(&c) {
                page = page.push(self.output(&c));
            }
        }

        // Where we are: each place a way back to it.
        let mut crumbs = widget::row::with_capacity(self.path.len() + 1).spacing(spacing.space_xxs).align_y(Alignment::Center);
        crumbs = crumbs.push(widget::button::text("configuration").on_press(Message::To(vec![])));
        for (i, w) in self.path.iter().enumerate() {
            crumbs = crumbs.push(widget::text::body("›"));
            crumbs = crumbs.push(widget::button::text(w.clone()).on_press(Message::To(self.path[..=i].to_vec())));
        }
        page = page.push(crumbs);

        if !self.path.is_empty() {
            let shown = self.at_path("show", None);
            let mut here = settings::section().title("In your draft");
            here = here.add(settings::item_row(vec![self.output(&shown), widget::button::destructive("Delete").on_press(Message::Remove).into()]).spacing(spacing.space_s));
            page = page.push(here);
        }

        let options = match &self.options {
            Loaded::Waiting => return page.push(waiting()).into(),
            Loaded::Failed(why) => return page.push(failed(why)).into(),
            Loaded::Got(o) => o,
        };
        // A value is typed; a word is chosen. What the command line offers is what is offered here.
        for c in options.iter().filter(|c| c.is_placeholder() && c.word != "<[Enter]>") {
            let entry = widget::text_input(c.word.clone(), &self.entry).on_input(Message::Entry).on_submit(|_| Message::SetEntry).width(Length::Fill);
            page = page.push(
                settings::section().title("Set a value").add(
                    settings::item::builder(c.help.clone()).control(
                        widget::row::with_capacity(2).push(entry).push(widget::button::suggested("Set").on_press(Message::SetEntry)).spacing(spacing.space_xs).align_y(Alignment::Center).width(Length::FillPortion(2)),
                    ),
                ),
            );
        }
        let wanted = self.filter.to_lowercase();
        let words: Vec<&Choice> = options.iter().filter(|c| !c.is_placeholder() && c.word != "|" && (wanted.is_empty() || c.word.to_lowercase().contains(&wanted) || c.help.to_lowercase().contains(&wanted))).collect();
        if !words.is_empty() || !wanted.is_empty() {
            page = page.push(widget::search_input("Filter this list", &self.filter).on_input(Message::Filter).on_clear(Message::Filter(String::new())));
        }
        if !words.is_empty() {
            let mut list = settings::section();
            for c in words.iter().take(LISTED) {
                let row = settings::item_row(vec![
                    widget::column::with_capacity(2)
                        .push(widget::text::body(c.word.clone()))
                        .push(widget::text::caption(c.help.clone()))
                        .width(Length::Fill)
                        .into(),
                    icon::from_name("go-next-symbolic").size(16).icon().into(),
                ])
                .spacing(spacing.space_s);
                list = list.add(widget::mouse_area(row).on_press(Message::Pick(self.path.clone(), c.word.clone())));
            }
            page = page.push(list);
            if words.len() > LISTED {
                page = page.push(widget::text::caption(format!("The first {LISTED} of {}: filter to narrow them.", words.len())));
            }
        }
        page.into()
    }

    fn changes(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let compare = "show | compare";
        let unchanged = matches!(self.outputs.get(compare), Some(Loaded::Got(t)) if t.starts_with("No changes"));
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        page = page.push(settings::section().title("Your draft, against the configuration in effect").add(settings::item_row(vec![self.output(compare)])));

        if !unchanged {
            let mut commit = settings::section().title("Commit");
            commit = commit.add(settings::item::builder("Comment").description("What the change is for: it goes in the history").control(widget::text_input("Optional", &self.comment).on_input(Message::Comment).width(Length::FillPortion(2))));
            commit = commit.add(settings::item::builder("Roll back unless confirmed").description("If the change cuts you off, the machine undoes it by itself").control(widget::toggler(self.confirmed).on_toggle(Message::Confirmed)));
            if self.confirmed {
                commit = commit.add(settings::item::builder("Minutes to confirm in").control(widget::text_input("10", &self.minutes).on_input(Message::Minutes).width(Length::Fixed(80.0))));
            }
            commit = commit.add(settings::item::builder("Apply at a later time").description("HH:MM, or \"YYYY-MM-DD HH:MM\"; approved now, applied then").control(widget::text_input("Now", &self.at).on_input(Message::AtTime).width(Length::FillPortion(2))));
            page = page.push(commit);
            page = page.push(
                widget::row::with_capacity(3)
                    .push(widget::button::suggested("Commit…").on_press(Message::Commit))
                    .push(widget::button::standard("Check").on_press(Message::Act(At::Draft, "commit check".into())))
                    .push(widget::button::destructive("Discard").on_press(Message::Act(At::Draft, "rollback 0".into())))
                    .spacing(spacing.space_xs),
            );
            page = page.push(widget::text::caption("Commit opens a terminal, where anything it needs from you is asked."));
        }

        let mut earlier = settings::section().title("Earlier configurations");
        match self.outputs.get("show system commit") {
            Some(Loaded::Got(t)) => {
                for (n, text) in app::configurations(t).into_iter().skip(1).take(30) {
                    earlier = earlier.add(
                        settings::item_row(vec![
                            widget::text::body(text).width(Length::Fill).into(),
                            widget::button::standard("Load into draft").on_press(Message::Act(At::Draft, format!("rollback {n}"))).into(),
                        ])
                        .spacing(spacing.space_s),
                    );
                }
            }
            Some(Loaded::Failed(why)) => earlier = earlier.add(failed(why)),
            _ => earlier = earlier.add(waiting()),
        }
        page.push(earlier).into()
    }

    fn history(&self) -> Element<'_, Message> {
        let rows = match &self.history {
            Loaded::Waiting => return waiting(),
            Loaded::Failed(why) => return failed(why),
            Loaded::Got(rows) => rows,
        };
        if rows.is_empty() {
            return widget::text::body("Nothing has changed yet.").into();
        }
        let spacing = cosmic::theme::spacing();
        // Newest first, a section per day; each change as `show system ledger` says it.
        let mut days: Vec<(String, Vec<&Row>)> = vec![];
        for r in rows.iter().rev() {
            match days.last_mut() {
                Some((day, list)) if *day == r.day => list.push(r),
                _ => days.push((r.day.clone(), vec![r])),
            }
        }
        let mut page = widget::column::with_capacity(days.len()).spacing(spacing.space_l);
        for (day, list) in days {
            let mut section = settings::section().title(day);
            for r in list {
                section = section.add(change(r));
            }
            page = page.push(section);
        }
        page.into()
    }

    fn proposals(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        match &self.proposals {
            Loaded::Waiting => page = page.push(waiting()),
            Loaded::Failed(why) => page = page.push(failed(why)),
            Loaded::Got(list) if list.is_empty() => page = page.push(widget::text::body("No proposals are waiting.")),
            Loaded::Got(list) => {
                let mut section = settings::section().title("Waiting for you");
                for p in list {
                    let mut who = format!("Proposed by {}", p.by);
                    if p.tainted {
                        who.push_str(" · ⚠ tainted: made after reading untrusted content");
                    }
                    section = section.add(
                        settings::item_row(vec![
                            widget::column::with_capacity(2).push(widget::text::body(format!("#{}  {}", p.seq, p.reason))).push(widget::text::caption(who)).width(Length::Fill).into(),
                            widget::button::suggested("Review and apply…").on_press(Message::Act(At::Read, format!("request system apply {}", p.seq))).into(),
                        ])
                        .spacing(spacing.space_s),
                    );
                }
                page = page.push(section);
                page = page.push(settings::section().title("In detail").add(settings::item_row(vec![self.output("show system proposals")])));
            }
        }
        page.push(actions(app::AGENT)).into()
    }

    fn command_view(&self, v: &'static View) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(v.sections.len() + 1).spacing(spacing.space_l);
        page = page.push(actions(v.actions));
        for (title, command) in v.sections {
            page = page.push(
                widget::column::with_capacity(2)
                    .push(settings::section().title(*title).add(settings::item_row(vec![self.output(command)])))
                    .push(widget::text::caption(format!("The same as: torqcli {command}")))
                    .spacing(spacing.space_xxs),
            );
        }
        page.into()
    }

    fn commands(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let prompt = match (self.typed_cf, self.edit.is_empty()) {
            (false, _) => "torqcli>".to_string(),
            (true, true) => "torqcli#".to_string(),
            (true, false) => format!("[edit {}] torqcli#", self.edit.join(" ")),
        };
        let mut page = widget::column::with_capacity(4).spacing(spacing.space_m);
        page = page.push(widget::text::body("Anything torqcli can do. Choose what comes next, or type it."));
        page = page.push(
            widget::row::with_capacity(3)
                .push(widget::text::monotext(prompt))
                .push(widget::text_input("show system alarms", &self.line).on_input(Message::Line).on_submit(|_| Message::RunLine).width(Length::Fill))
                .push(widget::button::suggested("Run").on_press(Message::RunLine))
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center),
        );
        if !self.offers.is_empty() {
            let mut list = settings::section().title("What may come next");
            for c in self.offers.iter().take(LISTED) {
                let row = settings::item_row(vec![
                    widget::text::monotext(c.word.clone()).width(Length::FillPortion(1)).into(),
                    widget::text::caption(c.help.clone()).width(Length::FillPortion(3)).into(),
                ])
                .spacing(spacing.space_s);
                list = list.add(if c.is_placeholder() { Element::from(row) } else { widget::mouse_area(row).on_press(Message::Offer(c.word.clone())).into() });
            }
            page = page.push(list);
        }
        for (line, out) in &self.log {
            page = page.push(
                widget::column::with_capacity(2)
                    .push(widget::text::heading(line.clone()))
                    .push(widget::text::monotext(out.trim_end().to_string()).width(Length::Fill))
                    .spacing(spacing.space_xxs),
            );
        }
        page.into()
    }
}

/// A page's buttons: each a torqcli command, run or finished on the Commands page.
fn actions(list: &'static [Action]) -> Element<'static, Message> {
    let mut section = settings::section().title("Actions");
    for a in list {
        let (button, sub) = match a.how {
            How::Run => (widget::button::standard(a.label).on_press(Message::Act(At::Read, a.command.to_string())), a.command.to_string()),
            how => (widget::button::standard(a.label).on_press(Message::Fill(how, a.command.to_string())), format!("{}…", a.command.trim_end())),
        };
        section = section.add(settings::item_row(vec![widget::text::caption(sub).width(Length::Fill).into(), button.into()]).spacing(cosmic::theme::spacing().space_s));
    }
    section.into()
}

fn attention<'a>(text: String, button: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    settings::item_row(vec![widget::text::body(text).width(Length::Fill).into(), button.into()]).spacing(cosmic::theme::spacing().space_s).into()
}

/// One change: its mark, what happened, who and when, its number, and undoing it where it can be.
fn change(r: &Row) -> Element<'_, Message> {
    let (name, colour) = match r.symbol {
        "✓" => ("emblem-ok-symbolic", Colour::Good),
        "✗" => ("dialog-error-symbolic", Colour::Bad),
        "⚠" => ("dialog-warning-symbolic", Colour::Warn),
        "↶" => ("edit-undo-symbolic", Colour::Plain),
        "↺" => ("view-refresh-symbolic", Colour::Plain),
        _ => ("media-record-symbolic", Colour::Plain),
    };
    let mut detail = r.time.clone();
    if !r.who.is_empty() {
        detail.push_str(&format!(" · {}", r.who));
    }
    if r.tainted {
        detail.push_str(" · ⚠ tainted: made after reading untrusted content");
    }
    let mut row = vec![
        colour.icon(name, 16),
        widget::column::with_capacity(2).push(widget::text::body(r.text.clone())).push(widget::text::caption(detail)).width(Length::Fill).into(),
        widget::text::body(format!("#{}", r.seq)).into(),
    ];
    if r.undoable {
        row.push(widget::button::standard("Undo…").on_press(Message::Act(At::Read, format!("request system revert {}", r.seq))).into());
    }
    settings::item_row(row).spacing(cosmic::theme::spacing().space_s).align_y(Alignment::Center).into()
}

/// How a mark is coloured: the theme's own success, warning and destructive colours.
enum Colour {
    Good,
    Warn,
    Bad,
    Plain,
}

impl Colour {
    fn icon(self, name: &'static str, size: u16) -> Element<'static, Message> {
        let i = icon::from_name(name).size(size).icon();
        let style = match self {
            Colour::Good => cosmic::theme::Svg::custom(|t| cosmic::iced::widget::svg::Style { color: Some(t.cosmic().success_color().into()) }),
            Colour::Warn => cosmic::theme::Svg::custom(|t| cosmic::iced::widget::svg::Style { color: Some(t.cosmic().warning_color().into()) }),
            Colour::Bad => cosmic::theme::Svg::custom(|t| cosmic::iced::widget::svg::Style { color: Some(t.cosmic().destructive_color().into()) }),
            Colour::Plain => return i.into(),
        };
        i.class(style).into()
    }
}

fn note(text: &str, is_failure: bool) -> Element<'static, Message> {
    let spacing = cosmic::theme::spacing();
    let colour = if is_failure { Colour::Bad } else { Colour::Good };
    let name = if is_failure { "dialog-error-symbolic" } else { "emblem-ok-symbolic" };
    widget::row::with_capacity(2)
        .push(colour.icon(name, 16))
        .push(widget::text::body(text.to_string()).width(Length::Fill))
        .spacing(spacing.space_xs)
        .into()
}

fn waiting() -> Element<'static, Message> {
    widget::text::body("Asking…").into()
}

fn failed(why: &str) -> Element<'_, Message> {
    widget::column::with_capacity(2)
        .push(widget::text::title4("That did not work"))
        .push(widget::text::body(why))
        .spacing(cosmic::theme::spacing().space_xs)
        .into()
}

/// A call that blocks (a socket, a session) on a thread of its own, so the window keeps drawing.
async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.map_err(|_| "the call stopped before it answered".to_string())
}

/// `off`, for a call of the library's that can fail: its error as torqcli would say it.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    off(move || f().map_err(|e| app::said(&e))).await.and_then(|r| r)
}
