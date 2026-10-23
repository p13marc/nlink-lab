//! What the app looks like: a function of its state.

mod controls;
mod dock;
mod home;
mod inspector;
mod workspace;

use iced::widget::{
    button, column, container, mouse_area, opaque, row, scrollable, space, stack, text, text_input,
};
use iced::{Alignment, Color, Element, Length, Theme};

use crate::message::{Message, PaletteMsg, Shortcut};
use crate::model::{App, Discard, Level, Route};

/// The source editor's id, to give it the keyboard.
pub const EDITOR: &str = "source-editor";
/// The palette's field's.
pub const PALETTE: &str = "palette";

/// The app; `terminals` are the terminal widgets, where there are any.
pub fn view<'a>(
    app: &'a App,
    terminals: Option<&'a crate::term::Terminals>,
) -> Element<'a, Message> {
    let body: Element<'a, Message> = match (app.route, app.current()) {
        (Route::Workspace, Some(tab)) => workspace::view(app, tab, terminals),
        _ => home::view(app),
    };
    let mut layers = stack![column![top_bar(app), body]]
        .width(Length::Fill)
        .height(Length::Fill);
    if !app.toasts.is_empty() {
        layers = layers.push(toasts(app));
    }
    if let Some(p) = &app.panel {
        layers = layers.push(panel(app, p));
    }
    if let Some(c) = &app.confirm {
        layers = layers.push(confirm(c));
    }
    if let Some(d) = app.discard {
        layers = layers.push(discard(app, d));
    }
    if app.help {
        layers = layers.push(help());
    }
    if let Some(p) = &app.palette {
        layers = layers.push(palette(app, p));
    }
    layers.into()
}

/// The rest of the app dimmed under `card`; a click beside it sends
/// `outside`.
fn modal<'a>(
    card: impl Into<Element<'a, Message>>,
    outside: Option<Message>,
    top: bool,
) -> Element<'a, Message> {
    let mut place = container(opaque(card))
        .width(Length::Fill)
        .height(Length::Fill);
    place = if top {
        place.align_x(Alignment::Center).padding(iced::Padding {
            top: 70.0,
            ..iced::Padding::ZERO
        })
    } else {
        place.center(Length::Fill)
    };
    let dim = place.style(|_| container::Style {
        background: Some(
            Color {
                a: 0.45,
                ..Color::BLACK
            }
            .into(),
        ),
        ..container::Style::default()
    });
    match outside {
        Some(m) => opaque(mouse_area(dim).on_press(m)),
        None => opaque(dim),
    }
}

/// Every command by name (Ctrl+K), filtered by what is typed.
fn palette<'a>(app: &'a App, p: &'a crate::model::Palette) -> Element<'a, Message> {
    /// Entries shown at once.
    const SHOWN: usize = 12;
    let entries = crate::commands::entries(app);
    let picked = crate::commands::filter(&entries, &p.query);
    let first = p.selected.saturating_sub(SHOWN - 1);
    let mut list = column![].spacing(1);
    for (i, &e) in picked.iter().enumerate().skip(first).take(SHOWN) {
        let entry = &entries[e];
        list = list.push(
            button(
                row![
                    text(entry.label.clone()).size(13).width(Length::Fill),
                    muted(entry.keys.unwrap_or("")),
                ]
                .spacing(8),
            )
            .style(if i == p.selected {
                button::primary
            } else {
                button::text
            })
            .width(Length::Fill)
            .padding([4, 10])
            .on_press(Message::Palette(PaletteMsg::Run(Some(i)))),
        );
    }
    if picked.is_empty() {
        list = list.push(container(muted("Nothing matches.")).padding([4, 10]));
    } else if picked.len() > SHOWN {
        list = list.push(
            container(muted(format!(
                "{} more: type to narrow",
                picked.len() - SHOWN
            )))
            .padding([4, 10]),
        );
    }
    let card = container(
        column![
            text_input("A command, a node, a file…", &p.query)
                .id(PALETTE)
                .on_input(|q| Message::Palette(PaletteMsg::Query(q)))
                .on_submit(Message::Palette(PaletteMsg::Run(None)))
                .size(15)
                .padding(8),
            list,
        ]
        .spacing(8),
    )
    .padding(10)
    .style(container::bordered_box)
    .max_width(620);
    modal(card, Some(Message::Palette(PaletteMsg::Close)), true)
}

