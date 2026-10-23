//! The self-tests of [`crate::schema_check`], here rather than in it:
//! `tests/integration.rs` includes that file by path, and its tests would
//! run in both binaries.

use serde_json::json;

use crate::schema_check::violations;

/// The check itself, where it must fail: each keyword it implements
/// rejects an instance that breaks it.
#[test]
fn each_keyword_rejects_what_breaks_it() {
    let schema = json!({
        "type": "object",
        "required": ["key"],
        "properties": {
            "key": { "type": "string" },
            "port": { "type": "integer", "minimum": 0, "maximum": 65535 },
            "keys": { "type": "array", "items": { "$ref": "#/definitions/Key" } },
            "pair": { "type": "array", "items": [{ "type": "string" }, { "type": "boolean" }], "maxItems": 2 },
            "kind": { "oneOf": [{ "const": "a" }, { "enum": ["b", "c"] }] },
        },
        "additionalProperties": false,
        "definitions": { "Key": { "type": "string", "minLength": 44, "maxLength": 44 } },
    });
    let good = json!({ "key": "k", "port": 22, "keys": ["x".repeat(44)], "pair": ["a", true], "kind": "c" });
    assert_eq!(violations(&schema, &good), Vec::<String>::new());

    for (bad, why) in [
        (json!({ "port": 1 }), "missing required"),
        (json!({ "key": [1, 2] }), "is not of type string"),
        (json!({ "key": "k", "port": 70000 }), "breaks maximum"),
        (json!({ "key": "k", "port": 1.5 }), "is not of type integer"),
        (
            json!({ "key": "k", "keys": [[1, 2]] }),
            "$.keys[0]: [1,2] is not of type string",
        ),
        (json!({ "key": "k", "keys": ["short"] }), "breaks minLength"),
        (json!({ "key": "k", "pair": ["a", "b"] }), "$.pair[1]"),
        (
            json!({ "key": "k", "pair": ["a", true, 1] }),
            "break maxItems",
        ),
        (
            json!({ "key": "k", "kind": "d" }),
            "matches none of its oneOf",
        ),
        (
            json!({ "key": "k", "extra": 1 }),
            "$.extra: the schema allows nothing here",
        ),
    ] {
        let errors = violations(&schema, &bad);
        assert!(
            errors.iter().any(|e| e.contains(why)),
            "{bad} should fail with `{why}`: {errors:?}"
        );
    }
}

/// Draft 2020-12's tuple: `prefixItems` per position, `items` after.
#[test]
fn prefix_items_check_each_position_and_items_the_rest() {
    let schema = json!({
        "type": "array",
        "prefixItems": [{ "type": "string" }, { "$ref": "#/$defs/Flag" }],
        "items": { "type": "integer" },
        "$defs": { "Flag": { "type": "boolean" } },
    });
    assert_eq!(
        violations(&schema, &json!(["a", true, 1, 2])),
        Vec::<String>::new()
    );
    let errors = violations(&schema, &json!(["a", "b", 1, "c"]));
    assert!(
        errors.iter().any(|e| e.starts_with("$[1]"))
            && errors.iter().any(|e| e.starts_with("$[3]")),
        "{errors:?}"
    );
}

#[test]
#[should_panic(expected = "does not know the keyword `pattern`")]
fn an_unknown_keyword_panics_rather_than_passes() {
    violations(&json!({ "type": "string", "pattern": "^a" }), &json!("b"));
}
