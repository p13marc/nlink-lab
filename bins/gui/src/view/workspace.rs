//! A tab: the toolbar, then the canvas, the inspector and the dock in a
//! pane grid.

use iced::widget::{Canvas, button, checkbox, container, pane_grid, pick_list, row, space, text};
use iced::{Alignment, Element, Length, Theme};

use super::{dock, inspector, muted};
use crate::canvas::TopoCanvas;
use crate::doc::Source;
use crate::export::Format;
use crate::message::{Message, Shortcut};
use crate::model::{App, DockTab, Pane, Tab};
use crate::settings::LayoutPref;

pub fn view<'a>(
    app: &'a App,
    tab: &'a Tab,
    terminals: Option<&'a crate::term::Terminals>,
) -> Element<'a, Message> {
    let grid = pane_grid(&tab.panes, |_, pane, _| {
        let (title, body): (Element<'a, Message>, Element<'a, Message>) = match pane {
            Pane::Canvas => (canvas_title(tab), canvas(tab)),
            Pane::Inspector => (text("Inspector").size(13).into(), inspector::view(app, tab)),
            Pane::Dock => (dock_title(tab), dock::view(app, tab, terminals)),
        };
        pane_grid::Content::new(container(body).width(Length::Fill).height(Length::Fill))
            .title_bar(
                pane_grid::TitleBar::new(title)
                    .padding([3, 8])
                    .style(super::bar),
            )
            .style(pane_style)
    })
    .spacing(4)
    .on_resize(8, Message::PaneResized);
    iced::widget::column![toolbar(app, tab), container(grid).padding(4)].into()
}

fn pane_style(theme: &Theme) -> container::Style {
    container::Style {
        border: iced::Border {
            width: 1.0,
            color: theme.extended_palette().background.strong.color,
            radius: 4.0.into(),
        },
        ..container::Style::default()
    }
}

fn toolbar<'a>(app: &'a App, tab: &'a Tab) -> Element<'a, Message> {
    let tool = |label: &'a str, msg: Message| {
        button(text(label).size(13))
            .style(button::secondary)
            .on_press(msg)
    };
    let can_export = tab.topology.is_some() && tab.view.scene.bounds.is_some();
    let export = Format::ALL.iter().fold(
        row![muted("Export")].spacing(4).align_y(Alignment::Center),
        |r, f| {
            r.push(
                button(text(f.label()).size(13))
                    .style(button::secondary)
                    .on_press_maybe(can_export.then_some(Message::Export(*f))),
            )
        },
    );
    let mut bar = row![
        tool("Reload", Message::Reload),
        muted("Layout"),
        pick_list(LayoutPref::ALL, Some(tab.view.layout), Message::SetLayout).text_size(13),
        tool("Fit", Message::Shortcut(Shortcut::Fit)),
        tool("−", Message::Shortcut(Shortcut::ZoomOut)),
        tool("+", Message::Shortcut(Shortcut::ZoomIn)),
        checkbox(app.settings.show_addresses)
            .label("Addresses")
            .text_size(13)
            .on_toggle(|_| Message::ToggleAddresses),
        checkbox(app.settings.show_mgmt)
            .label("Management")
            .text_size(13)
            .on_toggle(|_| Message::ToggleMgmt),
        space::horizontal(),
        export,
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    // deploy and apply, for an operator connected to a daemon
    if app.operator() {
        use crate::message::WorkflowMsg;
        let wf = |label: &'a str, m: WorkflowMsg, primary: bool| {
            button(text(label).size(13))
                .style(if primary {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Message::Workflow(m))
        };
        if tab.design().is_some() && tab.topology.is_some() {
            let lab = tab
                .topology
                .as_ref()
                .map(|t| t.lab.name.as_str())
                .unwrap_or("");
            if app.labs.iter().any(|l| l.lab == lab) {
                bar = bar.push(wf("Apply…", WorkflowMsg::AskApply, false));
            }
            bar = bar.push(wf("Deploy…", WorkflowMsg::AskDeploy, true));
        }
        if tab.live().is_some_and(|l| !l.gone) {
            bar = bar.push(wf("Apply a file…", WorkflowMsg::ApplyFile, false));
        }
    }
    if matches!(
        tab.design().map(|d| &d.source),
        Some(Source::Untitled { .. })
    ) {
        bar = bar.push(
            button(text("Save as…").size(13))
                .style(button::primary)
                .on_press(Message::SaveAs),
        );
    }
    container(bar).padding([6, 10]).width(Length::Fill).into()
}

