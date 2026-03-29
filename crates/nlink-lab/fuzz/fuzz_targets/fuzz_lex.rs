#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(input) = std::str::from_utf8(data) {
        // Must never panic — only Ok or Err.
        let _ = nlink_lab::parser::nll::lexer::lex(input);
    }
});
