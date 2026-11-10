//! `torq-agent-ui`: the person's agent in a window of its own (ADR-051 4), "TorqOS Agent" in the menu. The
//! conversation — each question, its answer as it streams, the tools it called and, said plainly, whether
//! its session read untrusted text — a box to ask in, **Stop**, **Forget** (asked first), and the proposals
//! it made, each with **Review** (TorqOS Settings' Proposals page). One window per person: started again (by
//! the applet's **Open**, or the menu), it raises the one that runs. `torq-agent-ui --check` asks the agent's
//! status through the same client and prints it, without a window.
//!
//! The person's own program: it talks only to their own agent's socket, through torqcli's client, runs
//! nothing as root and holds no secret.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

mod agentkit;

use cosmic::iced::{Alignment, Length, Subscription};
use cosmic::prelude::*;
use cosmic::widget;
use std::time::Duration;
use torq::agent::client::{Client, Status, Unreachable};
use torq::agent::conversation::{self, Conversation, Ended, Fate, Turn};
use torq::agent::handoff;
use torq::agent::setup::{self, Brain};
use torq::agent::Event;

/// `--ask <file>`: open with a question and what it is about, handed over in a file of the person's own
/// (`torq::agent::handoff`). Started again, the arguments go to the window that is open (an action of its).
#[derive(Clone, Debug, Default)]
struct Flags {
    action: Option<String>,
    args: Vec<String>,
}

impl cosmic::app::CosmicFlags for Flags {
    type SubCommand = String;
    type Args = Vec<String>;

    fn action(&self) -> Option<&String> {
        self.action.as_ref()
    }

    fn args(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }
}

