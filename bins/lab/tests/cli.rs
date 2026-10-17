//! The CLI as a user runs it, rootless: lab selection (`-L`,
//! `NLINK_LAB`, `use`, the only lab), usage errors (exit 64), output
//! formats. Each test has its own state root (`NLINK_LAB_STATE_DIR`), with
//! fake labs written as files: nothing here deploys.

use std::path::PathBuf;
use std::process::Output;

struct Host {
    root: tempfile::TempDir,
}

impl Host {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    /// A lab's state files, as a deploy leaves them.
    fn lab(&self, name: &str) -> &Self {
        let dir = self.root.path().join("labs").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let state = nlink_lab::state::LabState::new(name, "2026-10-17T12:00:00Z");
        std::fs::write(
            dir.join("state.json"),
            serde_json::to_string_pretty(&state).unwrap(),
        )
        .unwrap();
        let topo = nlink_lab::parser::parse(&format!("lab \"{name}\"\nnode n\n")).unwrap();
        std::fs::write(dir.join("topology.toml"), toml::to_string(&topo).unwrap()).unwrap();
        self
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut cmd = std::process::Command::new(bin());
        cmd.args(args)
            .env("NLINK_LAB_STATE_DIR", self.root.path())
            .env_remove("NLINK_LAB")
            .env_remove("SUDO_UID")
            .env("NO_COLOR", "1");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }
}

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_nlink-lab"))
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn json(o: &Output) -> serde_json::Value {
    serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
}

#[test]
fn the_lab_is_the_flag_then_env_then_use_then_the_only_one() {
    let h = Host::new();
    let none = h.run(&["ps"]);
    assert_eq!(code(&none), 1, "{}", stderr(&none));
    assert!(
        stderr(&none).contains("no lab is running"),
        "{}",
        stderr(&none)
    );

    h.lab("alpha");
    let only = h.run(&["--json", "ps"]);
    assert_eq!(code(&only), 0, "{}", stderr(&only));
    assert_eq!(json(&only)["lab"], "alpha");

    h.lab("beta");
    let two = h.run(&["ps"]);
    assert_eq!(code(&two), 1);
    assert!(
        stderr(&two).contains("2 labs are running (alpha, beta)"),
        "{}",
        stderr(&two)
    );

    let flag = h.run(&["--json", "ps", "-L", "beta"]);
    assert_eq!(json(&flag)["lab"], "beta");
    let env = h.run_env(&["--json", "ps"], &[("NLINK_LAB", "alpha")]);
    assert_eq!(json(&env)["lab"], "alpha");
    // the flag wins over the environment
    let both = h.run_env(&["--json", "ps", "-L", "beta"], &[("NLINK_LAB", "alpha")]);
    assert_eq!(json(&both)["lab"], "beta");

    let set = h.run(&["use", "beta"]);
    assert_eq!(code(&set), 0, "{}", stderr(&set));
    let current = h.run(&["--json", "use"]);
    assert_eq!(json(&current)["lab"], "beta");
    assert_eq!(json(&current)["from"], "context");
    assert_eq!(json(&h.run(&["--json", "ps"]))["lab"], "beta");
    // the environment wins over `use`
    let env = h.run_env(&["--json", "use"], &[("NLINK_LAB", "alpha")]);
    assert_eq!(json(&env)["from"], "env");

    assert_eq!(code(&h.run(&["use", "--clear"])), 0);
    assert_eq!(code(&h.run(&["ps"])), 1);
    assert_eq!(stdout(&h.run(&["-q", "use"])), "");
}

