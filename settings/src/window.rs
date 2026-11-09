//! The window: its pages, each one torqcli command's output, laid out as COSMIC lays out settings.

use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget::{self, icon, nav_bar, settings};
use torq::app::Machine;
use torq::history::Row;

/// How many changes History shows: the latest, as `show system ledger` does.
const HISTORY: usize = 200;

/// The pages, each the torqcli command whose output it shows.
#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    History,
}

impl Page {
    /// The command a person would type for what the page shows.
    fn command(self) -> &'static str {
        match self {
            Page::Overview => "torqcli show system alarms",
            Page::History => "torqcli show system ledger",
        }
    }
}

/// What a page has to show: not yet asked, the broker's answer, or why there is none.
enum Loaded<T> {
    Waiting,
    Got(T),
    Failed(String),
}

pub struct Settings {
    core: cosmic::Core,
    nav: nav_bar::Model,
    machine: Loaded<Machine>,
    history: Loaded<Vec<Row>>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Machine(Result<Machine, String>),
    History(Result<Vec<Row>, String>),
}


impl cosmic::Application for Settings {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.Settings";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: ()) -> (Self, Task<cosmic::Action<Message>>) {
        let mut nav = nav_bar::Model::default();
        nav.insert().text("Overview").data(Page::Overview).icon(icon::from_name("computer-symbolic")).activate();
        nav.insert().text("History").data(Page::History).icon(icon::from_name("document-open-recent-symbolic"));
        let mut app = Settings { core, nav, machine: Loaded::Waiting, history: Loaded::Waiting };
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
        Task::none()
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        vec![widget::button::icon(icon::from_name("view-refresh-symbolic")).tooltip("Ask the broker again").on_press(Message::Refresh).into()]
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            // Each a call to the broker's socket: off the window's thread, so it never stops drawing.
            Message::Refresh => Task::batch([
                cosmic::task::future(async {
                    Message::Machine(blocking(torq::app::machine).await)
                }),
                cosmic::task::future(async { Message::History(blocking(|| torq::app::history(HISTORY)).await) }),
            ]),
            Message::Machine(r) => {
                self.machine = match r {
                    Ok(m) => Loaded::Got(m),
                    Err(e) => Loaded::Failed(e),
                };
                Task::none()
            }
            Message::History(r) => {
                self.history = match r {
                    Ok(rows) => Loaded::Got(rows),
                    Err(e) => Loaded::Failed(e),
                };
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let page = self.nav.active_data::<Page>().copied().unwrap_or(Page::Overview);
        let body = match page {
            Page::Overview => self.overview(),
            Page::History => self.history(),
        };
        let spacing = cosmic::theme::spacing();
        let content = widget::column::with_capacity(2)
            .push(body)
            // Every page says which command it is: the window is torqcli's, not a second way in.
            .push(widget::text::caption(format!("The same as: {}", page.command())))
            .spacing(spacing.space_m)
            .max_width(900);
        widget::scrollable(widget::container(content).width(Length::Fill).center_x(Length::Fill).padding(spacing.space_m)).into()
    }
}

impl Settings {
    fn overview(&self) -> Element<'_, Message> {
        let m = match &self.machine {
            Loaded::Waiting => return waiting(),
            Loaded::Failed(why) => return failed(why),
            Loaded::Got(m) => m,
        };
        let (mark, colour, headline) = if m.well {
            ("emblem-ok-symbolic", Colour::Good, "This machine is exactly as its history says")
        } else {
            ("dialog-warning-symbolic", Colour::Warn, "This machine needs a look")
        };
        let spacing = cosmic::theme::spacing();
        let banner = widget::row::with_capacity(2)
            .push(colour.icon(mark, 32))
            .push(
                widget::column::with_capacity(2)
                    .push(widget::text::title3(headline))
                    .push(widget::text::body(format!("TorqOS {}", m.version))),
            )
            .spacing(spacing.space_s)
            .align_y(Alignment::Center);
        // `show system alarms`, line by line, as it prints them.
        let mut alarms = settings::section().title("Alarms");
        for line in m.alarms.lines().map(str::trim).filter(|l| !l.is_empty()) {
            alarms = alarms.add(settings::item_row(vec![widget::text::body(line.to_string()).width(Length::Fill).into()]));
        }
        widget::column::with_capacity(2).push(banner).push(alarms).spacing(spacing.space_l).into()
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
        for (day, list) in days {
            let mut section = settings::section().title(day);
            for r in list {
                section = section.add(change(r));
            }
            page = page.push(section);
        }
        page.push(widget::text::caption("To undo one change: request system revert <change> (its number, like #85).")).into()
    }
}

/// One change: its mark, what happened, who and when, its number.
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
    settings::item_row(vec![
        colour.icon(name, 16),
        widget::column::with_capacity(2)
            .push(widget::text::body(r.text.clone()))
            .push(widget::text::caption(detail))
            .width(Length::Fill)
            .into(),
        widget::text::body(format!("#{}", r.seq)).into(),
    ])
    .spacing(cosmic::theme::spacing().space_s)
    .into()
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

fn waiting() -> Element<'static, Message> {
    widget::text::body("Asking the broker…").into()
}

fn failed(why: &str) -> Element<'_, Message> {
    widget::column::with_capacity(2)
        .push(widget::text::title4("The broker did not answer"))
        .push(widget::text::body(why))
        .spacing(cosmic::theme::spacing().space_xs)
        .into()
}

/// A call to the broker (a blocking socket call) on a thread of its own, so the window keeps drawing;
/// its error as the command line would say it.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f().map_err(|e| format!("{e:#}")));
    });
    rx.await.unwrap_or_else(|_| Err("the call to the broker stopped before it answered".to_string()))
}
