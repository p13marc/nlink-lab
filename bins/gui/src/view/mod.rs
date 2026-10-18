//! What the app looks like: a function of its state.

mod dock;
mod home;
mod inspector;
mod workspace;

use iced::widget::{button, column, container, mouse_area, opaque, row, space, stack, text};
use iced::{Alignment, Color, Element, Length, Theme};

use crate::message::{Message, Shortcut};
use crate::model::{App, Level, Route};

pub fn view(app: &App) -> Element<'_, Message> {
    let body: Element<'_, Message> = match (app.route, app.current()) {
        (Route::Workspace, Some(tab)) => workspace::view(app, tab),
        _ => home::view(app),
    };
    let mut layers = stack![column![top_bar(app), body]]
        .width(Length::Fill)
        .height(Length::Fill);
    if !app.toasts.is_empty() {
        layers = layers.push(toasts(app));
    }
    if app.help {
        layers = layers.push(help());
    }
    layers.into()
}

/// The app's bar: Home, one button per tab, Open, Help.
fn top_bar(app: &App) -> Element<'_, Message> {
    let home = button(text("nlink-lab").size(15))
        .style(if app.route == Route::Home {
            button::primary
        } else {
            button::text
        })
        .on_press(Message::GoHome);
    let mut tabs = row![].spacing(2).align_y(Alignment::Center);
    for t in &app.tabs {
        let active = app.route == Route::Workspace && app.active == Some(t.id);
        let mut label = t.label();
        if t.errors() > 0 {
            label.push_str(" ●");
        }
        tabs = tabs.push(
            row![
                button(text(label).size(13))
                    .style(if active {
                        button::secondary
                    } else {
                        button::text
                    })
                    .on_press(Message::SelectTab(t.id)),
                button(text("×").size(13))
                    .style(button::text)
                    .on_press(Message::CloseTab(t.id)),
            ]
            .align_y(Alignment::Center),
        );
    }
    container(
        row![
            home,
            tabs,
            space::horizontal(),
            connection(app),
            button(text("Open…").size(13))
                .style(button::secondary)
                .on_press(Message::OpenDialog),
            button(text("?").size(13))
                .style(button::text)
                .on_press(Message::Shortcut(Shortcut::Help)),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding([4, 8])
    .width(Length::Fill)
    .style(bar)
    .into()
}

/// The connection to the daemon, in a word and a colour.
fn connection(app: &App) -> Element<'_, Message> {
    use crate::model::Conn;
    use nlink_lab_view::scene::Tone;
    let (word, tone) = match &app.conn {
        Conn::Off => return space().width(Length::Fixed(0.0)).into(),
        Conn::Connecting { .. } => ("connecting…".to_string(), Tone::Warn),
        Conn::Up { who, backend, .. } => (
            if backend.is_demo() {
                "demo".to_string()
            } else {
                format!("{}@{}", who.user, who.host)
            },
            Tone::Good,
        ),
        Conn::Down { .. } => ("no daemon".to_string(), Tone::Bad),
    };
    row![text("●").size(12).style(tone_text(tone)), muted(word)]
        .spacing(4)
        .align_y(Alignment::Center)
        .into()
}

pub fn bar(theme: &Theme) -> container::Style {
    let p = theme.extended_palette();
    container::Style {
        background: Some(p.background.weak.color.into()),
        ..container::Style::default()
    }
}

fn toasts(app: &App) -> Element<'_, Message> {
    let list = app.toasts.iter().fold(column![].spacing(6), |col, t| {
        let level = t.level;
        col.push(
            container(
                row![
                    text(&t.text).size(13).width(Length::Fill),
                    button(text("×").size(13))
                        .style(button::text)
                        .on_press(Message::DismissToast(t.id)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            )
            .padding([6, 10])
            .width(Length::Fixed(380.0))
            .style(move |theme: &Theme| {
                let p = theme.extended_palette();
                let pair = match level {
                    Level::Info => p.background.strong,
                    Level::Error => p.danger.base,
                };
                container::Style {
                    background: Some(pair.color.into()),
                    text_color: Some(pair.text),
                    border: iced::Border {
                        radius: 6.0.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                }
            }),
        )
    });
    container(list)
        .align_right(Length::Fill)
        .align_bottom(Length::Fill)
        .padding(16)
        .into()
}

fn help<'a>() -> Element<'a, Message> {
    let sheet = crate::keys::SHEET.iter().fold(
        column![text("Shortcuts").size(18)].spacing(6),
        |col, (keys, what)| {
            col.push(
                row![
                    text(*keys)
                        .size(13)
                        .width(Length::Fixed(200.0))
                        .font(iced::Font::MONOSPACE),
                    text(*what).size(13),
                ]
                .spacing(12),
            )
        },
    );
    let card = container(sheet)
        .padding(20)
        .style(container::bordered_box)
        .max_width(560);
    opaque(
        mouse_area(container(opaque(card)).center(Length::Fill).style(|_| {
            container::Style {
                background: Some(
                    Color {
                        a: 0.45,
                        ..Color::BLACK
                    }
                    .into(),
                ),
                ..container::Style::default()
            }
        }))
        .on_press(Message::Shortcut(Shortcut::Help)),
    )
}

/// Text in a status tone, readable on either theme (the canvas's
/// colours: iced's own greens are too dark for text on a dark theme).
pub fn tone_text(tone: nlink_lab_view::scene::Tone) -> impl Fn(&Theme) -> text::Style {
    move |theme: &Theme| text::Style {
        color: Some(crate::palette::Palette::of(theme).tone(tone)),
    }
}

/// A section's heading.
pub fn heading<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(s).size(15)
}

/// Secondary text.
pub fn muted<'a>(s: impl text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(s).size(12).style(|theme: &Theme| text::Style {
        color: Some(
            theme
                .extended_palette()
                .background
                .strong
                .text
                .scale_alpha(0.65),
        ),
    })
}

/// A `label  value` line.
pub fn field<'a>(label: &'a str, value: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    row![
        muted(label).width(Length::Fixed(96.0)),
        text(value).size(13),
    ]
    .spacing(8)
    .into()
}
