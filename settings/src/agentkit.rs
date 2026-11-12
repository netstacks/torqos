//! What the agent's applet and its window share (ADR-051 4): its mark, drawn, and a question asked
//! through torqcli's client (`torq::agent::client`), its answer streamed into the program's messages
//! off its drawing thread.

use cosmic::cosmic_theme::palette::Srgba;
use cosmic::iced::futures::channel::mpsc::Sender;
use cosmic::iced::futures::SinkExt as _;
use cosmic::prelude::*;
use cosmic::widget::icon;
use torq::agent::client::{Client, Status, Unreachable};
use torq::agent::conversation::Fate;
use torq::agent::setup::{self, Brain};
use torq::agent::conversation::Mood;
use torq::agent::Event;

/// A colour as SVG writes it.
fn hex(c: Srgba) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.red), b(c.green), b(c.blue))
}

/// The agent's mark: a speech bubble in the panel's own text colour, filled with the accent while it works
/// and the warning colour when something is wrong — drawn, as TorqOS's mark is, since a symbolic icon has
/// one colour.
pub fn mark(mood: Mood) -> icon::Handle {
    let theme = cosmic::theme::active();
    let c = theme.cosmic();
    let ink = hex(c.on_bg_color().into());
    let (fill, dots) = match mood {
        Mood::Quiet => ("none".to_string(), ink.clone()),
        Mood::Working => (hex(c.accent_color().into()), hex(c.bg_color().into())),
        Mood::Trouble => (hex(c.warning_color().into()), hex(c.bg_color().into())),
    };
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' width='16' height='16'><path d='M3 2.5 H13 A2 2 0 0 1 15 4.5 V10 A2 2 0 0 1 13 12 H7.5 L4 14.8 V12 H3 A2 2 0 0 1 1 10 V4.5 A2 2 0 0 1 3 2.5 Z' fill='{fill}' stroke='{ink}' stroke-width='1.3' stroke-linejoin='round'/><circle cx='5' cy='7.25' r='1.05' fill='{dots}'/><circle cx='8' cy='7.25' r='1.05' fill='{dots}'/><circle cx='11' cy='7.25' r='1.05' fill='{dots}'/></svg>"
    );
    icon::from_svg_bytes(svg.into_bytes())
}

/// Ask `question`, with what is attached (`context`: untrusted text, it taints the session): each event of
/// the answer as `heard(e)`, or why there is none as `lost(why)`; on a thread of its own. If the program stops
/// listening, the thread ends the ask (and so the task) when the next event finds no one to hear it: within the
/// client's QUIET of the last one at most, not at once (the review of the rebased branch, C9).
pub fn ask<M: Send + 'static>(client: Client, question: String, context: Option<String>, heard: fn(Event) -> M, lost: fn(String) -> M) -> Task<cosmic::Action<M>> {
    Task::stream(cosmic::iced::stream::channel(16, async move |out: Sender<M>| {
        let (done, over) = cosmic::iced::futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let mut out = out;
            let mut tell = |m: M| cosmic::iced::futures::executor::block_on(out.send(m)).is_ok();
            match client.ask(&question, context) {
                Err(e) => {
                    tell(lost(torq::app::said(&e.into())));
                }
                Ok(answer) => {
                    for e in answer {
                        let listening = match e {
                            Ok(e) => tell(heard(e)),
                            Err(e) => tell(lost(torq::app::said(&e.into()))),
                        };
                        if !listening {
                            break;
                        }
                    }
                }
            }
            let _ = done.send(());
        });
        let _ = over.await;
    }))
    .map(cosmic::Action::App)
}

/// A call that blocks on a thread of its own, so the program keeps drawing.
pub async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.map_err(|_| "the call stopped before it answered".to_string())
}

/// The agent as the applet and the window see it.
#[derive(Debug, Clone)]
pub struct Look {
    /// The person's own agent (looked for again while there was none: a person added later is seen).
    pub client: Result<Client, Unreachable>,
    /// Its status, asked only of a service that runs (`None`: not running) — looking never starts it.
    pub reach: Result<Option<Status>, Unreachable>,
    pub brain: Option<Brain>,
    /// What became of the proposals the conversation named.
    pub fates: Vec<(u64, Fate)>,
}

/// Look at the agent, off the drawing thread: `client` as it was, `seqs` the proposals to look up.
pub async fn look(client: Result<Client, Unreachable>, seqs: Vec<u64>) -> Look {
    let failed = |e: String| Look { client: Err(Unreachable::Broken(e.clone())), reach: Err(Unreachable::Broken(e)), brain: None, fates: vec![] };
    off(move || {
        let client = client.or_else(|_| Client::mine());
        let reach = match &client {
            Err(e) => Err(e.clone()),
            Ok(c) => match setup::setup() {
                Ok(s) if setup::service_running(&s.me) => c.status().map(Some),
                Ok(_) => Ok(None),
                Err(e) => Err(Unreachable::Broken(torq::app::said(&e))),
            },
        };
        let fates = torq::app::agent_fates(&seqs).unwrap_or_default();
        Look { client, reach, brain: setup::brain().ok().flatten(), fates }
    })
    .await
    .unwrap_or_else(failed)
}

/// The way back to a torqcli line waiting for the person's yes or no.
#[derive(Clone)]
pub struct Reply(std::sync::Arc<std::sync::Mutex<Option<std::sync::mpsc::Sender<bool>>>>);

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reply")
    }
}

impl Reply {
    pub fn send(&self, yes: bool) {
        if let Some(back) = self.0.lock().ok().and_then(|mut b| b.take()) {
            let _ = back.send(yes);
        }
    }
}

/// What heads the agent's answer wherever it is drawn: the words are its, not TorqOS's (the review of the rebased
/// branch, B13).
pub const AGENTS_WORDS: &str = "Your agent wrote:";

/// Decline proposal `seq`: `request system decline <n>`, run as TorqOS Settings runs its lines (torqcli's
/// session). torqcli shows the proposal and asks first: its question goes to the program as `asked`, and the
/// person's answer back through the `Reply`; what it said at the end as `done`.
pub fn decline<M: Send + 'static>(seq: u64, asked: fn(u64, String, Reply) -> M, done: fn(u64, Result<String, String>) -> M) -> Task<cosmic::Action<M>> {
    Task::stream(cosmic::iced::stream::channel(4, async move |mut out: Sender<M>| {
        let ui = out.clone();
        let ask = move |question: String| {
            let mut ui = ui.clone();
            let (back, answer) = std::sync::mpsc::channel();
            let sent = cosmic::iced::futures::executor::block_on(ui.send(asked(seq, question, Reply(std::sync::Arc::new(std::sync::Mutex::new(Some(back)))))));
            sent.is_ok() && answer.recv().unwrap_or(false)
        };
        let line = torq::app::decline_line(seq);
        let got = off(move || torq::app::Console::start().act(torq::app::At::Read, &line, ask)).await.and_then(|r| r);
        let _ = out.send(done(seq, got)).await;
    }))
    .map(cosmic::Action::App)
}