fn canvas_title(tab: &Tab) -> Element<'_, Message> {
    let mut r = row![text(tab.title()).size(13)]
        .spacing(10)
        .align_y(Alignment::Center);
    if let Some(d) = tab.design() {
        if d.loading {
            r = r.push(muted("loading…"));
        } else if tab.view.laying_out {
            r = r.push(muted("laying out…"));
        }
        if d.stale {
            r = r.push(
                text("does not parse: showing the last version that did")
                    .size(12)
                    .style(super::tone_text(nlink_lab_view::scene::Tone::Warn)),
            );
        }
    }
    if let Some(l) = tab.live() {
        use nlink_lab_view::scene::Tone;
        let (word, tone) = if l.gone {
            ("destroyed: showing what it was".to_string(), Tone::Bad)
        } else if let Some(e) = &l.error {
            (e.clone(), Tone::Bad)
        } else if tab.view.stale {
            ("metrics stopped".to_string(), Tone::Warn)
        } else if l.metrics_at.is_some() {
            ("live".to_string(), Tone::Good)
        } else {
            ("waiting for metrics…".to_string(), Tone::Muted)
        };
        r = r.push(
            text(format!("● {word}"))
                .size(12)
                .style(super::tone_text(tone)),
        );
        if !l.revision.is_empty() {
            r = r.push(muted(format!("revision {}", short(&l.revision))));
        }
    }
    r.into()
}

/// A revision's first characters, as git shows a commit.
fn short(revision: &str) -> &str {
    revision.get(..8).unwrap_or(revision)
}

fn canvas(tab: &Tab) -> Element<'_, Message> {
    let placeholder = if tab.topology.is_none() {
        Some(match (tab.design(), tab.live()) {
            (Some(d), _) if d.loading => "Loading…",
            (Some(_), _) => "This topology does not parse: see Problems",
            (_, Some(l)) if l.gone => "This lab is not running",
            (_, Some(l)) if l.error.is_some() => "The lab's topology did not come: see above",
            _ => "Waiting for the lab's topology…",
        })
    } else if tab.view.positions.is_empty() {
        Some(if tab.view.graph.nodes.is_empty() {
            "No nodes yet"
        } else {
            "Laying out…"
        })
    } else {
        None
    };
    Canvas::new(TopoCanvas {
        view: &tab.view,
        placeholder,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn dock_title(tab: &Tab) -> Element<'_, Message> {
    let page = |page: DockTab| {
        let label = match page {
            DockTab::Problems => format!(
                "Problems ({})",
                tab.design().map_or(0, |d| d.problems.len())
            ),
            DockTab::Source => "Source".into(),
            DockTab::Events => format!("Events ({})", tab.live().map_or(0, |l| l.events.len())),
            DockTab::Processes => format!(
                "Processes ({})",
                tab.live().map_or(0, |l| l.processes.len())
            ),
            DockTab::Console => {
                let running = tab
                    .live()
                    .map_or(0, |l| l.sessions.iter().filter(|s| s.running()).count());
                if running > 0 {
                    format!("Console ({running})")
                } else {
                    "Console".into()
                }
            }
            DockTab::Terminal => {
                format!("Terminal ({})", tab.live().map_or(0, |l| l.terminals.len()))
            }
            DockTab::Jobs => "Jobs".into(),
        };
        button(text(label).size(13))
            .style(if tab.dock == page {
                button::secondary
            } else {
                button::text
            })
            .padding([2, 8])
            .on_press(Message::Dock(page))
    };
    tab.dock_pages()
        .iter()
        .fold(row![].spacing(4), |r, p| r.push(page(*p)))
        .into()
}
