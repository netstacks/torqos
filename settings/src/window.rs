//! The window: each page torqcli's output, laid out as COSMIC lays out settings, and each button a torqcli
//! command, run by torqcli's own session (`torq::app::Console`). A yes or no is asked here, an approval in
//! COSMIC's own dialog (ADR-045 5): a password is never typed in a window TorqOS draws.

use cosmic::app::context_drawer;
use cosmic::iced::{Alignment, Length, Padding};
use cosmic::prelude::*;
use cosmic::widget::segmented_button::{Entity, SingleSelectModel};
use cosmic::widget::{self, icon, nav_bar, settings};
use cosmic::iced::futures::channel::mpsc::Sender;
use cosmic::iced::futures::SinkExt as _;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use torq::app::{self, Action, At, Choice, Console, Glance, How, Machine, Mode, Needs, Proposal, Typed, View, VIEWS};
use torq::history::Row;

/// How many changes History shows.
const HISTORY: usize = 200;
/// The most rows a list or the tree shows: filtering narrows the rest.
const LISTED: usize = 400;
/// The most lines the diff viewer draws.
const DIFF_LINES: usize = 2000;

const DRAFT: &str = "show | display set";
const ACTIVE: &str = "show configuration | display set";
const COMPARE: &str = "show | compare";
const COMMITS: &str = "show system commit";

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Configuration,
    History,
    Proposals,
    System,
    Commands,
}

/// Configuration's tabs.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Settings,
    Changes,
    Earlier,
}

/// What a diff page shows.
#[derive(Clone, Copy, PartialEq)]
enum DiffTab {
    Plain,
    Files,
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

/// A place in the configuration.
type Path = Vec<String>;
/// The configuration, each setting's path and its values.
type Values = BTreeMap<Path, Vec<String>>;

/// One row of the configuration tree.
struct TreeRow {
    path: Path,
    branch: bool,
    open: bool,
    value: Option<String>,
    change: Change,
}

/// How a setting in the draft differs from the configuration in effect.
#[derive(Clone, Copy, PartialEq)]
enum Change {
    Same,
    New,
    Changed,
    Removed,
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
    cfg_tabs: SingleSelectModel,
    sys_tabs: SingleSelectModel,
    diff_tabs: SingleSelectModel,
    // The tree:
    draft: Values,
    active: Values,
    /// Every setting NixOS has, as far as anyone has opened it (`Show all settings`).
    known: HashMap<Path, Vec<Choice>>,
    all: bool,
    /// In `all`: what is open. Otherwise: what has been closed (what is set is open at first).
    open: HashSet<Path>,
    closed: HashSet<Path>,
    filter: String,
    search: String,
    // The side panel:
    selected: Option<Path>,
    own: Vec<Choice>,
    help: String,
    entry: String,
    // Changes:
    comment: String,
    confirmed: bool,
    minutes: String,
    at: String,
    /// The earlier configuration being compared with the one in effect.
    comparing: Option<u32>,
    // Detail pages:
    change: Option<u64>,
    proposal: Option<u64>,
    // Commands:
    line: String,
    typed_cf: bool,
    edit: Vec<String>,
    offers: Vec<Choice>,
    asked: u64,
    log: Vec<(String, String)>,
    /// Ask to restart once the terminal has confirmed the change (`Confirm, then restart`).
    restart_after: bool,
    /// What a running command is asking.
    question: Option<(String, Reply)>,
}

/// The way back to a command waiting for a person's yes or no.
#[derive(Clone)]
pub struct Reply(Arc<Mutex<Option<std::sync::mpsc::Sender<bool>>>>);

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reply")
    }
}

impl Reply {
    fn send(&self, yes: bool) {
        if let Some(back) = self.0.lock().ok().and_then(|mut b| b.take()) {
            let _ = back.send(yes);
        }
    }
}

