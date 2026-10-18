//! The controls of a running lab, in the inspector: an endpoint's
//! impairment, a network's pairs, snapshots, destroy.

use iced::widget::{Column, button, column, pick_list, row, rule, text, text_input};
use iced::{Alignment, Element, Length};
use nlink_lab::Topology;
use nlink_lab_shared::api::Direction;

use super::{heading, muted};
use crate::control::{ImpairEditor, KNOBS, PairEditor, Preset};
use crate::message::{Control, Message};
use crate::model::Live;

/// A message, when the user may send it.
fn when(ok: bool, m: Message) -> Option<Message> {
    ok.then_some(m)
}

fn c(c: Control) -> Message {
    Message::Control(c)
}

/// Said to a viewer, where the controls would be.
pub fn read_only<'a>() -> Element<'a, Message> {
    muted("Read-only: the daemon makes you a viewer here (the nlink-lab group operates).").into()
}

const PLACEHOLDERS: [&str; 10] = [
    "50ms", "5ms", "1%", "10mbit", "0.1%", "25%", "1%", "25%", "25%", "1000",
];

/// The impairment editor of the selected endpoint.
pub fn impair<'a>(e: &'a ImpairEditor, l: &'a Live, operator: bool) -> Column<'a, Message> {
    let ok = operator && l.pending == 0;
    let partitioned = l
        .endpoints
        .iter()
        .any(|x| x.endpoint == e.endpoint && x.partitioned);
    let mut col = column![rule::horizontal(1), heading("Impair")].spacing(6);
    if !operator {
        col = col.push(read_only());
    }
    let dir = |d: Direction, label: &'static str| {
        button(text(label).size(12))
            .style(if e.direction == d {
                button::primary
            } else {
                button::secondary
            })
            .padding([2, 8])
            .on_press_maybe(when(operator, c(Control::Direction(d))))
    };
    col = col.push(
        row![
            muted("direction").width(Length::Fixed(96.0)),
            dir(Direction::Out, "out"),
            dir(Direction::In, "in"),
            dir(Direction::Both, "both"),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    );
    let presets = Preset::ALL
        .iter()
        .fold(
            row![muted("preset").width(Length::Fixed(96.0))].spacing(4),
            |r, p| {
                r.push(
                    button(text(p.label()).size(12))
                        .style(button::text)
                        .padding([2, 6])
                        .on_press_maybe(when(operator, c(Control::Preset(*p)))),
                )
            },
        )
        .wrap();
    col = col.push(presets);
    for (i, knob) in KNOBS.iter().enumerate() {
        let mut input = text_input(PLACEHOLDERS[i], &e.fields[i]).size(13);
        if operator {
            input = input
                .on_input(move |v| c(Control::Field(i, v)))
                .on_submit(c(Control::Impair));
        }
        col = col.push(
            row![muted(*knob).width(Length::Fixed(96.0)), input]
                .spacing(8)
                .align_y(Alignment::Center),
        );
    }
    col.push(
        row![
            button(text("Apply").size(13))
                .style(button::primary)
                .on_press_maybe(when(ok, c(Control::Impair))),
            button(text("Clear").size(13))
                .style(button::secondary)
                .on_press_maybe(when(ok, c(Control::Clear))),
            if partitioned {
                button(text("Heal").size(13))
                    .style(button::success)
                    .on_press_maybe(when(ok, c(Control::Heal)))
            } else {
                button(text("Partition").size(13))
                    .style(button::danger)
                    .on_press_maybe(when(ok, c(Control::Partition)))
            },
        ]
        .spacing(8),
    )
}

