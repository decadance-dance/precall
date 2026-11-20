//! The built-in judges of spec §10: `support`, `removal` and `duplicate`. Each is declarative, a
//! manifest and a prompt compiled into the binary, run by the user config's default runner
//! (or the instance's `runner`) through the judge protocol of §6.5.

/// The built-in manifests. They name no runner, so the user config's default applies (§6.8).
pub const MANIFESTS: [&str; 3] = [
    r#"id = "support"
name = "Support: rejects a claim its sources do not back, and a rewording"
version = "0.1.0"
min_precall = "0.1.0"
kind = "judge"

[judge]
prompt = "support.md"
ops = ["add", "update", "add_source"]
"#,
    r#"id = "removal"
name = "Removal: checks that a removal's evidence says what it claims"
version = "0.1.0"
min_precall = "0.1.0"
kind = "judge"

[judge]
prompt = "removal.md"
ops = ["remove"]
"#,
    r#"id = "duplicate"
name = "Duplicate: rejects an addition that restates an existing node"
version = "0.1.0"
min_precall = "0.1.0"
kind = "judge"

[judge]
prompt = "duplicate.md"
ops = ["add"]
"#,
];

/// The prompt files of the built-in judges, as (plugin id, file name, text).
pub const PROMPTS: [(&str, &str, &str); 3] = [
    ("support", "support.md", include_str!("judges/support.md")),
    ("removal", "removal.md", include_str!("judges/removal.md")),
    (
        "duplicate",
        "duplicate.md",
        include_str!("judges/duplicate.md"),
    ),
];
