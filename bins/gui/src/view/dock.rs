//! The dock: the topology's problems, and its source.

use iced::widget::{
    button, column, container, pick_list, rich_text, row, scrollable, span, text, text_input,
};
use iced::{Alignment, Element, Font, Length, Theme};

use super::muted;
use crate::doc::Severity;
use crate::live::Tone;
use crate::message::Control;
use crate::message::Message;
use crate::model::{App, Design, DockTab, Live, Tab};

pub fn view<'a>(app: &'a App, tab: &'a Tab) -> Element<'a, Message> {
    match (tab.dock, tab.design(), tab.live()) {
        (DockTab::Problems, Some(d), _) => problems(d),
        (DockTab::Source, Some(d), _) => source(d),
        (DockTab::Processes, _, Some(l)) => processes(tab, l, app.operator()),
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
