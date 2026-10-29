//! Home: open a topology, a recent one, or start from a template.

use iced::widget::{button, column, container, pick_list, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Theme};

use super::{heading, muted};
use crate::message::Message;
use crate::model::App;
use crate::settings::ThemePref;

pub fn view(app: &App) -> Element<'_, Message> {
    let open = column![
        text("nlink-lab").size(28),
        muted(concat!(
            "Network labs from topology files · ",
            env!("CARGO_PKG_VERSION"),
            " · experimental: may change in any minor release"
        )),
        row![
            button(text("Open a topology…"))
                .style(button::primary)
                .padding([8, 16])
                .on_press(Message::OpenDialog),
            muted("or from a terminal: nlink-lab-gui FILE.nll"),
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    ]
    .spacing(10);

    let recent: Element<'_, Message> = if app.settings.recent.is_empty() {
        muted("Topologies you open are listed here.").into()
    } else {
        app.settings
            .recent
            .iter()
            .fold(column![].spacing(2), |col, path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let dir = path
                    .parent()
                    .map(|d| d.display().to_string())
                    .unwrap_or_default();
                let exists = path.exists();
                let label = row![
                    text(name).size(14),
                    muted(if exists {
                        dir
                    } else {
                        format!("{dir} (missing)")
                    }),
                ]
                .spacing(10)
                .align_y(Alignment::Center);
                col.push(
                    row![
                        button(label)
                            .style(button::text)
                            .width(Length::Fill)
                            .on_press_maybe(exists.then(|| Message::Open(path.clone()))),
                        button(text("×").size(13))
                            .style(button::text)
                            .on_press(Message::ForgetRecent(path.clone())),
                    ]
                    .align_y(Alignment::Center),
                )
            })
            .into()
    };

    let templates =
        nlink_lab::templates::list()
            .chunks(3)
            .fold(column![].spacing(10), |col, chunk| {
                col.push(chunk.iter().fold(row![].spacing(10), |r, t| {
                    r.push(
                        button(
                            column![
                                text(t.name).size(14),
                                muted(t.description),
                                muted(format!("{} nodes · {} links", t.node_count, t.link_count)),
                            ]
                            .spacing(4),
                        )
                        .style(card)
                        .padding(12)
                        .width(Length::FillPortion(1))
                        .on_press(Message::OpenTemplate(t.name)),
                    )
                }))
            });

    let appearance = row![
        text("Theme").size(13),
        pick_list(ThemePref::ALL, Some(app.settings.theme), Message::SetTheme).text_size(13),
        space::horizontal(),
        muted("F1: shortcuts"),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    let page = column![
        open,
        heading("Running labs"),
        labs(app),
        heading("Recent"),
        recent,
        heading("Start from a template"),
        templates,
        appearance,
    ]
    .spacing(18)
    .padding(32)
    .max_width(980);

    scrollable(container(page).center_x(Length::Fill))
        .height(Length::Fill)
        .into()
}

/// A template's card: a light box that darkens under the mouse.
fn card(theme: &Theme, status: button::Status) -> button::Style {
    let p = theme.extended_palette();
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => p.background.weak.color,
        _ => p.background.base.color,
    };
    button::Style {
        background: Some(background.into()),
        text_color: p.background.base.text,
        border: iced::Border {
            width: 1.0,
            color: p.background.strong.color,
            radius: 6.0.into(),
        },
        ..button::Style::default()
    }
}

/// The labs the daemon serves, and how the connection to it goes.
fn labs(app: &App) -> Element<'_, Message> {
    use crate::model::Conn;
    let target = app
        .target
        .as_ref()
        .map(|t| t.to_string())
        .unwrap_or_default();
    match &app.conn {
        Conn::Off => muted("Started with --offline: topologies from their files only.").into(),
        Conn::Connecting { .. } => muted(format!("Connecting to {target}…")).into(),
        Conn::Down { reason, .. } => column![
            row![
                text(format!("No daemon answers at {target}")).size(13),
                button(text("Retry").size(13))
                    .style(button::secondary)
                    .on_press(Message::Reconnect),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            muted(reason.as_str()),
            muted(
                "Start it with `sudo systemctl enable --now nlink-labd.socket`, \
                 or look around with `nlink-lab-gui --demo`."
            ),
        ]
        .spacing(4)
        .into(),
        Conn::Up { who, backend, .. } => {
            let about = if backend.is_demo() {
                "The demo: made-up labs, made-up metrics.".to_string()
            } else {
                format!(
                    "{} on {}, as {} ({})",
                    who.server,
                    who.host,
                    who.user,
                    crate::backend::role_name(who.role)
                )
            };
            let mut col = column![muted(about)].spacing(2);
            if app.labs.is_empty() {
                return col
                    .push(muted(
                        "No lab is running: deploy one with `sudo nlink-lab deploy FILE`.",
                    ))
                    .into();
            }
            let cell = |s: String, w: f32| text(s).size(13).width(Length::Fixed(w));
            col = col.push(
                row![
                    muted("Lab").width(Length::Fill),
                    muted("Nodes").width(Length::Fixed(60.0)),
                    muted("Links").width(Length::Fixed(60.0)),
                    muted("Impaired").width(Length::Fixed(80.0)),
                    muted("Processes").width(Length::Fixed(80.0)),
                    muted("Health").width(Length::Fixed(80.0)),
                ]
                .spacing(8)
                .padding([0, 8]),
            );
            for l in &app.labs {
                let impaired = if l.partitioned > 0 {
                    format!("{} +{} cut", l.impaired, l.partitioned)
                } else {
                    l.impaired.to_string()
                };
                col = col.push(
                    button(
                        row![
                            text(&l.lab).size(14).width(Length::Fill),
                            cell(l.nodes.to_string(), 60.0),
                            cell(l.links.to_string(), 60.0),
                            cell(impaired, 80.0),
                            cell(l.processes.to_string(), 80.0),
                            cell(l.health.clone(), 80.0),
                        ]
                        .spacing(8),
                    )
                    .style(button::text)
                    .width(Length::Fill)
                    .on_press(Message::OpenLab(l.lab.clone())),
                );
            }
            col.into()
        }
    }
}
