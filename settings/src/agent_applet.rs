//! `torq-agent-applet`: the person's agent in COSMIC's panel (ADR-051 4), beside TorqOS's own. Its mark is
//! quiet while the agent is idle, in the accent while it works, in the warning colour when something is
//! wrong. Its popup: the agent's state and its model (REMOTE marked), a question box whose answer streams
//! below, the proposals the answer made with **Review** (TorqOS Settings' Proposals page), **Stop** while it
//! works, and **Open** (the agent's window). It is the person's own program: it talks only to their own
//! agent's socket, through torqcli's client, runs nothing as root and holds no secret.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

mod agentkit;

use cosmic::applet::token::subscription::{activation_token_subscription, TokenRequest, TokenUpdate};
use cosmic::cctk::sctk::reexports::calloop::channel::Sender as TokenSender;
use cosmic::iced::window::Id;
use cosmic::iced::{Alignment, Length, Rectangle, Subscription};
use cosmic::prelude::*;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget;
use std::time::Duration;
use torq::agent::client::{Client, Status, Unreachable};
use torq::agent::conversation::{self, Conversation, Fate};
use torq::agent::setup::Brain;
use torq::agent::Event;

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

/// How often the agent is looked at (its service is asked only when it runs: a look never starts it).
const EVERY: Duration = Duration::from_secs(10);
/// How often while the popup is open.
const OPEN: Duration = Duration::from_secs(2);
/// The most of an answer the popup shows; the window has it all.
const SHOWN: usize = 4000;
/// The agent's window, by absolute path (torq::app::system_program).
const WINDOW: &str = "torq-agent-ui";

struct Applet {
    core: cosmic::Core,
    popup: Option<Id>,
    /// The person's own agent, or why they have none.
    client: Result<Client, Unreachable>,
    /// What its service said when last asked (`None`: not running, not asked).
    reach: Result<Option<Status>, Unreachable>,
    brain: Option<Brain>,
    /// What became of the proposals its answers made, by the ledger.
    fates: Vec<(u64, Fate)>,
    /// A look under way: no second while it is.
    looking: bool,
    /// The questions asked here.
    talk: Conversation,
    question: String,
    /// What a Stop or the like said, until the next action.
    note: Option<String>,
    /// The compositor's activation tokens, so the window opened is raised, not left behind.
    tokens: Option<TokenSender<TokenRequest>>,
    /// torqcli's question about a Decline, waiting for the person's answer.
    declining: Option<(String, agentkit::Reply)>,
    /// The proposal whose Decline is in flight, from the press until it ends: one at a time, and only its own end
    /// clears its question.
    decline_seq: Option<u64>,
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
    Open,
    Review,
    /// Decline a proposal: torqcli's line; its question, the person's answer, what it said.
    Decline(u64),
    DeclineAsked(u64, String, agentkit::Reply),
    Answer(bool),
    Declined(u64, Result<String, String>),
    Settings,
    Token(TokenUpdate),
    PopupClosed(Id),
    Surface(cosmic::surface::Action<Message>),
}

