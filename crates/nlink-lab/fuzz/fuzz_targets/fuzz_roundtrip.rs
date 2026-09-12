#![no_main]

use libfuzzer_sys::fuzz_target;

// parse → render → parse must be a fixed point for every topology the
// parser accepts: whatever `render` emits has to parse back to the same
// `Topology`. Catches both render losses and parser/render disagreements.
fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(topo) = nlink_lab::parser::parse(input) else {
        return;
    };
    let Ok(rendered) = nlink_lab::render::render(&topo) else {
        return;
    };
    match nlink_lab::parser::parse(&rendered) {
        Ok(again) => {
            let a = serde_json::to_value(&topo).unwrap();
            let b = serde_json::to_value(&again).unwrap();
            assert_eq!(a, b, "render round-trip is not a fixed point:\n{rendered}");
        }
        Err(e) => panic!("rendered NLL does not parse: {e}\n{rendered}"),
    }
});