/// The action a handoff is.
const ASK: &str = "ask";

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--check") {
        return check();
    }
    let flags = match args.as_slice() {
        [flag, file] if flag == "--ask" => Flags { action: Some(ASK.into()), args: vec![file.clone()] },
        _ => Flags::default(),
    };
    let settings = cosmic::app::Settings::default().size_limits(cosmic::iced::Limits::NONE.min_width(420.0).min_height(360.0));
    match cosmic::app::run_single_instance::<Window>(settings, flags) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("torq-agent-ui: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// `--check`: the agent's state through the client the window uses, in the panel's words; exit 0 only if
/// the person's own service answered.
fn check() -> std::process::ExitCode {
    let reach = Client::mine().and_then(|c| c.status()).map(Some);
    let (line, _) = conversation::glance(&reach, None);
    println!("{line}");
    if let Some(b) = setup::brain().ok().flatten() {
        println!("{}", b.line());
    }
    match reach {
        Ok(_) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("torq-agent-ui: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// How many of the latest turns are shown in full.
const FULL: usize = 8;

/// How often the agent's state is looked at, while the window is open (its service asked only while it runs).
const EVERY: Duration = Duration::from_secs(5);

struct Window {
    core: cosmic::Core,
    client: Result<Client, Unreachable>,
    reach: Result<Option<Status>, Unreachable>,
    brain: Option<Brain>,
    fates: Vec<(u64, Fate)>,
    /// How many turns the agent remembered at the last look: fewer now is a Forget from elsewhere.
    remembered: Option<usize>,
    looking: bool,
    talk: Conversation,
    question: String,
    /// Forget, asked first.
    forgetting: bool,
    note: Option<(String, bool)>,
    /// What an entry point attached (an alarm, a crash's summary…): sent with the next question, and then gone.
    attached: Option<String>,
    /// torqcli's question about a Decline, waiting for the person's answer.
    declining: Option<(String, agentkit::Reply)>,
}

#[derive(Debug, Clone)]
enum Message {
    Look,
    Looked(agentkit::Look),
    Question(String),
    Ask,
    Heard(Event),
    Lost(String),
    Stop,
    Stopped(Result<Status, String>),
    Forget,
    ForgetAnswer(bool),
    Forgot(Result<(), String>),
    Review,
    Settings,
    /// Drop what was attached, before asking.
    Unattach,
    Decline(u64),
    DeclineAsked(String, agentkit::Reply),
    Answer(bool),
    Declined(Result<String, String>),
}

impl Window {
    fn working(&self) -> bool {
        self.talk.working() || matches!(self.reach, Ok(Some(Status { running: true, .. })))
    }

    /// A question handed over in a file (`--ask <file>`): put in the box with what it is about attached, for
    /// the person to read and ask — or asked at once, when the person already did (the launcher).
    fn handed(&mut self, files: &[String]) -> Task<cosmic::Action<Message>> {
        let Some(file) = files.first() else { return Task::none() };
        match handoff::take(std::path::Path::new(file)) {
            Ok(h) => {
                self.question = h.question;
                self.attached = h.context;
                self.note = None;
                if h.ask_now {
                    return cosmic::Application::update(self, Message::Ask);
                }
            }
            Err(e) => self.note = Some((torq::app::said(&e), true)),
        }
        Task::none()
    }

    /// A turn: its question; then, said first, whether its session read untrusted text; its answer (only the
    /// question and its end for an older turn, `full` false, so a long sitting stays light to lay out); each
    /// proposal with its fate, and Review where it waits.
    fn turn<'a>(&self, t: &'a Turn, full: bool) -> Element<'a, Message> {
        let spacing = cosmic::theme::spacing();
        let theme = cosmic::theme::active();
        let warn: cosmic::iced::Color = theme.cosmic().warning_color().into();
        let mut col = widget::column::with_capacity(6).spacing(spacing.space_xxs);
        col = col.push(widget::text::heading(t.question.clone()).width(Length::Fill));
        if let Some(taint) = t.taint() {
            col = col.push(widget::text::body(taint).class(cosmic::theme::Text::Color(warn)).width(Length::Fill));
        }
        if full {
            if !t.answer.is_empty() {
                col = col.push(widget::text::body(t.answer.clone()).width(Length::Fill));
            }
            for e in &t.errors {
                col = col.push(widget::text::body(e.clone()).width(Length::Fill));
            }
        }
        for row in t.proposal_rows(&self.fates) {
            let mut line = widget::row::with_capacity(2).align_y(Alignment::Center).spacing(spacing.space_xs).push(widget::text::body(row.text).width(Length::Fill));
            if row.review {
                line = line.push(widget::button::standard("Review").on_press(Message::Review));
                line = line.push(widget::button::destructive("Decline…").on_press_maybe(self.declining.is_none().then_some(Message::Decline(row.seq))));
            }
            col = col.push(line);
        }
        col = col.push(widget::text::caption(if full { t.state() } else { format!("{} · its answer folded away (an earlier turn)", t.state()) }));
        widget::container(col).padding(spacing.space_s).class(cosmic::theme::Container::Card).width(Length::Fill).into()
    }
}

impl cosmic::Application for Window {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.AgentWindow";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, flags: Flags) -> (Self, Task<cosmic::Action<Message>>) {
        let mut app = Window { core, client: Client::mine(), reach: Ok(None), brain: None, fates: vec![], remembered: None, looking: false, talk: Conversation::default(), question: String::new(), forgetting: false, note: None, attached: None, declining: None };
        let title = match app.core.main_window_id() {
            Some(id) => app.set_window_title("TorqOS Agent".into(), id),
            None => Task::none(),
        };
        let look = app.update(Message::Look);
        let handed = app.handed(&flags.args);
        (app, Task::batch([title, look, handed]))
    }

    /// Started again (the applet's Open, the menu): this window raised. With the compositor's token, COSMIC's
    /// toolkit activates it itself; without one, it is asked for focus.
    fn dbus_activation(&mut self, msg: cosmic::dbus_activation::Message) -> Task<cosmic::Action<Message>> {
        // Said in its journal: started again, the window handed over here.
        eprintln!("torq-agent-ui: started again: this window raised");
        // With a question handed over (the key, the launcher, an Ask the agent): put in, or asked.
        let handed = match msg.msg {
            cosmic::dbus_activation::Details::ActivateAction { action, args } if action == ASK => self.handed(&args),
            _ => Task::none(),
        };
        let focus = match self.core.main_window_id() {
            Some(id) => cosmic::iced::window::gain_focus(id),
            None => Task::none(),
        };
        Task::batch([focus, handed])
    }

    fn subscription(&self) -> Subscription<Message> {
        cosmic::iced::time::every(EVERY).map(|_| Message::Look)
    }

    /// Forget asks first: the agent's memory of the conversation is erased for good.
    fn dialog(&self) -> Option<Element<'_, Message>> {
        // A Decline's question, as torqcli asks it: the proposal, then yes or no.
        if let Some((q, _)) = &self.declining {
            let body = widget::container(widget::scrollable(widget::text::monotext(q.trim_end().to_string()).width(Length::Fill))).max_height(360.0);
            return Some(
                widget::dialog()
                    .title("Decline this proposal?")
                    .control(body)
                    .primary_action(widget::button::destructive("Decline").on_press(Message::Answer(true)))
                    .secondary_action(widget::button::standard("Keep it").on_press(Message::Answer(false)))
                    .into(),
            );
        }
        if !self.forgetting {
            return None;
        }
        Some(
            widget::dialog()
                .title("Forget the conversation?")
                .body("Your agent forgets what it remembers of your conversation: later questions start afresh. This window's own record goes too.")
                .primary_action(widget::button::destructive("Forget").on_press(Message::ForgetAnswer(true)))
                .secondary_action(widget::button::standard("Cancel").on_press(Message::ForgetAnswer(false)))
                .into(),
        )
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Look => {
                if self.looking {
                    return Task::none();
                }
                self.looking = true;
                let (client, seqs) = (self.client.clone(), self.talk.proposals().into_iter().map(|(s, _)| s).collect());
                return cosmic::task::future(async move { Message::Looked(agentkit::look(client, seqs).await) });
            }
            Message::Looked(l) => {
                self.looking = false;
                if let Ok(Some(st)) = &l.reach {
                    // Forgotten elsewhere (the Agent page, torq ask --forget): not shown as current any more.
                    if conversation::forgotten(self.remembered, st.turns) && !self.talk.working() {
                        self.talk.clear();
                        self.note = Some(("Your agent forgot the conversation (Forget, from elsewhere): what was shown here is gone from its memory.".into(), false));
                    }
                    self.remembered = Some(st.turns);
                }
                self.client = l.client;
                self.reach = l.reach;
                self.brain = l.brain;
                self.fates = l.fates;
            }
            Message::Question(s) => self.question = s,
            Message::Ask => {
                let q = self.question.trim().to_string();
                let Ok(client) = self.client.clone() else { return Task::none() };
                if q.is_empty() || self.working() {
                    return Task::none();
                }
                self.question.clear();
                self.note = None;
                self.talk.asked(&q);
                // What was attached goes with it, once: untrusted text, it taints the session (the service says so).
                return agentkit::ask(client, q, self.attached.take(), Message::Heard, Message::Lost);
            }
            Message::Heard(e) => {
                let over = matches!(e, Event::Done { .. });
                self.talk.heard(e);
                if over {
                    return self.update(Message::Look);
                }
            }
            Message::Lost(why) => {
                self.talk.lost(&why);
                return self.update(Message::Look);
            }
            Message::Stop => {
                let Ok(client) = self.client.clone() else { return Task::none() };
                self.note = Some(("Stopping…".into(), false));
                return cosmic::task::future(async move { Message::Stopped(agentkit::off(move || client.stop().map_err(|e| e.to_string())).await.and_then(|r| r)) });
            }
            Message::Stopped(r) => {
                self.note = r.err().map(|e| (e, true));
                return self.update(Message::Look);
            }
            Message::Forget => self.forgetting = true,
            Message::ForgetAnswer(yes) => {
                self.forgetting = false;
                let Ok(client) = self.client.clone() else { return Task::none() };
                if yes {
                    return cosmic::task::future(async move { Message::Forgot(agentkit::off(move || client.forget().map_err(|e| e.to_string())).await.and_then(|r| r)) });
                }
            }
            Message::Forgot(r) => {
                self.note = Some(match r {
                    Ok(()) => {
                        self.talk.clear();
                        self.remembered = Some(0);
                        ("Your agent has forgotten the conversation.".into(), false)
                    }
                    Err(e) => (e, true),
                });
                return self.update(Message::Look);
            }
            Message::Review => torq::app::open_settings("proposals"),
            Message::Unattach => self.attached = None,
            Message::Decline(seq) => {
                self.note = None;
                return agentkit::decline(seq, Message::DeclineAsked, Message::Declined);
            }
            Message::DeclineAsked(q, reply) => self.declining = Some((q, reply)),
            Message::Answer(yes) => {
                if let Some((_, reply)) = self.declining.take() {
                    reply.send(yes);
                }
            }
            Message::Declined(r) => {
                self.declining = None;
                self.note = Some(match r {
                    Ok(said) => (conversation::shown(said.trim(), 600), false),
                    Err(e) => (e, true),
                });
                // Its row then says "declined", from the ledger.
                return self.update(Message::Look);
            }
            Message::Settings => torq::app::open_settings("agent"),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let (line, mood) = conversation::glance(&self.reach, self.talk.last());
        let mut page = widget::column::with_capacity(8).spacing(spacing.space_m).max_width(820);
        let mut head = widget::row::with_capacity(3).spacing(spacing.space_s).align_y(Alignment::Center);
        head = head.push(widget::icon(agentkit::mark(mood)).width(Length::Fixed(24.0)).height(Length::Fixed(24.0)));
        let mut said = widget::column::with_capacity(2).push(widget::text::title4(line)).width(Length::Fill);
        if let Some(b) = &self.brain {
            let model = widget::text::body(b.line());
            said = said.push(if b.remote() {
                let theme = cosmic::theme::active();
                model.class(cosmic::theme::Text::Color(theme.cosmic().warning_color().into()))
            } else {
                model
            });
        }
        head = head.push(said);
        head = head.push(widget::button::text("Agent settings").on_press(Message::Settings));
        page = page.push(head);
        if let Err(e) = &self.client {
            page = page.push(widget::text::body(e.to_string()).width(Length::Fill));
            return widget::container(page).padding(spacing.space_m).center_x(Length::Fill).into();
        }
        if let Some((n, failed)) = &self.note {
            let t = widget::text::body(n.clone()).width(Length::Fill);
            page = page.push(if *failed {
                let theme = cosmic::theme::active();
                t.class(cosmic::theme::Text::Color(theme.cosmic().destructive_color().into()))
            } else {
                t
            });
        }
        let mut talk = widget::column::with_capacity(conversation::MAX_TURNS).spacing(spacing.space_s);
        if self.talk.last().is_none() {
            talk = talk.push(widget::text::body("Ask your agent anything about this machine. It reads, explains, and proposes changes you approve; it never applies one itself."));
        }
        // The last turns in full; earlier ones folded (50 answers of 64 000 characters would be slow to lay out).
        let count = self.talk.turns().count();
        for (i, t) in self.talk.turns().enumerate() {
            talk = talk.push(self.turn(t, i + FULL >= count));
        }
        page = page.push(widget::scrollable(talk).height(Length::Fill));
        let proposals = self.talk.proposals();
        if !proposals.is_empty() {
            let mut list = widget::row::with_capacity(proposals.len() + 1).spacing(spacing.space_xs).align_y(Alignment::Center);
            list = list.push(widget::text::body("Proposed here:"));
            for (seq, tainted) in proposals.iter().take(12) {
                list = list.push(widget::text::body(if *tainted { format!("#{seq} (tainted)") } else { format!("#{seq}") }));
            }
            // Review where one of them still waits for the person.
            if self.fates.iter().any(|(s, f)| *f == Fate::Waiting && proposals.iter().any(|(p, _)| p == s)) {
                list = list.push(widget::button::standard("Review").on_press(Message::Review));
            }
            page = page.push(list);
        }
        // What an entry point attached, shown before it is sent: untrusted text, and the person may drop it.
        if let Some(a) = &self.attached {
            let theme = cosmic::theme::active();
            let warn: cosmic::iced::Color = theme.cosmic().warning_color().into();
            page = page.push(
                widget::column::with_capacity(3)
                    .spacing(spacing.space_xxs)
                    .push(widget::text::caption("Attached, sent with your question: untrusted text, so the agent's session is marked tainted when it reads it.").class(cosmic::theme::Text::Color(warn)).width(Length::Fill))
                    .push(widget::container(widget::scrollable(widget::text::monotext(a.clone()).width(Length::Fill))).max_height(160.0))
                    .push(widget::button::text("Remove it").on_press(Message::Unattach)),
            );
        }
        let working = self.working();
        let mut ask = widget::row::with_capacity(4).spacing(spacing.space_xs).align_y(Alignment::Center);
        ask = ask.push(widget::text_input("Ask your agent…", &self.question).on_input(Message::Question).on_submit_maybe((!working).then_some(|_| Message::Ask)).width(Length::Fill));
        ask = ask.push(widget::button::suggested("Ask").on_press_maybe((!working).then_some(Message::Ask)));
        if working {
            ask = ask.push(widget::button::destructive("Stop").on_press(Message::Stop));
        }
        ask = ask.push(widget::button::standard("Forget…").on_press_maybe((!working).then_some(Message::Forget)));
        page = page.push(ask);
        if self.talk.last().is_some_and(|t| t.ended == Ended::Working) {
            page = page.push(widget::text::caption("Answering: Stop ends it, everything it started included."));
        }
        widget::container(page).padding(spacing.space_m).center_x(Length::Fill).into()
    }
}