impl Applet {
    fn working(&self) -> bool {
        self.talk.working() || matches!(self.reach, Ok(Some(Status { running: true, .. })))
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut col = widget::column::with_capacity(10).spacing(spacing.space_s).padding(spacing.space_s).width(Length::Fixed(380.0));
        col = col.push(widget::text::heading("TorqOS Agent"));
        let (line, _) = conversation::glance(&self.reach, self.talk.last());
        col = col.push(widget::text::body(line).width(Length::Fill));
        if let Err(e) = &self.client {
            // No agent of their own here: where to set one up.
            col = col.push(widget::text::caption(e.to_string()).width(Length::Fill));
            col = col.push(widget::divider::horizontal::default());
            col = col.push(widget::button::text("Agent settings").on_press(Message::Settings));
            return self.core.applet.popup_container(col).into();
        }
        if let Some(b) = &self.brain {
            col = col.push(widget::text::caption(b.line()).width(Length::Fill));
        }
        col = col.push(
            widget::text_input("Ask your agent…", &self.question)
                .on_input(Message::Question)
                .on_submit_maybe((!self.working()).then_some(|_| Message::Ask)),
        );
        if let Some(t) = self.talk.last() {
            col = col.push(widget::text::caption(t.question.clone()).width(Length::Fill));
            // Said before the answer, never scrolled out of sight: what it proposes is marked so.
            if let Some(taint) = t.taint() {
                let theme = cosmic::theme::active();
                col = col.push(widget::text::caption(taint).class(cosmic::theme::Text::Color(theme.cosmic().warning_color().into())).width(Length::Fill));
            }
            let mut answer = widget::column::with_capacity(4).spacing(spacing.space_xxs);
            if !t.answer.is_empty() {
                // Named as the agent's own words, apart from the rows TorqOS draws.
                answer = answer.push(widget::text::caption(crate::agentkit::AGENTS_WORDS));
                answer = answer.push(widget::text::body(conversation::shown(&t.answer, SHOWN)).width(Length::Fill));
            }
            for e in &t.errors {
                answer = answer.push(widget::text::caption(e.clone()).width(Length::Fill));
            }
            answer = answer.push(widget::text::caption(t.state()));
            col = col.push(widget::container(widget::scrollable(answer)).padding(spacing.space_xs).class(cosmic::theme::Container::Secondary).max_height(260.0));
            for row in t.proposal_rows(&self.fates) {
                let mut line = widget::row::with_capacity(2).align_y(Alignment::Center).push(widget::text::body(row.text).width(Length::Fill));
                if row.review {
                    line = line.push(widget::button::standard("Review").on_press(Message::Review));
                    line = line.push(widget::button::destructive("Decline").on_press_maybe(self.decline_seq.is_none().then_some(Message::Decline(row.seq))));
                }
                col = col.push(line);
            }
        }
        // A Decline's question, as torqcli asks it: the proposal, then yes or no.
        if let Some((q, _)) = &self.declining {
            col = col.push(widget::container(widget::scrollable(widget::text::monotext(q.trim_end().to_string()).width(Length::Fill))).max_height(200.0));
            col = col.push(
                widget::row::with_capacity(2)
                    .spacing(spacing.space_xs)
                    .push(widget::button::destructive("Decline it").on_press(Message::Answer(true)))
                    .push(widget::button::standard("Keep it").on_press(Message::Answer(false))),
            );
        }
        if let Some(n) = &self.note {
            col = col.push(widget::text::caption(n.clone()).width(Length::Fill));
        }
        col = col.push(widget::divider::horizontal::default());
        let mut buttons = widget::row::with_capacity(2).spacing(spacing.space_xs);
        if self.working() {
            buttons = buttons.push(widget::button::destructive("Stop").on_press(Message::Stop));
        }
        buttons = buttons.push(widget::button::text("Open").on_press(Message::Open));
        col = col.push(buttons);
        self.core.applet.popup_container(col).into()
    }

    /// A program of the person's, raised if it runs: with an activation token when the compositor gives one. One that
    /// cannot start is said in the popup.
    fn launch(&mut self, exec: &str) {
        let asked = self.tokens.as_ref().is_some_and(|tx| tx.send(TokenRequest { app_id: <Self as cosmic::Application>::APP_ID.to_string(), exec: exec.to_string() }).is_ok());
        // No token to be had (none asked for, or its subscription gone): started without one.
        if !asked {
            self.said_if_not(spawn(exec, None));
        }
    }

    fn said_if_not(&mut self, r: std::io::Result<()>) {
        if let Err(e) = r {
            self.note = Some(format!("could not start it: {e}"));
        }
    }
}