/// Unsaved edits about to be lost: closing their tab, or quitting.
fn discard(app: &App, d: Discard) -> Element<'_, Message> {
    let (title, body, go) = match d {
        Discard::Tab(id) => (
            format!(
                "Close {} without saving?",
                app.tab(id).map(|t| t.title()).unwrap_or_default()
            ),
            "Its edits are lost.".to_string(),
            "Close without saving",
        ),
        Discard::Quit => {
            let n = app
                .tabs
                .iter()
                .filter_map(|t| t.design())
                .filter(|d| d.edited)
                .count();
            (
                "Quit without saving?".to_string(),
                format!("{n} tab(s) have edits not saved; they are lost."),
                "Quit without saving",
            )
        }
    };
    let card = container(
        column![
            text(title).size(18),
            text(body).size(13),
            row![
                space::horizontal(),
                button(text("Cancel").size(13))
                    .style(button::secondary)
                    .on_press(Message::Discard(false)),
                button(text(go).size(13))
                    .style(button::danger)
                    .on_press(Message::Discard(true)),
            ]
            .spacing(8),
        ]
        .spacing(14),
    )
    .padding(20)
    .style(container::bordered_box)
    .max_width(520);
    modal(card, Some(Message::Discard(false)), false)
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
        if t.design().is_some_and(|d| d.edited) {
            label.push_str(" *");
        }
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
                    t.action.as_ref().map(|(label, msg)| {
                        button(text(label.as_str()).size(13))
                            .style(button::secondary)
                            .on_press((**msg).clone())
                    }),
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

/// A change that asks first: what it does, the lab's name to type for a
/// destroy, Confirm and Cancel.
fn confirm(c: &crate::control::Confirm) -> Element<'_, Message> {
    use crate::control::ConfirmWhat;
    use crate::message::Control;
    let lab = c.lab.as_str();
    let (title, body): (String, Element<'_, Message>) = match &c.what {
        ConfirmWhat::Destroy => (
            format!("Destroy {lab}?"),
            column![
                text("Its nodes, links, processes and containers go, for everyone.").size(13),
                text_input(lab, &c.typed)
                    .on_input(|v| Message::Control(Control::Typed(v)))
                    .on_submit(Message::Control(Control::Confirm))
                    .size(13),
                muted("Type the lab's name to confirm."),
            ]
            .spacing(8)
            .into(),
        ),
        ConfirmWhat::Restore { name, plan } => (
            format!("Restore {lab} to {name}?"),
            scrollable(text(plan.as_str()).size(12).font(iced::Font::MONOSPACE))
                .height(Length::Fixed(260.0))
                .into(),
        ),
        ConfirmWhat::DeleteSnapshot { name } => (
            format!("Delete the snapshot {name}?"),
            text("It cannot be restored afterwards.").size(13).into(),
        ),
    };
    let card = container(
        column![
            text(title).size(18),
            body,
            row![
                space::horizontal(),
                button(text("Cancel").size(13))
                    .style(button::secondary)
                    .on_press(Message::Control(Control::Cancel)),
                button(text("Confirm").size(13))
                    .style(button::danger)
                    .on_press_maybe(c.ready().then_some(Message::Control(Control::Confirm))),
            ]
            .spacing(8),
        ]
        .spacing(14),
    )
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
        .on_press(Message::Control(Control::Cancel)),
    )
}