/// Puts a command's question on the window and waits for the answer; on a thread that may wait.
fn asker(ui: Sender<Message>) -> impl Fn(String) -> bool + Send {
    move |question| {
        let mut ui = ui.clone();
        let (back, answer) = std::sync::mpsc::channel();
        let sent = cosmic::iced::futures::executor::block_on(ui.send(Message::Asked(question, Reply(Arc::new(Mutex::new(Some(back)))))));
        sent.is_ok() && answer.recv().unwrap_or(false)
    }
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
    /// A command asks a person: its question, and the way back for the answer.
    Asked(String, Reply),
    Answer(bool),
    ConfirmDone(Result<String, String>),
    Confirm,
    Restart,
    LogOut,
    Review,
    Approve,
    CfgTab(Entity),
    SysTab(Entity),
    DiffTab(Entity),
    Fill(How, String),
    Filled(String, Typed),
    // The tree:
    All(bool),
    Toggle(Path, bool),
    Known(Path, Vec<Choice>),
    Select(Path),
    Selected(Path, Vec<Choice>, Vec<Choice>),
    CloseDrawer,
    Pick(Path, String),
    Picked(Path, String, Vec<Choice>),
    Filter(String),
    Entry(String),
    SetEntry,
    SetBool(bool),
    Search(String),
    Find,
    // Changes:
    Comment(String),
    Confirmed(bool),
    Minutes(String),
    AtTime(String),
    Commit,
    Compare(u32),
    // Detail pages:
    OpenChange(u64),
    OpenProposal(u64),
    Back,
    // Commands:
    Line(String),
    Offers(u64, Vec<Choice>),
    Offer(String),
    RunLine,
    Typed(String, Typed),
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
        let start = start.unwrap_or_default();
        let want_page = match start.as_str() {
            "configuration" | "changes" => Page::Configuration,
            "history" => Page::History,
            "proposals" => Page::Proposals,
            "commands" => Page::Commands,
            s if VIEWS.iter().any(|v| v.id == s) => Page::System,
            _ => Page::Overview,
        };
        let mut nav = nav_bar::Model::default();
        for (name, page, glyph) in [
            ("Overview", Page::Overview, "emblem-ok-symbolic"),
            ("Configuration", Page::Configuration, "preferences-system-symbolic"),
            ("History", Page::History, "document-open-recent-symbolic"),
            ("Proposals", Page::Proposals, "mail-message-new-symbolic"),
            ("System", Page::System, "computer-symbolic"),
            ("Command Line", Page::Commands, "utilities-terminal-symbolic"),
        ] {
            let item = nav.insert().text(name).data(page).icon(icon::from_name(glyph));
            if page == want_page {
                item.activate();
            }
        }
        let mut cfg_tabs = SingleSelectModel::default();
        for (name, tab) in [("Settings", Tab::Settings), ("Your Changes", Tab::Changes), ("Earlier Versions", Tab::Earlier)] {
            let item = cfg_tabs.insert().text(name).data(tab);
            if (tab == Tab::Changes) == (start == "changes") && (tab == Tab::Settings || start == "changes") {
                item.activate();
            }
        }
        let mut sys_tabs = SingleSelectModel::default();
        for (i, v) in VIEWS.iter().enumerate() {
            let item = sys_tabs.insert().text(v.name).data(*v);
            if v.id == start || (i == 0 && !VIEWS.iter().any(|v| v.id == start)) {
                item.activate();
            }
        }
        let mut diff_tabs = SingleSelectModel::default();
        diff_tabs.insert().text("What changed").data(DiffTab::Plain).activate();
        diff_tabs.insert().text("Exact files").data(DiffTab::Files);
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
            cfg_tabs,
            sys_tabs,
            diff_tabs,
            draft: Values::new(),
            active: Values::new(),
            known: HashMap::new(),
            all: false,
            open: HashSet::new(),
            closed: HashSet::new(),
            filter: String::new(),
            search: String::new(),
            selected: None,
            own: vec![],
            help: String::new(),
            entry: String::new(),
            comment: String::new(),
            confirmed: false,
            minutes: "10".into(),
            at: String::new(),
            comparing: None,
            change: None,
            proposal: None,
            line: String::new(),
            typed_cf: false,
            edit: vec![],
            offers: vec![],
            asked: 0,
            log: vec![],
            restart_after: false,
            question: None,
        };
        let title = match app.core.main_window_id() {
            Some(id) => app.set_window_title("TorqOS Settings".into(), id),
            None => Task::none(),
        };
        let load = app.load();
        (app, Task::batch([title, load]))
    }

    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav)
    }

    /// A page chosen in the navigation: libcosmic leaves switching to it to the application.
    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        self.nav.activate(id);
        self.note = None;
        self.change = None;
        self.proposal = None;
        self.close_drawer();
        self.load()
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        vec![widget::button::icon(icon::from_name("view-refresh-symbolic")).tooltip("Ask the broker again").on_press(Message::Refresh).into()]
    }

    /// What a running command asks a person (commit's review, an undo, a delete): the yes or no a terminal
    /// would ask. An approval the system needs comes after it, in COSMIC's own dialog.
    fn dialog(&self) -> Option<Element<'_, Message>> {
        let (question, _) = self.question.as_ref()?;
        let body = widget::container(widget::scrollable(widget::text::monotext(question.trim_end().to_string()).width(Length::Fill))).max_height(360.0);
        Some(
            widget::dialog()
                .title("Go ahead?")
                .control(body)
                .primary_action(widget::button::suggested("Yes").on_press(Message::Answer(true)))
                .secondary_action(widget::button::standard("No").on_press(Message::Answer(false)))
                .into(),
        )
    }

    /// The side panel: one setting, what it is, and the control for it.
    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Message>> {
        if !self.core.window.show_context {
            return None;
        }
        let path = self.selected.as_ref()?;
        Some(context_drawer::context_drawer(self.inspector(path), Message::CloseDrawer).title(path.last().cloned().unwrap_or_default()))
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
                if c == COMPARE && matches!(&r, Ok(t) if t.starts_with("No changes")) {
                    self.comment.clear();
                    self.at.clear();
                    self.confirmed = false;
                }
                if let Ok(text) = &r {
                    if c == DRAFT {
                        self.draft = values(text);
                    } else if c == ACTIVE {
                        self.active = values(text);
                    }
                }
                let got_commits = c == COMMITS;
                self.outputs.insert(c, r.into());
                if got_commits {
                    return self.read_change();
                }
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
            Message::Confirm => {
                self.restart_after = matches!(&self.glance, Loaded::Got(g) if matches!(g.needs, Needs::Restart(_)));
                self.busy = Some("confirming".into());
                self.note = None;
                return cosmic::task::future(async { Message::ConfirmDone(blocking(app::confirm).await) });
            }
            Message::ConfirmDone(r) => {
                self.busy = None;
                let restart = std::mem::take(&mut self.restart_after);
                if restart && r.is_ok() {
                    app::ask_restart();
                }
                self.note = Some(match r {
                    Ok(out) => (out.trim().to_string(), false),
                    Err(e) => (e, true),
                });
                return self.load();
            }
            Message::Asked(question, reply) => self.question = Some((question, reply)),
            Message::Answer(yes) => {
                if let Some((_, reply)) = self.question.take() {
                    reply.send(yes);
                }
            }
            Message::Restart => app::ask_restart(),
            Message::LogOut => app::ask_log_out(),
            Message::Review => {
                self.go(Page::Configuration);
                let id = self.cfg_tabs.iter().find(|&i| self.cfg_tabs.data::<Tab>(i) == Some(&Tab::Changes));
                if let Some(id) = id {
                    self.cfg_tabs.activate(id);
                }
                return self.load();
            }
            Message::Approve => {
                self.go(Page::Proposals);
                return self.load();
            }
            Message::CfgTab(id) => {
                self.cfg_tabs.activate(id);
                self.close_drawer();
                return self.load();
            }
            Message::SysTab(id) => {
                self.sys_tabs.activate(id);
                return self.load();
            }
            Message::DiffTab(id) => self.diff_tabs.activate(id),
            Message::Fill(how, command) => {
                // The Command Line's session, in the mode the command is of.
                let console = self.console.clone();
                let step = match (how == How::FillCf, self.typed_cf) {
                    (true, false) => "configure",
                    (true, true) => "top",
                    (false, true) => "exit configuration-mode",
                    (false, false) => "",
                };
                return cosmic::task::future(async move {
                    let t = off(move || console.typed(step, |_| false)).await;
                    Message::Filled(command, t.unwrap_or_else(|e| Typed { output: Err(e), configuration: false, edit: vec![] }))
                });
            }
            Message::Filled(command, t) => {
                self.typed_cf = t.configuration;
                self.edit = t.edit;
                self.line = command;
                self.go(Page::Commands);
                return self.offer();
            }

            Message::All(on) => {
                self.all = on;
                return self.learn(vec![]);
            }
            Message::Toggle(path, open) => {
                if self.all {
                    if open {
                        self.open.insert(path.clone());
                        return self.learn(path);
                    }
                    self.open.remove(&path);
                } else if open {
                    self.closed.remove(&path);
                } else {
                    self.closed.insert(path);
                }
            }
            Message::Known(path, got) => {
                self.known.insert(path, got);
            }
            Message::Select(path) => {
                self.entry.clear();
                self.own.clear();
                self.help.clear();
                self.selected = Some(path.clone());
                self.set_show_context(true);
                return self.read_selected(path);
            }
            Message::Selected(path, own, parent) => {
                if self.selected.as_ref() == Some(&path) {
                    self.help = path.last().and_then(|w| parent.iter().find(|c| &c.word == w)).map(|c| c.help.clone()).unwrap_or_default();
                    self.own = own;
                }
            }
            Message::CloseDrawer => self.close_drawer(),
            Message::Pick(path, word) => {
                // A word that ends the line is a value to set; any other is a place to go down to.
                let console = self.console.clone();
                let before = format!("{} ", self.line_at("set", &path, Some(&word)));
                return cosmic::task::future(async move {
                    let got = off(move || console.choices(At::Draft, &before)).await.unwrap_or_default();
                    Message::Picked(path, word, got)
                });
            }
            Message::Picked(path, word, next) => {
                // The person has moved on: this word was for a place they have left.
                if self.selected.as_ref() != Some(&path) {
                    return Task::none();
                }
                if next.is_empty() {
                    self.note = Some((format!("Could not read what may follow `{word}`."), true));
                    return Task::none();
                }
                // `<[Enter]>` alone: the line is whole. With other words (`enable`: true, false) there is a choice to show.
                if next.iter().all(|c| c.word == "<[Enter]>") {
                    let line = self.line_at("set", &path, Some(&word));
                    return self.act(At::Draft, line);
                }
                let mut below = path;
                below.push(word);
                return self.update(Message::Select(below));
            }
            Message::Filter(s) => self.filter = s,
            Message::Entry(s) => self.entry = s,
            Message::SetEntry => {
                let value = self.entry.trim().to_string();
                if let (false, Some(path)) = (value.is_empty(), self.selected.clone()) {
                    self.entry.clear();
                    let line = self.line_at("set", &path, Some(&value));
                    return self.act(At::Draft, line);
                }
            }
            Message::SetBool(on) => {
                if let Some(path) = self.selected.clone() {
                    let line = self.line_at("set", &path, Some(if on { "true" } else { "false" }));
                    return self.act(At::Draft, line);
                }
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
            Message::Compare(n) => {
                self.comparing = Some(n);
                return self.read(At::Read, format!("show system rollback {n} compare 0"));
            }

            Message::OpenChange(seq) => {
                self.change = Some(seq);
                self.note = None;
                return self.read_change();
            }
            Message::OpenProposal(seq) => {
                self.proposal = Some(seq);
                self.note = None;
                let key = format!("proposal {seq}");
                return cosmic::task::future(async move {
                    let got = blocking(move || app::proposal_effect(seq)).await;
                    Message::Output(key, got)
                });
            }
            Message::Back => {
                self.change = None;
                self.proposal = None;
                self.note = None;
                return self.load();
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
                return Task::stream(cosmic::iced::stream::channel(4, async move |mut out: Sender<Message>| {
                    let ask = asker(out.clone());
                    let shown = line.clone();
                    let t = off(move || console.typed(&line, ask)).await;
                    let _ = out.send(Message::Typed(shown, t.unwrap_or_else(|e| Typed { output: Err(e), configuration: false, edit: vec![] }))).await;
                }))
                .map(cosmic::Action::App);
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
        let spacing = cosmic::theme::spacing();
        let body = match self.page() {
            Page::Overview => self.overview(),
            Page::Configuration => self.configuration(),
            Page::History => match self.change {
                Some(seq) => self.change_page(seq),
                None => self.history(),
            },
            Page::Proposals => match self.proposal {
                Some(seq) => self.proposal_page(seq),
                None => self.proposals(),
            },
            Page::System => self.system(),
            Page::Commands => self.commands(),
        };
        let mut content = widget::column::with_capacity(3).spacing(spacing.space_m).max_width(960);
        if let Some(what) = &self.busy {
            content = content.push(widget::text::body(format!("Working: {what}…")));
        }
        if let Some((text, failed)) = &self.note {
            content = content.push(note(text, *failed));
        }
        content = content.push(body);
        widget::scrollable(widget::container(content).width(Length::Fill).center_x(Length::Fill).padding(spacing.space_m)).into()
    }
}

/// A configuration's `| display set` lines, by setting.
fn values(shown: &str) -> Values {
    let mut out = Values::new();
    for (path, value) in app::settings_of(shown) {
        out.entry(path).or_default().push(value);
    }
    out
}

/// The words that may follow a place, without what is not a word.
fn words_of(c: &[Choice]) -> impl Iterator<Item = &Choice> {
    c.iter().filter(|c| !c.is_placeholder() && c.word != "|")
}

/// Whether what follows a place is its value, not settings below it.
fn is_value(c: &[Choice]) -> bool {
    c.iter().any(|c| c.is_placeholder()) || (!c.is_empty() && words_of(c).all(|c| c.word == "true" || c.word == "false"))
}

/// The next words below `path` in a configuration.
fn below(values: &Values, path: &[String], out: &mut BTreeSet<String>) {
    for key in values.keys() {
        if key.len() > path.len() && key[..path.len()] == *path {
            out.insert(key[path.len()].clone());
        }
    }
}

impl Settings {
    fn page(&self) -> Page {
        self.nav.active_data::<Page>().copied().unwrap_or(Page::Overview)
    }

    fn go(&mut self, page: Page) {
        let id = self.nav.iter().find(|&i| self.nav.data::<Page>(i) == Some(&page));
        if let Some(id) = id {
            self.nav.activate(id);
        }
    }

    fn close_drawer(&mut self) {
        self.selected = None;
        self.set_show_context(false);
    }

    fn tab(&self) -> Tab {
        self.cfg_tabs.active_data::<Tab>().copied().unwrap_or(Tab::Settings)
    }

    fn view_shown(&self) -> Option<&'static View> {
        self.sys_tabs.active_data::<&'static View>().copied()
    }

    /// Ask again for what this page shows (and the state every page's actions depend on).
    fn load(&mut self) -> Task<cosmic::Action<Message>> {
        let console = self.console.clone();
        let mut tasks = vec![cosmic::task::future(async move {
            Message::Glance(blocking(move || app::glance(app::session_recorded().as_deref(), console.uncommitted())).await)
        })];
        match self.page() {
            Page::Overview => tasks.push(cosmic::task::future(async { Message::Machine(blocking(app::machine).await) })),
            Page::History => {
                tasks.push(cosmic::task::future(async { Message::History(blocking(|| app::history(HISTORY)).await) }));
                tasks.push(self.read(At::Read, COMMITS.into()));
            }
            Page::Proposals => tasks.push(cosmic::task::future(async { Message::Proposals(blocking(app::proposals).await) })),
            Page::Configuration => match self.tab() {
                Tab::Settings => {
                    tasks.push(self.read(At::Draft, DRAFT.into()));
                    tasks.push(self.read(At::Read, ACTIVE.into()));
                    if self.all {
                        tasks.push(self.learn(vec![]));
                    }
                    if let Some(path) = self.selected.clone() {
                        tasks.push(self.read_selected(path));
                    }
                }
                Tab::Changes => tasks.push(self.read(At::Draft, COMPARE.into())),
                Tab::Earlier => tasks.push(self.read(At::Read, COMMITS.into())),
            },
            Page::System => {
                if let Some(v) = self.view_shown() {
                    tasks.extend(v.sections.iter().map(|(_, c)| self.read(At::Read, c.to_string())));
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

    /// `verb` at a place in the configuration, with one more word.
    fn line_at(&self, verb: &str, path: &[String], last: Option<&str>) -> String {
        let mut w: Vec<&str> = vec![verb];
        w.extend(path.iter().map(String::as_str));
        w.extend(last);
        app::line(&w)
    }

    /// What torqcli would offer below a place, for the tree.
    fn learn(&self, path: Path) -> Task<cosmic::Action<Message>> {
        if self.known.contains_key(&path) {
            return Task::none();
        }
        let console = self.console.clone();
        let before = format!("{} ", self.line_at("set", &path, None));
        cosmic::task::future(async move {
            let got = off(move || console.choices(At::Draft, &before)).await.unwrap_or_default();
            Message::Known(path, got)
        })
    }

    /// What the side panel needs of a setting: what may follow it, and its help, from the place above it.
    fn read_selected(&self, path: Path) -> Task<cosmic::Action<Message>> {
        let console = self.console.clone();
        let own = format!("{} ", self.line_at("set", &path, None));
        let parent = format!("{} ", self.line_at("set", &path[..path.len().saturating_sub(1)], None));
        cosmic::task::future(async move {
            let (a, b) = off(move || (console.choices(At::Draft, &own), console.choices(At::Draft, &parent))).await.unwrap_or_default();
            Message::Selected(path, a, b)
        })
    }

    /// The two comparisons of the change being looked at, once `show system commit` says which it is.
    fn read_change(&self) -> Task<cosmic::Action<Message>> {
        let Some((older, newer)) = self.change_configurations() else { return Task::none() };
        let plain = format!("show system rollback {older} compare {newer}");
        let files = format!("{plain} | display diff");
        Task::batch([self.read(At::Read, plain), self.read(At::Read, files)])
    }

    /// The configuration before the change being looked at, and its own.
    fn change_configurations(&self) -> Option<(u32, u32)> {
        let Some(Loaded::Got(shown)) = self.outputs.get(COMMITS) else { return None };
        let n = app::rollback_of(shown, self.change?)?;
        app::configurations(shown).iter().any(|(k, _)| *k == n + 1).then_some((n + 1, n))
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
        Task::stream(cosmic::iced::stream::channel(4, async move |mut out: Sender<Message>| {
            let ask = asker(out.clone());
            let c = line.clone();
            let got = off(move || console.act(at, &c, ask)).await.and_then(|r| r);
            let _ = out.send(Message::Did(line, got)).await;
        }))
        .map(cosmic::Action::App)
    }

    fn terminal(&mut self, script: String, draft: bool, what: String) -> Task<cosmic::Action<Message>> {
        self.busy = Some(format!("{what} (in the terminal)"));
        self.note = None;
        cosmic::task::future(async move {
            let got = off(move || app::in_terminal(&script, draft)).await.and_then(|r| r);
            Message::Closed(what, got)
        })
    }

    /// What may follow the Command Line's line.
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

    fn diff(&self, command: &str) -> Element<'_, Message> {
        match self.outputs.get(command) {
            None | Some(Loaded::Waiting) => waiting(),
            Some(Loaded::Failed(why)) => failed(why),
            Some(Loaded::Got(text)) => diff_view(text),
        }
    }

    // ── The tree ─────────────────────────────────────────────────────────────────────────────────

    fn change_at(&self, path: &[String]) -> Change {
        match (self.draft.get(path), self.active.get(path)) {
            (Some(d), Some(a)) if d == a => Change::Same,
            (Some(_), Some(_)) => Change::Changed,
            (Some(_), None) => Change::New,
            (None, Some(_)) => Change::Removed,
            (None, None) => Change::Same,
        }
    }

    /// The rows from `path` down. Whether anything there matched the filter.
    fn walk(&self, path: &mut Path, wanted: &str, rows: &mut Vec<TreeRow>) -> bool {
        let mut next = BTreeSet::new();
        below(&self.draft, path, &mut next);
        below(&self.active, path, &mut next);
        if self.all {
            if let Some(known) = self.known.get(path.as_slice()).filter(|k| !is_value(k)) {
                next.extend(words_of(known).map(|c| c.word.clone()));
            }
        }
        let mut any = false;
        for word in next {
            if rows.len() >= LISTED {
                break;
            }
            path.push(word);
            let values = self.draft.get(path.as_slice()).or_else(|| self.active.get(path.as_slice()));
            let mut deeper = BTreeSet::new();
            below(&self.draft, path, &mut deeper);
            below(&self.active, path, &mut deeper);
            let unknown = self.all && values.is_none() && !self.known.get(path.as_slice()).is_some_and(|k| is_value(k));
            let branch = !deeper.is_empty() || unknown;
            let open = !wanted.is_empty() || if self.all { self.open.contains(path.as_slice()) } else { !self.closed.contains(path.as_slice()) };
            let at = rows.len();
            let joined = path.join(" ").to_lowercase();
            let matched = wanted.is_empty() || joined.contains(wanted) || values.is_some_and(|v| v.iter().any(|x| x.to_lowercase().contains(wanted)));
            rows.push(TreeRow { path: path.clone(), branch, open, value: values.map(|v| v.join(", ")), change: self.change_at(path) });
            let inside = branch && open && self.walk(path, wanted, rows);
            if matched || inside {
                any = true;
            } else {
                rows.truncate(at);
            }
            path.pop();
        }
        any
    }

    fn tree(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut rows = vec![];
        self.walk(&mut vec![], &self.filter.to_lowercase(), &mut rows);
        if rows.is_empty() {
            return widget::text::body(if self.filter.is_empty() { "Nothing is set yet." } else { "Nothing here matches." }).into();
        }
        let mut list = widget::list_column();
        for r in rows {
            let depth = (r.path.len() - 1) as f32 * 20.0;
            let chevron: Element<'_, Message> = if r.branch {
                let glyph = if r.open { "pan-down-symbolic" } else { "pan-end-symbolic" };
                widget::mouse_area(icon::from_name(glyph).size(16).icon()).on_press(Message::Toggle(r.path.clone(), !r.open)).into()
            } else {
                widget::container(widget::text::body("")).width(Length::Fixed(16.0)).into()
            };
            let name = r.path.last().cloned().unwrap_or_default();
            let mut line = widget::row::with_capacity(5).spacing(spacing.space_xs).align_y(Alignment::Center);
            line = line.push(chevron);
            let selected = self.selected.as_ref() == Some(&r.path);
            line = line.push(if selected { widget::text::heading(name) } else { widget::text::body(name) });
            if let Some(v) = r.value.filter(|_| !r.branch || r.change != Change::Same) {
                line = line.push(widget::text::monotext(v));
            }
            line = line.push(widget::container(widget::text::body("")).width(Length::Fill));
            match r.change {
                Change::Same => {}
                Change::New => line = line.push(tagged("new", Colour::Good)),
                Change::Changed => line = line.push(tagged("changed", Colour::Warn)),
                Change::Removed => line = line.push(tagged("removed", Colour::Bad)),
            }
            let on_press = if r.branch { Message::Toggle(r.path.clone(), !r.open) } else { Message::Select(r.path.clone()) };
            if r.branch {
                line = line.push(widget::button::icon(icon::from_name("document-properties-symbolic")).tooltip("About this setting").on_press(Message::Select(r.path.clone())));
            }
            let body = widget::container(line).padding(Padding { top: 0.0, right: 0.0, bottom: 0.0, left: depth });
            list = list.add(widget::mouse_area(body).on_press(on_press));
        }
        list.into()
    }

    /// The side panel for a setting.
    fn inspector(&self, path: &Path) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut col = widget::column::with_capacity(8).spacing(spacing.space_m);
        col = col.push(widget::text::caption(path.join(" › ")));
        if !self.help.is_empty() {
            col = col.push(widget::text::body(self.help.clone()));
        }
        let draft = self.draft.get(path);
        let active = self.active.get(path);

        // A switch, a box for a value, or the choices: as what may follow this setting says.
        let booleans = !self.own.is_empty() && words_of(&self.own).count() > 0 && words_of(&self.own).all(|c| c.word == "true" || c.word == "false");
        if booleans {
            let on = draft.is_some_and(|v| v.iter().any(|x| x == "true"));
            col = col.push(settings::item::builder("On").control(widget::toggler(on).on_toggle(Message::SetBool)));
        } else {
            for c in self.own.iter().filter(|c| c.is_placeholder() && c.word != "<[Enter]>") {
                col = col.push(
                    widget::column::with_capacity(3)
                        .spacing(spacing.space_xxs)
                        .push(widget::text::heading("Value"))
                        .push(widget::text::caption(c.help.clone()))
                        .push(
                            widget::row::with_capacity(2)
                                .spacing(spacing.space_xs)
                                .align_y(Alignment::Center)
                                .push(widget::text_input(c.word.clone(), &self.entry).on_input(Message::Entry).on_submit(|_| Message::SetEntry).width(Length::Fill))
                                .push(widget::button::suggested("Set").on_press(Message::SetEntry)),
                        ),
                );
            }
            let words: Vec<&Choice> = words_of(&self.own).filter(|c| c.word != "<[Enter]>").collect();
            if !words.is_empty() {
                let mut list = settings::section().title("Choose");
                for c in words.iter().take(LISTED) {
                    let row = settings::item_row(vec![
                        widget::column::with_capacity(2).push(widget::text::body(c.word.clone())).push(widget::text::caption(c.help.clone())).width(Length::Fill).into(),
                        icon::from_name("go-next-symbolic").size(16).icon().into(),
                    ])
                    .spacing(spacing.space_s);
                    list = list.add(widget::mouse_area(row).on_press(Message::Pick(path.clone(), c.word.clone())));
                }
                col = col.push(list);
            }
            if self.own.len() == 1 && self.own[0].word == "<[Enter]>" && draft.is_none() {
                col = col.push(widget::button::suggested("Use this value").on_press(Message::Act(At::Draft, self.line_at("set", path, None))));
            }
        }

        // What it is set to, each value removable.
        if let Some(values) = draft {
            let mut section = settings::section().title("In your draft");
            for v in values {
                let control: Element<'_, Message> = if values.len() > 1 {
                    widget::button::icon(icon::from_name("edit-delete-symbolic")).tooltip("Remove this one").on_press(Message::Act(At::Draft, self.line_at("delete", path, Some(v)))).into()
                } else {
                    widget::text::body("").into()
                };
                section = section.add(settings::item_row(vec![widget::text::monotext(v.clone()).width(Length::Fill).into(), control]));
            }
            col = col.push(section);
        }
        if active != draft {
            let in_effect = active.map_or("Not set".to_string(), |v| v.join(", "));
            col = col.push(widget::column::with_capacity(2).push(widget::text::heading("In effect now")).push(widget::text::monotext(in_effect)).spacing(spacing.space_xxs));
        }

        let mut buttons = widget::row::with_capacity(2).spacing(spacing.space_xs);
        if active != draft {
            buttons = buttons.push(widget::button::standard("Undo my change").on_press(Message::Act(At::Draft, format!("rollback 0 {}", app::line(&path.iter().map(String::as_str).collect::<Vec<_>>())))));
        }
        let anything_here = draft.is_some() || self.draft.keys().any(|k| k.len() > path.len() && k[..path.len()] == **path);
        if anything_here {
            buttons = buttons.push(widget::button::destructive("Remove").on_press(Message::Act(At::Draft, self.line_at("delete", path, None))));
        }
        col = col.push(buttons);
        col.push(widget::text::caption(format!("The same as: set {}", path.join(" ")))).into()
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
            waiting_on = waiting_on.add(attention("You have changes not yet committed.".to_string(), widget::button::standard("Review").on_press(Message::Review)));
        }
        if g.waiting.0 > 0 {
            any = true;
            let mut s = if g.waiting.0 == 1 { "A proposal is waiting for you.".to_string() } else { format!("{} proposals are waiting for you.", g.waiting.0) };
            if g.waiting.1 > 0 {
                s.push_str(&format!(" {} came from a session that read untrusted content.", g.waiting.1));
            }
            waiting_on = waiting_on.add(attention(s, widget::button::standard("Review").on_press(Message::Approve)));
        }
        if any {
            page = page.push(waiting_on);
        }

        if let Some(n) = g.needs.note() {
            page = page.push(widget::text::body(n));
        }
        // `show system alarms`, line by line, as it prints them.
        let mut alarms = settings::section().title("Status");
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
        let tabs = widget::tab_bar::horizontal(&self.cfg_tabs).on_activate(Message::CfgTab);
        let body = match self.tab() {
            Tab::Settings => self.settings_tab(),
            Tab::Changes => self.changes_tab(),
            Tab::Earlier => self.earlier_tab(),
        };
        widget::column::with_capacity(2).push(tabs).push(body).spacing(spacing.space_m).into()
    }

    fn settings_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(5).spacing(spacing.space_m);
        page = page.push(widget::text::body(if self.all {
            "Every setting NixOS has. Open a branch to see what is inside it."
        } else {
            "What is set on this machine. Choose a setting to change it: nothing takes effect until you commit it under Your Changes."
        }));
        page = page.push(
            widget::row::with_capacity(3)
                .spacing(spacing.space_s)
                .align_y(Alignment::Center)
                .push(widget::search_input("Filter", &self.filter).on_input(Message::Filter).on_clear(Message::Filter(String::new())).width(Length::Fill))
                .push(widget::text::body("Show all settings"))
                .push(widget::toggler(self.all).on_toggle(Message::All)),
        );
        page = page.push(self.tree());
        page = page.push(
            widget::row::with_capacity(2)
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center)
                .push(widget::search_input("Search every setting and command", &self.search).on_input(Message::Search).on_submit(|_| Message::Find).width(Length::Fill))
                .push(widget::button::standard("Search").on_press(Message::Find)),
        );
        let terms = self.search.trim();
        if !terms.is_empty() {
            let c = format!("help {terms}");
            if self.outputs.contains_key(&c) {
                page = page.push(self.output(&c));
            }
        }
        page.into()
    }

    fn changes_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let unchanged = matches!(self.outputs.get(COMPARE), Some(Loaded::Got(t)) if t.starts_with("No changes"));
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        if unchanged {
            return page.push(widget::text::body("Your draft is the configuration in effect: nothing to commit.")).into();
        }
        page = page.push(self.diff(COMPARE));
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
                .push(widget::button::standard("Check it builds").on_press(Message::Act(At::Draft, "commit check".into())))
                .push(widget::button::destructive("Discard my changes").on_press(Message::Act(At::Draft, "rollback 0".into())))
                .spacing(spacing.space_xs),
        );
        page.push(widget::text::caption("Commit shows you the change, then asks for your approval in COSMIC's own password dialog.")).into()
    }

    fn earlier_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        page = page.push(widget::text::body("Every configuration this machine has had. Loading one puts it in your draft: you review it and commit it like any change."));
        if let Some(n) = self.comparing {
            page = page.push(widget::text::heading(format!("Configuration {n}, against the one in effect")));
            page = page.push(self.diff(&format!("show system rollback {n} compare 0")));
        }
        let mut earlier = settings::section();
        match self.outputs.get(COMMITS) {
            Some(Loaded::Got(t)) => {
                for (n, text) in app::configurations(t).into_iter().skip(1).take(50) {
                    earlier = earlier.add(
                        settings::item_row(vec![
                            widget::text::body(text).width(Length::Fill).into(),
                            widget::button::standard("Compare").on_press(Message::Compare(n)).into(),
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
        let mut page = widget::column::with_capacity(days.len() + 1).spacing(spacing.space_l);
        page = page.push(widget::text::body("Every change this machine has been through. Choose one to see exactly what it did."));
        for (day, list) in days {
            let mut section = settings::section().title(day);
            for r in list {
                section = section.add(change(r));
            }
            page = page.push(section);
        }
        page.into()
    }

    /// One change: what it did, in words and exactly.
    fn change_page(&self, seq: u64) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let row = match &self.history {
            Loaded::Got(rows) => rows.iter().find(|r| r.seq == seq),
            _ => None,
        };
        let mut page = widget::column::with_capacity(5).spacing(spacing.space_m);
        page = page.push(widget::button::text("‹ History").on_press(Message::Back));
        let Some(r) = row else { return page.push(waiting()).into() };
        page = page.push(widget::text::title3(r.text.clone()));
        let mut detail = format!("#{} · {} {}", r.seq, r.day, r.time);
        if !r.who.is_empty() {
            detail.push_str(&format!(" · {}", r.who));
        }
        if r.tainted {
            detail.push_str(" · ⚠ tainted: made after reading untrusted content");
        }
        page = page.push(widget::text::caption(detail));
        if r.undoable {
            page = page.push(widget::button::standard("Undo this change…").on_press(Message::Act(At::Read, format!("request system revert {}", r.seq))));
        }
        let Some((older, newer)) = self.change_configurations() else {
            return page.push(widget::text::body("This entry made no new configuration: there is nothing to compare.")).into();
        };
        let plain = format!("show system rollback {older} compare {newer}");
        let tab = self.diff_tabs.active_data::<DiffTab>().copied().unwrap_or(DiffTab::Plain);
        page = page.push(widget::tab_bar::horizontal(&self.diff_tabs).on_activate(Message::DiffTab));
        page.push(match tab {
            DiffTab::Plain => match self.outputs.get(&plain) {
                Some(Loaded::Got(t)) if t.starts_with("No differences") => Element::from(widget::text::body("No settings changed: this change moved the software (TorqOS or NixOS). Exact files shows how.")),
                _ => self.diff(&plain),
            },
            DiffTab::Files => self.diff(&format!("{plain} | display diff")),
        })
        .into()
    }

    fn proposals(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        match &self.proposals {
            Loaded::Waiting => page = page.push(waiting()),
            Loaded::Failed(why) => page = page.push(failed(why)),
            Loaded::Got(list) if list.is_empty() => page = page.push(widget::text::body("No proposals are waiting. When the agent proposes a change, you review it here.")),
            Loaded::Got(list) => {
                let mut section = settings::section().title("Waiting for you");
                for p in list {
                    let mut who = format!("Proposed by {}", p.by);
                    if p.tainted {
                        who.push_str(" · ⚠ tainted: made after reading untrusted content");
                    }
                    let row = settings::item_row(vec![
                        widget::column::with_capacity(2).push(widget::text::body(format!("#{}  {}", p.seq, p.reason))).push(widget::text::caption(who)).width(Length::Fill).into(),
                        icon::from_name("go-next-symbolic").size(16).icon().into(),
                    ])
                    .spacing(spacing.space_s);
                    section = section.add(widget::mouse_area(row).on_press(Message::OpenProposal(p.seq)));
                }
                page = page.push(section);
            }
        }
        page.push(actions(app::AGENT)).into()
    }

    /// One proposal: what it does, and applying it.
    fn proposal_page(&self, seq: u64) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(5).spacing(spacing.space_m);
        page = page.push(widget::button::text("‹ Proposals").on_press(Message::Back));
        if let Loaded::Got(list) = &self.proposals {
            if let Some(p) = list.iter().find(|p| p.seq == seq) {
                page = page.push(widget::text::title3(p.reason.clone()));
                let mut who = format!("#{} · proposed by {}", p.seq, p.by);
                if p.tainted {
                    who.push_str(" · ⚠ tainted: made after reading untrusted content");
                }
                page = page.push(widget::text::caption(who));
            }
        }
        page = page.push(widget::button::suggested("Review and apply…").on_press(Message::Act(At::Read, format!("request system apply {seq}"))));
        page = page.push(widget::text::heading("What it does"));
        page.push(self.diff(&format!("proposal {seq}"))).into()
    }

    fn system(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let tabs = widget::tab_bar::horizontal(&self.sys_tabs).on_activate(Message::SysTab);
        let mut page = widget::column::with_capacity(4).push(tabs).spacing(spacing.space_l);
        let Some(v) = self.view_shown() else { return page.into() };
        page = page.push(actions(v.actions));
        for (title, command) in v.sections {
            page = page.push(settings::section().title(*title).add(settings::item_row(vec![self.output(command)])));
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

/// A diff, coloured and a card per file (a unified diff) or per place (`show | compare`).
fn diff_view(text: &str) -> Element<'static, Message> {
    let spacing = cosmic::theme::spacing();
    let theme = cosmic::theme::active();
    let cosmic = theme.cosmic();
    let (added, gone, hunk): (cosmic::iced::Color, cosmic::iced::Color, cosmic::iced::Color) = (cosmic.success_color().into(), cosmic.destructive_color().into(), cosmic.accent_color().into());
    let mut cards: Vec<(String, Vec<(String, Option<cosmic::iced::Color>)>)> = vec![];
    let all: Vec<&str> = text.lines().collect();
    for line in all.iter().take(DIFF_LINES) {
        let title = line
            .strip_prefix("[edit")
            .map(|t| t.trim().trim_end_matches(']').trim().to_string())
            .or_else(|| line.strip_prefix("+++ b/").map(str::to_string))
            .or_else(|| line.strip_prefix("--- a/").map(str::to_string));
        if line.starts_with("diff --git") || line.starts_with("index ") {
            continue;
        }
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            // One card a file: its `---` opens it, its `+++` names it if it is new.
            let name = title.unwrap_or_default();
            if line.starts_with("--- ") || cards.is_empty() {
                cards.push((name, vec![]));
            } else if let (Some(last), false) = (cards.last_mut(), name.is_empty()) {
                last.0 = name;
            }
            continue;
        }
        if line.starts_with("[edit") {
            cards.push((title.unwrap_or_default(), vec![]));
            continue;
        }
        if cards.is_empty() {
            cards.push((String::new(), vec![]));
        }
        let colour = if line.starts_with("@@") {
            Some(hunk)
        } else if line.starts_with('+') {
            Some(added)
        } else if line.starts_with('-') {
            Some(gone)
        } else {
            None
        };
        if let Some(card) = cards.last_mut() {
            card.1.push((line.to_string(), colour));
        }
    }
    let mut column = widget::column::with_capacity(cards.len() + 1).spacing(spacing.space_m);
    for (title, lines) in cards {
        let mut body = widget::column::with_capacity(lines.len());
        for (line, colour) in lines {
            let t = widget::text::monotext(line).width(Length::Fill);
            body = body.push(match colour {
                Some(c) => t.class(cosmic::theme::Text::Color(c)),
                None => t,
            });
        }
        let mut section = settings::section();
        if !title.is_empty() {
            section = section.title(title);
        }
        column = column.push(section.add(settings::item_row(vec![body.into()])));
    }
    if all.len() > DIFF_LINES {
        column = column.push(widget::text::caption(format!("The first {DIFF_LINES} of {} lines.", all.len())));
    }
    column.into()
}

/// A page's buttons: each a torqcli command, run or finished on the Command Line.
fn actions(list: &'static [Action]) -> Element<'static, Message> {
    let mut section = settings::section().title("Actions");
    for a in list {
        let (button, command) = match a.how {
            How::Run => (widget::button::standard("Run").on_press(Message::Act(At::Read, a.command.to_string())), a.command.to_string()),
            how => (widget::button::standard("Open").on_press(Message::Fill(how, a.command.to_string())), format!("{}…", a.command.trim_end())),
        };
        section = section.add(settings::item::builder(a.label).description(command).control(button));
    }
    section.into()
}

fn attention<'a>(text: String, button: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    settings::item_row(vec![widget::text::body(text).width(Length::Fill).into(), button.into()]).spacing(cosmic::theme::spacing().space_s).into()
}

/// One change: its mark, what happened, who and when, its number; choosing it shows what it did.
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
    let row = settings::item_row(vec![
        colour.icon(name, 16),
        widget::column::with_capacity(2).push(widget::text::body(r.text.clone())).push(widget::text::caption(detail)).width(Length::Fill).into(),
        widget::text::body(format!("#{}", r.seq)).into(),
        icon::from_name("go-next-symbolic").size(16).icon().into(),
    ])
    .spacing(cosmic::theme::spacing().space_s)
    .align_y(Alignment::Center);
    widget::mouse_area(row).on_press(Message::OpenChange(r.seq)).into()
}

/// How a mark is coloured: the theme's own success, warning and destructive colours.
#[derive(Clone, Copy)]
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

    fn colour(self) -> cosmic::iced::Color {
        let theme = cosmic::theme::active();
        let c = theme.cosmic();
        match self {
            Colour::Good => c.success_color().into(),
            Colour::Warn => c.warning_color().into(),
            Colour::Bad => c.destructive_color().into(),
            Colour::Plain => c.on_bg_color().into(),
        }
    }
}

/// A short coloured word beside a setting.
fn tagged(word: &'static str, colour: Colour) -> Element<'static, Message> {
    widget::text::caption(word).class(cosmic::theme::Text::Color(colour.colour())).into()
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
