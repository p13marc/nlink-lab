//! The API's wire shapes (#233): what goes over the socket must come back
//! as it went, and look the way docs/API.md says.

use nlink_lab_shared::api::*;
use serde_json::json;

fn round_trip<T>(v: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
{
    let text = serde_json::to_string(v).unwrap();
    let back: T = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    assert_eq!(&back, v, "{text}");
    back
}

#[test]
fn requests_are_method_and_params() {
    let r = Request::Impair {
        lab: "wan".into(),
        endpoint: "r1:eth0".into(),
        direction: Direction::Both,
        spec: ImpairmentSpec {
            delay: Some("50ms".into()),
            loss: Some("1%".into()),
            ..Default::default()
        },
    };
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        json!({"method": "impair", "params": {"lab": "wan", "endpoint": "r1:eth0",
               "direction": "both", "spec": {"delay": "50ms", "loss": "1%"}}})
    );
    round_trip(&r);
    assert_eq!(r.method(), "impair");
    assert_eq!(r.lab(), Some("wan"));
    // defaults: a topology is redacted unless asked
    let t: Request =
        serde_json::from_value(json!({"method": "get_topology", "params": {"lab": "x"}})).unwrap();
    assert_eq!(
        t,
        Request::GetTopology {
            lab: "x".into(),
            redact: true
        }
    );
    let p: Request = serde_json::from_value(json!({"method": "ping"})).unwrap();
    assert_eq!(p, Request::Ping);
}

#[test]
fn roles_follow_what_a_method_does() {
    let lab = || "l".to_string();
    assert_eq!(Request::ListLabs.meta().role, Role::Viewer);
    let secret = Request::GetTopology {
        lab: lab(),
        redact: false,
    };
    assert_eq!(
        secret.meta().role,
        Role::Operator,
        "secrets need an operator"
    );
    let deploy = Request::Deploy {
        topology: json!({}),
        options: Default::default(),
        source: None,
    };
    let m = deploy.meta();
    assert!(m.mutating && m.long_running && m.role == Role::Operator);
    assert_eq!(m.on_disconnect, OnDisconnect::Continue);
    let exec = Request::Exec {
        lab: lab(),
        node: "a".into(),
        argv: vec!["true".into()],
        env: vec![],
        workdir: None,
        stdin: false,
        tty: false,
        timeout_ms: None,
    };
    assert_eq!(exec.meta().role, Role::Operator);
    assert_eq!(exec.meta().on_disconnect, OnDisconnect::Cancel);
    let wait = Request::WaitFor {
        lab: lab(),
        node: "a".into(),
        cond: WaitSpec {
            exec: Some("true".into()),
            ..Default::default()
        },
    };
    assert_eq!(
        wait.meta().role,
        Role::Operator,
        "an exec condition runs a command"
    );
}

#[test]
fn messages_round_trip() {
    let hello = ClientMsg::Hello {
        api: api_version_string(),
        client: "test".into(),
        features: vec![],
    };
    round_trip(&hello);
    let call = ClientMsg::Call {
        request: Box::new(Request::ListLabs),
    };
    assert_eq!(
        serde_json::to_value(&call).unwrap(),
        json!({"t": "call", "method": "list_labs"})
    );
    round_trip(&call);
    round_trip(&ClientMsg::Resize { rows: 24, cols: 80 });

    let event = ServerMsg::Event {
        item: StreamItem::Op {
            event: OpEvent {
                op: "op-1".into(),
                lab: "wan".into(),
                seq: 3,
                ts_ms: 1,
                phase: OpPhase::Step {
                    done: 4,
                    total: 9,
                    describe: "veth a:eth0 ↔ b:eth0".into(),
                },
            },
        },
    };
    let v = serde_json::to_value(&event).unwrap();
    assert_eq!(v["t"], "event");
    assert_eq!(v["item"], "op");
    assert_eq!(v["event"]["phase"], "step");
    assert_eq!(v["event"]["done"], 4);
    round_trip(&event);

    let failed = OpPhase::Finished {
        outcome: OpOutcome::RolledBack {
            error: ApiError::new(ErrorCode::Kernel, "EEXIST"),
        },
        elapsed_ms: 10,
    };
    round_trip(&failed);
    round_trip(&OpPhase::Waiting {
        what: WaitKind::Healthcheck { node: "db".into() },
        detail: Some("pg_isready".into()),
        deadline_ms: Some(30_000),
    });
    let end = ServerMsg::End {
        ok: Some(Response::Labs {
            labs: vec![LabSummary {
                lab: "wan".into(),
                nodes: 3,
                ..Default::default()
            }],
        }),
        err: None,
    };
    round_trip(&end);
    let err = ServerMsg::End {
        ok: None,
        err: Some(ApiError::new(ErrorCode::NotFound, "lab not found: x").with_hint("ls")),
    };
    round_trip(&err);
    round_trip(&Notification::TopologyChanged {
        lab: "wan".into(),
        revision: "abc".into(),
    });
}