/// A deploy or an apply being prepared: its options, its plan or diff,
/// and the button that runs it.
fn panel<'a>(app: &'a App, p: &'a crate::model::Panel) -> Element<'a, Message> {
    use crate::message::WorkflowMsg as W;
    use crate::model::{Panel, Preview};
    let w = Message::Workflow;
    let tab = app.tab(p.tab());
    let file = tab
        .and_then(|t| t.design())
        .map(|d| d.source.title())
        .unwrap_or_default();
    let loaded = tab.is_some_and(|t| t.topology.is_some());
    let (title, options, preview_label, go_label): (String, Element<'a, Message>, &str, &str) =
        match p {
            Panel::Deploy { options, .. } => {
                let lab = tab
                    .and_then(|t| t.topology.as_ref())
                    .map(|t| t.lab.name.clone())
                    .unwrap_or_default();
                (
                    format!(
                        "Deploy {}",
                        crate::model::deployed_name(&lab, options.suffix.as_deref())
                    ),
                    column![
                        row![
                            muted("suffix").width(Length::Fixed(96.0)),
                            text_input(
                                "none: the lab's own name",
                                options.suffix.as_deref().unwrap_or("")
                            )
                            .size(13)
                            .on_input(move |v| w(W::Suffix(v))),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center),
                        iced::widget::checkbox(options.force)
                            .label("Replace a lab of that name")
                            .text_size(13)
                            .on_toggle(move |v| w(W::Force(v))),
                        iced::widget::checkbox(options.skip_assertions)
                            .label("Skip the validate assertions")
                            .text_size(13)
                            .on_toggle(move |v| w(W::SkipAssertions(v))),
                        iced::widget::checkbox(options.strict)
                            .label("A failed assertion fails the deploy")
                            .text_size(13)
                            .on_toggle(move |v| w(W::Strict(v))),
                    ]
                    .spacing(8)
                    .into(),
                    "Plan",
                    "Deploy",
                )
            }
            Panel::Apply { lab, options, .. } => (
                format!("Apply {file} to {lab}"),
                column![
                    iced::widget::checkbox(options.reset_impairments)
                        .label("Drop the impairments and partitions set live")
                        .text_size(13)
                        .on_toggle(move |v| w(W::ResetImpairments(v))),
                    iced::widget::checkbox(options.strict)
                        .label("A failed assertion fails the apply")
                        .text_size(13)
                        .on_toggle(move |v| w(W::Strict(v))),
                    iced::widget::checkbox(options.keep_conntrack)
                        .label("Keep the flows of changed NAT rules on their old translation")
                        .text_size(13)
                        .on_toggle(move |v| w(W::KeepConntrack(v))),
                ]
                .spacing(8)
                .into(),
                "Show the changes",
                "Apply",
            ),
        };
    let preview: Element<'a, Message> = match match p {
        Panel::Deploy { preview, .. } | Panel::Apply { preview, .. } => preview,
    } {
        Preview::None => muted("Look before you leap: the plan, or what would change.").into(),
        Preview::Asked => muted("Asking the daemon…").into(),
        Preview::Got(Ok(text)) => scrollable(text_mono(text))
            .height(Length::Fixed(220.0))
            .into(),
        Preview::Got(Err(e)) => text(e.as_str())
            .size(13)
            .style(tone_text(nlink_lab_view::scene::Tone::Bad))
            .into(),
    };
    let card = container(
        column![
            text(title).size(18),
            options,
            preview,
            row![
                space::horizontal(),
                button(text("Cancel").size(13))
                    .style(button::secondary)
                    .on_press(w(W::ClosePanel)),
                button(text(preview_label).size(13))
                    .style(button::secondary)
                    .on_press_maybe(loaded.then_some(w(W::Preview))),
                button(text(go_label).size(13))
                    .style(button::primary)
                    .on_press_maybe(loaded.then_some(w(W::Go))),
            ]
            .spacing(8),
        ]
        .spacing(14),
    )
    .padding(20)
    .style(container::bordered_box)
    .max_width(620);
    opaque(container(opaque(card)).center(Length::Fill).style(|_| {
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
}

fn text_mono(s: &str) -> iced::widget::Text<'_> {
    text(s).size(12).font(iced::Font::MONOSPACE)
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
