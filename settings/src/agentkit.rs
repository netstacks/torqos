//! What the agent's applet and its window share (ADR-051 4): its mark, drawn, and a question asked
//! through torqcli's client (`torq::agent::client`), its answer streamed into the program's messages
//! off its drawing thread.

use cosmic::cosmic_theme::palette::Srgba;
use cosmic::iced::futures::channel::mpsc::Sender;
use cosmic::iced::futures::SinkExt as _;
use cosmic::prelude::*;
use cosmic::widget::icon;
use torq::agent::client::Client;
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

/// Ask `question`: each event of the answer as `heard(e)`, or why there is none as `lost(why)`; on a thread
/// of its own, which ends the ask (and so the task) if the program stops listening.
pub fn ask<M: Send + 'static>(client: Client, question: String, heard: fn(Event) -> M, lost: fn(String) -> M) -> Task<cosmic::Action<M>> {
    Task::stream(cosmic::iced::stream::channel(16, async move |out: Sender<M>| {
        let (done, over) = cosmic::iced::futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            let mut out = out;
            let mut tell = |m: M| cosmic::iced::futures::executor::block_on(out.send(m)).is_ok();
            match client.ask(&question, None) {
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
