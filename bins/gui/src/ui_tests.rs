//! The app drawn headless (iced's tiny-skia simulator): every screen
//! renders, and a text field takes the keys typed in it, so a shortcut
//! (`keyboard::listen()` sees only keys nobody took) never fires while
//! typing. With `NLINK_LAB_GUI_SNAPSHOTS=DIR`, the frames are written
//! there as PNGs to look at.

use iced::{Settings, Theme, event};
use iced_test::Simulator;
use nlink_lab_view::Hit;

use crate::doc::Source;
use crate::message::{CanvasMsg, Message};
use crate::model::App;
use crate::reduce::{
    reduce,
    testing::{demo, demo_lab, settle},
};

const SIZE: (f32, f32) = (1280.0, 820.0);

/// A lab with something of everything the canvas draws.
const LAB: &str = r#"param wan default 30ms
lab "showcase" { description "Two sites over a WAN, a shared LAN" }
profile router { forward ipv4 }
site west {
  node r : router
  node h
  link r:eth1 -- h:eth0 { 10.1.0.1/24 -- 10.1.0.2/24 }
}
site east {
  node r : router
  node db { image "postgres:16" }
  link r:eth1 -- db:eth0 { 10.2.0.1/24 -- 10.2.0.2/24 }
}
link west-r:eth0 -- east-r:eth0 { 192.168.0.1/30 -- 192.168.0.2/30  delay ${wan} }
node c1
node c2
network lan { subnet 10.9.0.0/24  members [west-h:eth1, c1:eth0, c2:eth0] }
"#;

fn app_with(text: &str) -> App {
    let mut app = App::new(crate::settings::Settings::default(), None);
    let e = crate::reduce::open(
        &mut app,
        Source::Untitled {
            name: "showcase".into(),
            text: text.into(),
        },
        Default::default(),
    );
    settle(&mut app, e);
    app
}

fn simulator(app: &App) -> Simulator<'_, Message> {
    Simulator::with_size(Settings::default(), SIZE, crate::view::view(app))
}

/// Draw `app`, and keep the frame when asked to.
fn snapshot(app: &App, theme: &Theme, name: &str) {
    let mut ui = simulator(app);
    let frame = ui.snapshot(theme).expect("a frame");
    if let Some(dir) = std::env::var_os("NLINK_LAB_GUI_SNAPSHOTS") {
        let path = std::path::Path::new(&dir).join(format!("{name}.png"));
        // `matches_image` writes the file when there is none
        for renderer in ["tiny-skia", "wgpu"] {
            let _ = std::fs::remove_file(path.with_file_name(format!("{name}-{renderer}.png")));
        }
        assert!(frame.matches_image(&path).expect("written"));
    }
}

#[test]
fn the_showcase_parses_cleanly() {
    let app = app_with(LAB);
    let t = app.current().unwrap();
    assert!(t.topology.is_some(), "{:?}", t.design().unwrap().problems);
    assert_eq!(t.errors(), 0, "{:?}", t.design().unwrap().problems);
    assert_eq!(t.view.graph.regions.len(), 2);
}

#[test]
fn home_renders() {
    let mut app = App::new(crate::settings::Settings::default(), None);
    app.settings
        .remember(std::path::Path::new("/srv/labs/spine-leaf.nll"));
    snapshot(&app, &Theme::Light, "home-light");
    snapshot(&app, &Theme::Dark, "home-dark");
}

#[test]
fn a_workspace_renders_in_both_themes() {
    let mut app = app_with(LAB);
    reduce(
        &mut app,
        Message::Canvas(CanvasMsg::Select(Some(Hit::Node("west-r".into())))),
    );
    snapshot(&app, &Theme::Light, "workspace-light");
    snapshot(&app, &Theme::Dark, "workspace-dark");
    reduce(
        &mut app,
        Message::Canvas(CanvasMsg::Select(Some(Hit::Endpoint("west-r:eth0".into())))),
    );
    snapshot(&app, &Theme::Light, "workspace-endpoint");
    reduce(&mut app, Message::Canvas(CanvasMsg::Select(None)));
    reduce(&mut app, Message::Dock(crate::model::DockTab::Source));
    snapshot(&app, &Theme::Light, "workspace-lab");
}

#[test]
fn a_broken_topology_shows_its_problem() {
    let mut app = app_with("lab \"b\"\nnode r1 {\n  bogus\n}\n");
    assert_eq!(
        app.current().unwrap().design().unwrap().problems[0].rule,
        "parse"
    );
    snapshot(&app, &Theme::Light, "workspace-broken");
    // the problem leads to its line
    reduce(&mut app, Message::ProblemClicked(0));
    assert_eq!(app.current().unwrap().dock, crate::model::DockTab::Source);
    snapshot(&app, &Theme::Dark, "workspace-broken-source");
}

#[test]
fn the_shortcut_sheet_renders() {
    let mut app = app_with(LAB);
    app.help = true;
    snapshot(&app, &Theme::Dark, "help");
}

#[test]
fn typing_in_a_field_is_taken_by_the_field() {
    let app = app_with(LAB);
    let mut ui = simulator(&app);
    // the `wan` param's field holds its default
    ui.click("30ms").expect("the param field");
    // `f` (fit), `l` (layout), `a` (addresses): all shortcuts
    let status = ui.typewrite("fla");
    assert_eq!(status, event::Status::Captured);
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(
        messages
            .iter()
            .any(|m| matches!(m, Message::ParamEdited { name, .. } if name == "wan")),
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(|m| matches!(m, Message::Shortcut(_))),
        "{messages:?}"
    );
}