/// `exec` (a program and its words, none with spaces), with the token that lets it raise its window.
fn spawn(exec: &str, token: Option<String>) -> std::io::Result<()> {
    let mut words = exec.split(' ');
    let Some(program) = words.next() else { return Err(std::io::Error::other("nothing to start")) };
    let mut c = std::process::Command::new(program);
    c.args(words);
    if let Some(t) = token {
        c.env("XDG_ACTIVATION_TOKEN", &t).env("DESKTOP_STARTUP_ID", &t);
    }
    // Waited for on a thread of its own: it leaves no zombie behind.
    let mut child = c.spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

impl cosmic::Application for Applet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.Agent";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: ()) -> (Self, Task<cosmic::Action<Message>>) {
        let client = Client::mine();
        let app = Applet { core, popup: None, client: client.clone(), reach: Ok(None), brain: None, fates: vec![], looking: true, talk: Conversation::default(), question: String::new(), note: None, tokens: None, declining: None, decline_seq: None };
        (app, cosmic::task::future(async move { Message::Looked(agentkit::look(client, vec![]).await) }))
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        // Sooner while the popup is open: what it shows is looked at.
        let every = if self.popup.is_some() { OPEN } else { EVERY };
        Subscription::batch([cosmic::iced::time::every(every).map(|_| Message::Look), activation_token_subscription(0).map(Message::Token)])
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Look => {
                if self.looking {
                    return Task::none();
                }
                self.looking = true;
                // The service asked only while it runs: looking never starts it.
                let (client, seqs) = (self.client.clone(), self.talk.proposals().into_iter().map(|(s, _)| s).collect());
                return cosmic::task::future(async move { Message::Looked(agentkit::look(client, seqs).await) });
            }
            Message::Looked(l) => {
                self.looking = false;
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
                return agentkit::ask(client, q, None, Message::Heard, Message::Lost);
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
                self.note = Some("Stopping…".into());
                return cosmic::task::future(async move { Message::Stopped(agentkit::off(move || client.stop().map_err(|e| e.to_string())).await.and_then(|r| r)) });
            }
            Message::Stopped(r) => {
                self.note = match r {
                    Ok(_) => None,
                    Err(e) => Some(e),
                };
                return self.update(Message::Look);
            }
            Message::Decline(seq) => {
                if self.decline_seq.is_some() {
                    return Task::none();
                }
                self.decline_seq = Some(seq);
                self.note = None;
                return agentkit::decline(seq, Message::DeclineAsked, Message::Declined);
            }
            Message::DeclineAsked(seq, q, reply) if self.decline_seq == Some(seq) => self.declining = Some((q, reply)),
            Message::DeclineAsked(..) => {}
            Message::Answer(yes) => {
                if let Some((_, reply)) = self.declining.take() {
                    reply.send(yes);
                }
            }
            Message::Declined(seq, _) if self.decline_seq != Some(seq) => {}
            Message::Declined(_, r) => {
                self.decline_seq = None;
                self.declining = None;
                self.note = Some(match r {
                    Ok(said) => conversation::shown(said.trim(), 300),
                    Err(e) => e,
                });
                // Its row then says "declined", from the ledger.
                return self.update(Message::Look);
            }
            Message::Open => self.launch(&torq::app::system_program(WINDOW)),
            Message::Review => self.launch(&format!("{} proposals", torq::app::system_program("torq-settings"))),
            Message::Settings => self.launch(&format!("{} agent", torq::app::system_program("torq-settings"))),
            Message::Token(TokenUpdate::Init(tx)) => self.tokens = Some(tx),
            Message::Token(TokenUpdate::Finished) => self.tokens = None,
            Message::Token(TokenUpdate::ActivationToken { token, exec }) => self.said_if_not(spawn(&exec, token)),
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
        let (line, mood) = conversation::glance(&self.reach, self.talk.last());
        // At a symbolic icon's size, as the panel's other status icons are (the tunnels applet's way, ADR-050).
        let (w, h) = self.core.applet.suggested_size(true);
        let glyph = widget::icon(agentkit::mark(mood)).width(Length::Fixed(w as f32)).height(Length::Fixed(h as f32));
        let button = self.core.applet.button_from_element(glyph, true).on_press_with_rectangle(move |offset, bounds| {
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
        self.core.applet.applet_tooltip::<Message>(button, format!("TorqOS Agent: {line}"), self.popup.is_some(), Message::Surface, None).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        self.popup_view()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
