//! The dock: the topology's problems, and its source.

use iced::widget::{button, column, container, rich_text, row, scrollable, span, text};
use iced::{Alignment, Element, Font, Length, Theme};

use super::muted;
use crate::doc::Severity;
use crate::message::Message;
use crate::model::{DockTab, Tab};

pub fn view(tab: &Tab) -> Element<'_, Message> {
    match tab.dock {
        DockTab::Problems => problems(tab),
        DockTab::Source => source(tab),
    }
}

fn problems(tab: &Tab) -> Element<'_, Message> {
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
            let mark = text("●").size(12).style(move |theme: &Theme| {
                let ext = theme.extended_palette();
                text::Style {
                    color: Some(match severity {
                        Severity::Error => ext.danger.base.color,
                        Severity::Warning => ext.warning.base.color,
                    }),
                }
            });
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

fn source(tab: &Tab) -> Element<'_, Message> {
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