/// The per-pair impairment form of the selected network.
pub fn pairs<'a>(
    p: &'a PairEditor,
    l: &'a Live,
    topo: &'a Topology,
    operator: bool,
) -> Column<'a, Message> {
    let ok = operator && l.pending == 0;
    let mut col = column![rule::horizontal(1), heading("Between two members")].spacing(6);
    if !operator {
        col = col.push(read_only());
    }
    let network = topo.networks.get(&p.network);
    let mut nodes: Vec<String> = network
        .map(|n| {
            n.members
                .iter()
                .filter_map(|m| m.split(':').next().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    nodes.sort_by(|a, b| nlink_lab_view::layout::natural(a, b));
    nodes.dedup();
    col = col.push(
        row![
            pick_list(nodes.clone(), p.src.clone(), |v| c(Control::PairSrc(v)))
                .placeholder("from")
                .text_size(13),
            text("→").size(13),
            pick_list(nodes, p.dst.clone(), |v| c(Control::PairDst(v)))
                .placeholder("to")
                .text_size(13),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    );
    for (i, knob) in KNOBS.iter().take(4).enumerate() {
        let mut input = text_input(PLACEHOLDERS[i], &p.fields[i]).size(13);
        if operator {
            input = input.on_input(move |v| c(Control::PairField(i, v)));
        }
        col = col.push(
            row![muted(*knob).width(Length::Fixed(96.0)), input]
                .spacing(8)
                .align_y(Alignment::Center),
        );
    }
    let mut cap = text_input("100mbit", &p.rate_cap).size(13);
    if operator {
        cap = cap.on_input(|v| c(Control::PairRateCap(v)));
    }
    col = col.push(
        row![muted("rate cap").width(Length::Fixed(96.0)), cap]
            .spacing(8)
            .align_y(Alignment::Center),
    );
    col = col.push(
        row![
            button(text("Set").size(13))
                .style(button::primary)
                .on_press_maybe(when(ok, c(Control::SetPair))),
            button(text("Clear").size(13))
                .style(button::secondary)
                .on_press_maybe(when(ok, c(Control::ClearPair))),
        ]
        .spacing(8),
    );
    if let Some(n) = network.filter(|n| !n.impairments.is_empty()) {
        col = col.push(muted("Declared"));
        for pi in &n.impairments {
            col = col.push(muted(format!(
                "{} → {}: {}{}",
                pi.src,
                pi.dst,
                pi.impairment.summary(),
                pi.rate_cap
                    .as_ref()
                    .map(|r| format!(", cap {r}"))
                    .unwrap_or_default()
            )));
        }
    }
    col
}

/// The lab's snapshots, and the form that takes one.
pub fn snapshots<'a>(l: &'a Live, operator: bool) -> Column<'a, Message> {
    let ok = operator && l.pending == 0;
    let mut col = column![rule::horizontal(1), heading("Snapshots")].spacing(6);
    if l.snapshots.is_empty() {
        col = col.push(muted("None yet."));
    }
    for s in &l.snapshots {
        col = col.push(
            row![
                column![
                    text(s.name.as_str()).size(13),
                    muted(format!(
                        "{}{}",
                        s.created_at.replace('T', " "),
                        s.description
                            .as_ref()
                            .map(|d| format!(" · {d}"))
                            .unwrap_or_default()
                    )),
                ]
                .width(Length::Fill),
                button(text("Restore…").size(12))
                    .style(button::secondary)
                    .padding([2, 8])
                    .on_press_maybe(when(ok, c(Control::AskRestore(s.name.clone())))),
                button(text("Delete").size(12))
                    .style(button::text)
                    .padding([2, 8])
                    .on_press_maybe(when(ok, c(Control::AskDelete(s.name.clone())))),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        );
    }
    if operator {
        col = col.push(
            row![
                text_input("name", &l.snapshot_form.name)
                    .size(13)
                    .on_input(|v| c(Control::SnapshotName(v)))
                    .on_submit(c(Control::Snapshot)),
                text_input("note (optional)", &l.snapshot_form.description)
                    .size(13)
                    .on_input(|v| c(Control::SnapshotNote(v)))
                    .on_submit(c(Control::Snapshot)),
                button(text("Save").size(13))
                    .style(button::primary)
                    .on_press_maybe(when(ok, c(Control::Snapshot))),
            ]
            .spacing(6),
        );
    }
    col
}

/// Destroy, behind its confirmation.
pub fn destroy<'a>(l: &'a Live, operator: bool) -> Element<'a, Message> {
    if !operator || l.gone {
        return column![].into();
    }
    column![
        rule::horizontal(1),
        button(text("Destroy…").size(13))
            .style(button::danger)
            .on_press_maybe(when(l.pending == 0, c(Control::AskDestroy))),
    ]
    .spacing(6)
    .into()
}