#[test]
fn keys_on_the_canvas_are_left_to_the_shortcuts() {
    let app = app_with(LAB);
    let mut ui = simulator(&app);
    // focus nothing: a key goes unclaimed, for keyboard::listen()
    ui.point_at((300.0, 300.0));
    let f = iced::keyboard::Key::Character("f".into());
    assert_eq!(ui.tap_key(f), event::Status::Ignored);
}

/// Every example opens, lays out and draws: the graph, the layouts and the
/// canvas meet every kind of topology there is.
#[test]
fn every_example_opens_and_draws() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let mut files = Vec::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|e| e == "nll") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(files.len() > 40, "{}", files.len());
    let mut drawn = 0;
    for file in &files {
        let mut app = App::new(crate::settings::Settings::default(), None);
        let e = crate::reduce::open(&mut app, Source::File(file.clone()), Default::default());
        settle(&mut app, e);
        let t = app.current().unwrap();
        // an import module or a template with required params may not
        // stand alone; what does must be drawn whole
        if t.topology.is_none() {
            continue;
        }
        assert_eq!(
            t.view.positions.len(),
            t.view.graph.nodes.len(),
            "{}",
            file.display()
        );
        assert_eq!(
            t.view.scene.nodes.len(),
            t.view.graph.nodes.len(),
            "{}",
            file.display()
        );
        let stem = file.file_stem().unwrap().to_string_lossy();
        snapshot(&app, &Theme::Light, &format!("example-{stem}"));
        drawn += 1;
    }
    assert!(
        drawn > 40,
        "only {drawn} of {} examples parsed",
        files.len()
    );
}

#[test]
fn home_lists_the_running_labs() {
    let mut app = App::new(crate::settings::Settings::default(), None);
    demo(&mut app);
    snapshot(&app, &Theme::Light, "home-labs");
    // and says so when no daemon answers
    let mut app = App::new(crate::settings::Settings::default(), None);
    app.target = Some(crate::backend::Target::Daemon(
        nlink_lab_client::Endpoint::Unix("/run/nlink-lab/api/api.sock".into()),
    ));
    reduce(
        &mut app,
        Message::Connected(Err("No such file or directory".into())),
    );
    snapshot(&app, &Theme::Dark, "home-no-daemon");
}

#[test]
fn a_running_lab_renders_live() {
    let mut app = App::new(crate::settings::Settings::default(), None);
    let (fake, lab) = demo_lab(&mut app);
    // a minute of samples, for the sparklines
    for t in 6..66 {
        let snapshot = fake.metrics(&lab, t as f64).unwrap();
        let epoch = app.epoch;
        reduce(
            &mut app,
            Message::Feed {
                epoch,
                lab: Some(lab.clone()),
                feed: crate::backend::Feed::note(nlink_lab_shared::api::Notification::Metrics {
                    lab: lab.clone(),
                    snapshot,
                }),
            },
        );
    }
    let node = app.current().unwrap().view.graph.nodes[0].id.clone();
    reduce(
        &mut app,
        Message::Canvas(CanvasMsg::Select(Some(Hit::Node(node)))),
    );
    snapshot(&app, &Theme::Light, "live-light");
    snapshot(&app, &Theme::Dark, "live-dark");
    reduce(&mut app, Message::Dock(crate::model::DockTab::Processes));
    reduce(&mut app, Message::Canvas(CanvasMsg::Select(None)));
    snapshot(&app, &Theme::Light, "live-lab");
    assert!(app.current().unwrap().live().is_some_and(|l| l.lab == lab));
}

#[test]
fn the_controls_render() {
    use crate::message::Control;
    let mut app = App::new(crate::settings::Settings::default(), None);
    let (_, lab) = demo_lab(&mut app);
    let ep = app
        .current()
        .unwrap()
        .live()
        .unwrap()
        .endpoints
        .iter()
        .find(|e| e.live.is_some())
        .unwrap()
        .endpoint
        .clone();
    reduce(
        &mut app,
        Message::Canvas(CanvasMsg::Select(Some(Hit::Endpoint(ep)))),
    );
    snapshot(&app, &Theme::Light, "control-impair");
    reduce(&mut app, Message::Control(Control::AskDestroy));
    reduce(
        &mut app,
        Message::Control(Control::Typed(lab[..3].to_string())),
    );
    snapshot(&app, &Theme::Dark, "control-destroy");
    // the processes page, with its start form
    reduce(&mut app, Message::Control(Control::Cancel));
    reduce(&mut app, Message::Dock(crate::model::DockTab::Processes));
    snapshot(&app, &Theme::Light, "control-processes");
    // a network's pair form, in the campus lab
    let e = reduce(&mut app, Message::OpenLab("demo-campus".into()));
    settle(&mut app, e);
    let net = app
        .current()
        .unwrap()
        .view
        .graph
        .nodes
        .iter()
        .find(|n| n.id.starts_with("net:"))
        .unwrap()
        .id
        .clone();
    reduce(
        &mut app,
        Message::Canvas(CanvasMsg::Select(Some(Hit::Node(net)))),
    );
    snapshot(&app, &Theme::Light, "control-pairs");
}
