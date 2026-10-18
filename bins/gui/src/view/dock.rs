//! The dock: the topology's problems, and its source.

use iced::widget::{
    button, column, container, pick_list, rich_text, row, scrollable, space, span, text, text_input,
};
use iced::{Alignment, Element, Font, Length, Theme};

use super::muted;
use crate::doc::Severity;
use crate::live::Tone;
use crate::message::Message;
use crate::message::{Control, SessionMsg};
use crate::model::{App, Design, DockTab, Live, Tab};

pub fn view<'a>(
    app: &'a App,
    tab: &'a Tab,
    terminals: Option<&'a crate::term::Terminals>,
) -> Element<'a, Message> {
    match (tab.dock, tab.design(), tab.live()) {
        (DockTab::Problems, Some(d), _) => problems(d),
        (DockTab::Source, Some(d), _) => source(d),
        (DockTab::Processes, _, Some(l)) => processes(tab, l, app.operator()),
        (DockTab::Console, _, Some(l)) => console(tab, l, app.operator()),
        (DockTab::Terminal, _, Some(l)) => terminal(l, app.operator(), terminals),
        (DockTab::Jobs, _, _) => jobs(app),
        (_, _, Some(l)) => events(l),
        _ => container(muted("Nothing here.")).padding(10).into(),
    }
}

fn problems(tab: &Design) -> Element<'_, Message> {
    if tab.problems.is_empty() {
        return container(muted(if tab.loading {
            "Checking…"
        } else {
            "No problems: the topology parses and validates."
        }))
        .padding(10)
        .into();
    }
    let list = tab
        .problems
        .iter()
        .enumerate()
        .fold(column![].spacing(2), |col, (i, p)| {
            let severity = p.severity;
            let mark = text("●").size(12).style(super::tone_text(match severity {
                Severity::Error => nlink_lab_view::scene::Tone::Bad,
                Severity::Warning => nlink_lab_view::scene::Tone::Warn,
            }));
            let mut detail = vec![format!("[{}]", p.rule)];
            if let Some(loc) = &p.location {
                detail.push(loc.clone());
            }
            let mut body = column![text(&p.message).size(13), muted(detail.join("  "))].spacing(2);
            if let Some(hint) = &p.hint {
                body = body.push(muted(format!("hint: {hint}")));
            }
            let line = row![mark, body].spacing(8).align_y(Alignment::Start);
            // a disabled button dims its text: one that leads nowhere is
            // a plain row
            let row: Element<'_, Message> = if p.target().is_some() || p.line().is_some() {
                button(line)
                    .style(button::text)
                    .width(Length::Fill)
                    .on_press(Message::ProblemClicked(i))
                    .into()
            } else {
                container(line).padding([5, 10]).width(Length::Fill).into()
            };
            col.push(row)
        });
    scrollable(container(list).padding(6)).into()
}