#[test]
fn a_lab_that_does_not_run_is_refused() {
    let h = Host::new();
    h.lab("alpha");
    let o = h.run(&["use", "nope"]);
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("lab not found: nope"), "{}", stderr(&o));
    let o = h.run_env(&["ps"], &[("NLINK_LAB", "nope")]);
    assert_eq!(code(&o), 1, "{}", stderr(&o));

    // `use` remembered a lab that was destroyed since
    assert_eq!(code(&h.run(&["use", "alpha"])), 0);
    std::fs::remove_dir_all(h.root.path().join("labs/alpha")).unwrap();
    let o = h.run(&["ps"]);
    assert_eq!(code(&o), 1);
    assert!(
        stderr(&o).contains("`nlink-lab use --clear`"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn the_old_lab_first_order_gets_a_hint() {
    let h = Host::new();
    h.lab("alpha");
    let o = h.run(&["ps", "alpha"]);
    assert_eq!(code(&o), 64, "{}", stderr(&o));
    assert!(
        stderr(&o).contains("'alpha' is a lab: select it with -L alpha"),
        "{}",
        stderr(&o)
    );
}

#[test]
fn usage_errors_exit_64_and_say_why() {
    let h = Host::new();
    for args in [
        &["no-such-command"][..],
        &["wait-for", "n", "--tcp", "80", "--timeout", "30"],
        &["exec", "n", "-e", "NOEQUALS", "--", "true"],
        &["capture", "not-an-endpoint"],
        &["kill", "1", "-s", "NOPE"],
    ] {
        let o = h.run(args);
        assert_eq!(code(&o), 64, "{args:?}: {}", stderr(&o));
    }
    let o = h.run(&["wait-for", "n", "--tcp", "80", "--timeout", "30"]);
    assert!(stderr(&o).contains("write `30s`"), "{}", stderr(&o));
    let o = h.run(&["--json", "no-such-command"]);
    let doc: serde_json::Value = serde_json::from_slice(&o.stderr).unwrap();
    assert_eq!(doc["code"], "invalid_argument");
    assert_eq!(doc["exit_code"], 64);
}

#[test]
fn formats_and_quiet() {
    let h = Host::new();
    h.lab("alpha");
    let o = h.run(&["--format", "ndjson", "ps"]);
    let out = stdout(&o);
    assert_eq!(out.lines().count(), 1, "{out}");
    let doc: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(doc["schema_version"], 2);
    assert_eq!(doc["processes"], serde_json::json!([]));
    assert_eq!(stdout(&h.run(&["ps"])), "No tracked processes.\n");
    assert_eq!(stdout(&h.run(&["-q", "ps"])), "");
    assert_eq!(code(&h.run(&["--json", "--format", "text", "ps"])), 64);
}

#[test]
fn an_error_under_json_is_the_api_error_document() {
    let h = Host::new();
    let o = h.run(&["--json", "ps", "-L", "nope"]);
    assert_eq!(code(&o), 1);
    let doc: serde_json::Value = serde_json::from_slice(&o.stderr).unwrap();
    assert_eq!(doc["code"], "not_found", "{doc}");
    assert_eq!(doc["exit_code"], 1);
}

#[test]
fn ls_and_status() {
    let h = Host::new();
    h.lab("alpha").lab("beta");
    assert_eq!(code(&h.run(&["use", "beta"])), 0);
    let ls = json(&h.run(&["--json", "ls"]));
    assert_eq!(ls["schema_version"], 1);
    let labs: Vec<(&str, bool)> = ls["labs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["lab"].as_str().unwrap(), l["current"].as_bool().unwrap()))
        .collect();
    assert_eq!(labs, [("alpha", false), ("beta", true)]);
    assert_eq!(stdout(&h.run(&["-q", "ls"])), "alpha\nbeta\n");

    let st = h.run(&["--json", "status", "alpha"]);
    assert_eq!(code(&st), 0, "{}", stderr(&st));
    let st = json(&st);
    assert_eq!(
        (st["schema_version"].clone(), st["lab"].clone()),
        (2.into(), "alpha".into())
    );
    assert_eq!(st["nodes"][0]["node"], "n");
    assert_eq!(st["current"], false);
    // without a name: the current lab
    assert_eq!(json(&h.run(&["--json", "status"]))["lab"], "beta");
    let snaps = json(&h.run(&["--json", "snapshot", "ls", "-L", "alpha"]));
    assert_eq!(snaps["snapshots"], serde_json::json!([]));
}

#[test]
fn removed_commands_and_flags_say_what_replaced_them() {
    let h = Host::new();
    let o = h.run(&["inspect", "wan"]);
    assert_eq!(code(&o), 64);
    assert!(
        stderr(&o).contains("`nlink-lab inspect` was removed: use `nlink-lab status LAB"),
        "{}",
        stderr(&o)
    );
    let o = h.run(&["apply", "--check", "wan.nll"]);
    assert_eq!(code(&o), 64);
    assert!(
        stderr(&o)
            .contains("`nlink-lab apply --check` was removed: use `nlink-lab verify --file FILE`"),
        "{}",
        stderr(&o)
    );
    let o = h.run(&["--json", "restore", "x"]);
    let doc: serde_json::Value = serde_json::from_slice(&o.stderr).unwrap();
    assert!(
        doc["message"]
            .as_str()
            .unwrap()
            .contains("snapshot restore"),
        "{doc}"
    );
}

#[test]
fn destroy_never_guesses_the_lab() {
    let h = Host::new();
    h.lab("alpha");
    let o = h.run(&["destroy"]);
    assert_eq!(code(&o), 1);
    assert!(
        stderr(&o).contains("name the lab to destroy: `nlink-lab destroy alpha`"),
        "{}",
        stderr(&o)
    );
}
