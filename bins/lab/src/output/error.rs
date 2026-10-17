//! The `--json` error envelope: the daemon API's [`ApiError`], so a
//! script reads the same document from the CLI and from the daemon.
//!
//! [`ApiError`]: nlink_lab::api::ApiError

/// `err` as the JSON document `--json` prints on stderr.
pub fn error_json(err: &nlink_lab::Error) -> serde_json::Value {
    serde_json::to_value(err.to_api())
        .unwrap_or_else(|_| serde_json::json!({ "message": err.to_string() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_envelope_carries_what_nlink_says() {
        let kernel = nlink::Error::from_errno_ext_ack(16, Some("busy".into()), Some(36));
        let e = nlink_lab::Error::nlink("failed to apply nftables config on 'b'", kernel);
        let v = error_json(&e);
        assert_eq!(v["code"], "kernel");
        assert_eq!(v["errno"], 16);
        assert_eq!(v["ext_ack"], "busy");
        assert_eq!(v["nlink"]["ext_ack_offset"], 36);
        assert_eq!(v["nlink"]["kind"], "busy", "{v}");
        assert!(v["nlink"]["context"].is_array());
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .starts_with("failed to apply")
        );
        assert_eq!(v["exit_code"], 1);
        // and nothing invented for an error nlink did not raise
        let v = error_json(&nlink_lab::Error::deploy_failed("no runtime"));
        assert!(v.get("nlink").is_none() && v.get("errno").is_none(), "{v}");
    }

    #[test]
    fn a_parse_error_says_where() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("bad.nll");
        std::fs::write(&file, "lab \"x\"\nnode a {\n  bogus\n}\n").unwrap();
        let e = nlink_lab::parser::parse_file(&file).unwrap_err();
        let v = error_json(&e);
        assert_eq!(v["code"], "validation", "{v}");
        assert_eq!(v["exit_code"], 2);
        let issue = &v["issues"][0];
        assert_eq!(issue["rule"], "parse");
        assert!(
            issue["location"].as_str().unwrap().ends_with(":3:3"),
            "{issue}"
        );
    }
}