fn source(tab: &Design) -> Element<'_, Message> {
    if tab.text.is_empty() {
        return container(muted("Nothing read yet.")).padding(10).into();
    }
    // the lines parse errors point at, in a red that reads on both themes
    const MARK: iced::Color = iced::Color::from_rgb(0.87, 0.28, 0.30);
    let marked: Vec<usize> = tab.problems.iter().filter_map(|p| p.line()).collect();
    let mut numbers = Vec::new();
    let mut code = Vec::new();
    for (i, line) in tab.text.lines().enumerate() {
        let n = i + 1;
        let mut num: text::Span<'_, ()> = span(format!("{n}\n"));
        let mut src: text::Span<'_, ()> = span(format!("{line}\n"));
        if marked.contains(&n) {
            num = num.color(MARK).underline(true);
            src = src.color(MARK).underline(true);
        }
        numbers.push(num);
        code.push(src);
    }
    scrollable(
        row![
            rich_text(numbers).font(Font::MONOSPACE).size(12),
            rich_text(code).font(Font::MONOSPACE).size(12),
        ]
        .spacing(12)
        .padding(8),
    )
    .direction(scrollable::Direction::Both {
        vertical: scrollable::Scrollbar::default(),
        horizontal: scrollable::Scrollbar::default(),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The lab's events, newest first.
fn events(l: &Live) -> Element<'_, Message> {
    if l.events.is_empty() {
        return container(muted("No event yet.")).padding(10).into();
    }
    let list = l.events.iter().rev().fold(column![].spacing(2), |col, e| {
        let tone = e.tone;
        col.push(
            row![
                muted(e.time.as_str())
                    .font(Font::MONOSPACE)
                    .width(Length::Fixed(64.0)),
                text(e.kind.as_str())
                    .size(13)
                    .width(Length::Fixed(130.0))
                    .style(move |theme: &Theme| {
                        use nlink_lab_view::scene::Tone as T;
                        let p = crate::palette::Palette::of(theme);
                        text::Style {
                            color: match tone {
                                Tone::Info => None,
                                Tone::Good => Some(p.tone(T::Good)),
                                Tone::Warn => Some(p.tone(T::Warn)),
                                Tone::Bad => Some(p.tone(T::Bad)),
                            },
                        }
                    }),
                text(e.text.as_str()).size(13),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
    });
    scrollable(container(list).padding(8)).into()
}

/// The lab's tracked processes; an operator starts and stops them.
fn processes<'a>(tab: &'a Tab, l: &'a Live, operator: bool) -> Element<'a, Message> {
    let ok = operator && l.pending == 0 && !l.gone;
    let mut top = column![].spacing(8);
    if operator && !l.gone {
        let mut nodes: Vec<String> = tab
            .topology
            .as_ref()
            .map(|t| t.nodes.keys().cloned().collect())
            .unwrap_or_default();
        nodes.sort_by(|a, b| nlink_lab_view::layout::natural(a, b));
        top = top.push(
            row![
                pick_list(nodes, l.spawn_form.node.clone(), |v| {
                    Message::Control(Control::SpawnNode(v))
                })
                .placeholder("node")
                .text_size(13),
                text_input("a command, e.g. iperf3 -s", &l.spawn_form.command)
                    .size(13)
                    .on_input(|v| Message::Control(Control::SpawnCommand(v)))
                    .on_submit(Message::Control(Control::Spawn)),
                button(text("Start").size(13))
                    .style(button::primary)
                    .on_press_maybe(ok.then_some(Message::Control(Control::Spawn))),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    }
    if l.processes.is_empty() {
        return container(top.push(muted("No tracked process yet.")))
            .padding(10)
            .into();
    }
    let head = row![
        muted("Node").width(Length::Fixed(160.0)),
        muted("PID").width(Length::Fixed(80.0)),
        muted("Status").width(Length::Fill),
    ]
    .spacing(8);
    let list = l.processes.iter().fold(top.push(head), |col, p| {
        let status = match (p.alive, p.exit_code) {
            (true, _) => "running".to_string(),
            (false, Some(c)) => format!("exited, status {c}"),
            (false, None) => "exited".to_string(),
        };
        col.push(
            row![
                text(p.node.as_str()).size(13).width(Length::Fixed(160.0)),
                text(p.pid.to_string())
                    .size(13)
                    .font(Font::MONOSPACE)
                    .width(Length::Fixed(80.0)),
                text(status).size(13).width(Length::Fill),
                operator.then(|| {
                    button(text("Log").size(12))
                        .style(button::text)
                        .padding([2, 8])
                        .on_press(Message::Session(SessionMsg::Logs(p.pid)))
                }),
                (operator && p.alive).then(|| {
                    button(text("Stop").size(12))
                        .style(button::secondary)
                        .padding([2, 8])
                        .on_press_maybe(ok.then_some(Message::Control(Control::Kill(p.pid))))
                }),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
    });
    scrollable(container(list).padding(8)).into()
}

fn s(m: SessionMsg) -> Message {
    Message::Session(m)
}

/// The console: a command line, its runs and the logs followed as chips,
/// the chosen one's output.
fn console<'a>(tab: &'a Tab, l: &'a Live, operator: bool) -> Element<'a, Message> {
    use crate::session::{State, Stream};
    let mut col = column![].spacing(8).padding(8);
    if operator && !l.gone {
        let mut nodes: Vec<String> = tab
            .topology
            .as_ref()
            .map(|t| t.nodes.keys().cloned().collect())
            .unwrap_or_default();
        nodes.sort_by(|a, b| nlink_lab_view::layout::natural(a, b));
        let node = l.console.node.clone().or_else(|| nodes.first().cloned());
        col = col.push(
            row![
                pick_list(nodes, node, |v| s(SessionMsg::ConsoleNode(v)))
                    .placeholder("node")
                    .text_size(13),
                text_input("a command, e.g. ping -c 3 10.0.0.2", &l.console.command)
                    .size(13)
                    .font(Font::MONOSPACE)
                    .on_input(|v| s(SessionMsg::ConsoleCommand(v)))
                    .on_submit(s(SessionMsg::Run)),
                button(text("Run").size(13))
                    .style(button::primary)
                    .on_press(s(SessionMsg::Run)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
    } else if !operator {
        col = col.push(muted("Read-only: commands are an operator's."));
    }
    if l.sessions.is_empty() {
        return col
            .push(muted(
                "Commands run here, logs followed from Processes, captures from an interface.",
            ))
            .into();
    }
    let chips = l.sessions.iter().fold(row![].spacing(4), |r, x| {
        let active = l.active_session == Some(x.id);
        let label = if x.running() {
            format!("● {}", x.title())
        } else {
            x.title()
        };
        r.push(
            row![
                button(text(label).size(12))
                    .style(if active {
                        button::secondary
                    } else {
                        button::text
                    })
                    .padding([2, 8])
                    .on_press(s(SessionMsg::Select(x.id))),
                button(text("×").size(12))
                    .style(button::text)
                    .padding([2, 4])
                    .on_press(s(SessionMsg::Close(x.id))),
            ]
            .align_y(Alignment::Center),
        )
    });
    col = col.push(
        scrollable(chips).direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::default(),
        )),
    );
    let Some(x) = l
        .active_session
        .and_then(|id| l.sessions.iter().find(|x| x.id == id))
    else {
        return col.into();
    };
    // the output, coloured by stream, kept at its end as it grows
    const ERR: iced::Color = iced::Color::from_rgb(0.87, 0.28, 0.30);
    const NOTE: iced::Color = iced::Color::from_rgb(0.45, 0.55, 0.65);
    let spans: Vec<text::Span<'_, ()>> = x
        .shown()
        .map(|(stream, line)| {
            let sp = span(format!("{line}\n"));
            match stream {
                Stream::Out => sp,
                Stream::Err => sp.color(ERR),
                Stream::Note => sp.color(NOTE),
            }
        })
        .collect();
    let footer = match &x.state {
        State::Running => row![
            muted(if x.bytes > 0 {
                format!("running · {} bytes", x.bytes)
            } else {
                "running".to_string()
            }),
            space::horizontal(),
            button(text("Stop").size(12))
                .style(button::secondary)
                .padding([2, 8])
                .on_press(s(SessionMsg::Stop(x.id))),
        ],
        State::Ended { how, .. } => row![muted(how.as_str())],
    };
    col.push(
        scrollable(rich_text(spans).font(Font::MONOSPACE).size(12))
            .anchor_bottom()
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .push(footer.align_y(Alignment::Center))
    .into()
}

/// The lab's terminals: a tab each, the chosen one's widget.
fn terminal<'a>(
    l: &'a Live,
    operator: bool,
    terminals: Option<&'a crate::term::Terminals>,
) -> Element<'a, Message> {
    if !operator {
        return container(muted("Read-only: shells are an operator's."))
            .padding(10)
            .into();
    }
    if l.terminals.is_empty() {
        return container(muted(
            "Open a shell on a node from the inspector: select it, then Terminal.",
        ))
        .padding(10)
        .into();
    }
    let chips = l.terminals.iter().fold(row![].spacing(4), |r, t| {
        let active = l.active_terminal == Some(t.id);
        let label = if t.ended {
            format!("{} (ended)", t.title)
        } else {
            t.title.clone()
        };
        r.push(
            row![
                button(text(label).size(12))
                    .style(if active {
                        button::secondary
                    } else {
                        button::text
                    })
                    .padding([2, 8])
                    .on_press(s(SessionMsg::SelectTerminal(t.id))),
                button(text("×").size(12))
                    .style(button::text)
                    .padding([2, 4])
                    .on_press(s(SessionMsg::CloseTerminal(t.id))),
            ]
            .align_y(Alignment::Center),
        )
    });
    let widget: Element<'a, Message> = match (l.active_terminal, terminals) {
        (Some(id), Some(host)) if crate::term::Terminals::EMBEDDED => host
            .view(id)
            .unwrap_or_else(|| muted("This terminal has closed.").into()),
        _ => muted("The shell opened in the desktop's terminal.").into(),
    };
    column![chips, widget].spacing(6).padding(6).into()
}

/// The long operations: each one's progress, the chosen one's account,
/// and the daemon's other operations to follow.
fn jobs(app: &App) -> Element<'_, Message> {
    use crate::job::JobState;
    use crate::message::WorkflowMsg as W;
    let w = Message::Workflow;
    let mut col = column![].spacing(6).padding(8);
    if app.jobs.is_empty() && app.others.is_empty() {
        return col
            .push(muted(
                "Deploys, applies, scenarios, benchmarks and assertions run here.",
            ))
            .into();
    }
    for j in app.jobs.iter().rev() {
        let active = app.active_job == Some(j.id);
        let status = match &j.state {
            JobState::Running => {
                let mut s = j
                    .stage
                    .as_ref()
                    .map(|(name, i, of)| format!("{name} ({}/{of})", i + 1))
                    .unwrap_or_else(|| "starting".into());
                if let Some(wait) = &j.waiting {
                    s = format!("{s} · {wait}");
                }
                s
            }
            JobState::Done { text, .. } => text.clone(),
        };
        col = col.push(
            row![
                button(text(j.title()).size(13))
                    .style(if active {
                        button::secondary
                    } else {
                        button::text
                    })
                    .padding([2, 8])
                    .width(Length::Fixed(220.0))
                    .on_press(w(W::Select(j.id))),
                iced::widget::progress_bar(0.0..=1.0, j.progress())
                    .girth(8)
                    .length(Length::Fixed(160.0)),
                text(status).size(12).width(Length::Fill),
                if j.running() {
                    button(text("Cancel").size(12))
                        .style(button::secondary)
                        .padding([2, 8])
                        .on_press(w(W::Cancel(j.id)))
                } else {
                    button(text("×").size(12))
                        .style(button::text)
                        .padding([2, 6])
                        .on_press(w(W::Forget(j.id)))
                },
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
    }
    for o in &app.others {
        col = col.push(
            row![
                text(format!("{} of {} (by {})", o.kind, o.lab, o.actor))
                    .size(13)
                    .width(Length::Fill),
                button(text("Follow").size(12))
                    .style(button::secondary)
                    .padding([2, 8])
                    .on_press(w(W::Follow(o.op.clone()))),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        );
    }
    if let Some(j) = app
        .active_job
        .and_then(|id| app.jobs.iter().find(|j| j.id == id))
    {
        let lines = j.lines.iter().fold(
            column![].spacing(2).width(Length::Fill),
            |c, (tone, line)| {
                use crate::live::Tone as T;
                use nlink_lab_view::scene::Tone;
                let tone = match tone {
                    T::Info => Tone::Normal,
                    T::Good => Tone::Good,
                    T::Warn => Tone::Warn,
                    T::Bad => Tone::Bad,
                };
                c.push(
                    text(line.as_str())
                        .size(12)
                        .font(Font::MONOSPACE)
                        .style(super::tone_text(tone)),
                )
            },
        );
        col = col.push(iced::widget::rule::horizontal(1)).push(
            scrollable(lines)
                .anchor_bottom()
                .width(Length::Fill)
                .height(Length::Fill),
        );
    }
    col.into()
}