#[test]
fn exit_codes_go_with_error_codes() {
    assert_eq!(ApiError::new(ErrorCode::Validation, "x").exit_code, 2);
    assert_eq!(ApiError::new(ErrorCode::Timeout, "x").exit_code, 124);
    assert_eq!(ApiError::new(ErrorCode::Cancelled, "x").exit_code, 130);
    assert_eq!(ApiError::new(ErrorCode::NotFound, "x").exit_code, 1);
}

#[test]
fn frames_encode_and_decode_across_split_reads() {
    let a = Frame::message(0, &ClientMsg::Ping);
    let b = Frame::Data {
        call: 7,
        stream: DataStream::Stdout,
        bytes: b"hello\n".to_vec(),
    };
    let mut wire = Vec::new();
    a.encode(&mut wire);
    b.encode(&mut wire);
    // any prefix is "incomplete", never an error
    for cut in 0..wire.len() {
        let got = Frame::decode(&wire[..cut]).unwrap();
        if let Some((f, n)) = got {
            assert_eq!(f, a);
            assert!(n <= cut);
        }
    }
    let (f1, n1) = Frame::decode(&wire).unwrap().unwrap();
    assert_eq!(f1, a);
    let (f2, n2) = Frame::decode(&wire[n1..]).unwrap().unwrap();
    assert_eq!(f2, b);
    assert_eq!(n1 + n2, wire.len());
    assert_eq!(f2.call(), 7);
}

#[test]
fn bad_frames_are_errors() {
    let mut big = ((MAX_FRAME as u32) + 1).to_be_bytes().to_vec();
    big.extend_from_slice(&[0; 8]);
    assert!(matches!(Frame::decode(&big), Err(FrameError::TooLarge(_))));
    let mut short = 3u32.to_be_bytes().to_vec();
    short.extend_from_slice(&[0; 3]);
    assert_eq!(Frame::decode(&short), Err(FrameError::Truncated));
    let mut kind = 5u32.to_be_bytes().to_vec();
    kind.extend_from_slice(&[9, 0, 0, 0, 1]);
    assert_eq!(Frame::decode(&kind), Err(FrameError::UnknownKind(9)));
    let mut stream = 6u32.to_be_bytes().to_vec();
    stream.extend_from_slice(&[1, 0, 0, 0, 1, 42]);
    assert_eq!(Frame::decode(&stream), Err(FrameError::UnknownStream(42)));
}

#[test]
fn versions_parse() {
    assert_eq!(api_major(&api_version_string()), Some(API_VERSION.0));
    assert_eq!(api_major("2.7"), Some(2));
    assert_eq!(api_major("x"), None);
}

#[test]
fn every_message_has_a_schema() {
    for schema in [
        serde_json::to_value(schemars::schema_for!(ClientMsg)).unwrap(),
        serde_json::to_value(schemars::schema_for!(ServerMsg)).unwrap(),
    ] {
        assert!(schema.is_object());
    }
}
