//! Whether registrations and generated API consumers have diverged.
//!
//! The registrations in `crates/scripting/src/lib.rs` are authoritative; the
//! catalog `ide/src/mekiki-api.generated.json` is what the IDE reads. After adding an API,
//! update `api/rhai-api.toml` and regenerate. Changing an implementation or a
//! generated consumer alone fails this test.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize)]
struct Catalog {
    globals: Vec<String>,
    members: std::collections::BTreeMap<String, Vec<String>>,
}

fn workspace_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn registered_names(src: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut rest = src;
    while let Some(idx) = rest.find("register_") {
        rest = &rest[idx..];
        let kind = if rest.starts_with("register_fn(") {
            "fn"
        } else if rest.starts_with("register_get(") {
            "get"
        } else {
            rest = &rest["register_".len()..];
            continue;
        };
        let after = &rest[if kind == "fn" {
            "register_fn(".len()
        } else {
            "register_get(".len()
        }..];
        let after = after.trim_start();
        if let Some(name) = after.strip_prefix('"').and_then(|s| s.split('"').next()) {
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
        rest = after;
    }
    names
}

fn registration_function<'a>(src: &'a str, name: &str) -> &'a str {
    let marker = format!("fn {name}(");
    let start = src
        .find(&marker)
        .unwrap_or_else(|| panic!("missing {name}"));
    let body = &src[start..];
    let end = body[marker.len()..]
        .find("\nfn ")
        .map(|offset| marker.len() + offset)
        .unwrap_or(body.len());
    &body[..end]
}

fn catalog_set(names: &[String]) -> BTreeSet<String> {
    names.iter().cloned().collect()
}

fn read_catalog() -> Catalog {
    let json = std::fs::read_to_string(workspace_file("ide/src/mekiki-api.generated.json"))
        .expect("mekiki-api.generated.json is readable");
    serde_json::from_str(&json).expect("the catalog JSON is valid")
}

#[test]
fn catalog_matches_registration_owner_types() {
    let lib = std::fs::read_to_string(workspace_file("crates/scripting/src/lib.rs"))
        .expect("lib.rs is readable");
    let catalog = read_catalog();

    let globals = registered_names(registration_function(&lib, "register_globals"));
    assert_eq!(globals, catalog_set(&catalog.globals), "global API drift");

    let mut target = registered_names(registration_function(&lib, "register_target_builders"));
    target.extend(registered_names(registration_function(
        &lib,
        "register_target_actions",
    )));

    // register_expect contains the Target.expect constructor followed by the
    // Expect methods. Keep that one cross-type overload explicit so moving it
    // between types cannot pass through a flattened name-set comparison.
    let mut expect = registered_names(registration_function(&lib, "register_expect"));
    assert!(
        expect.remove("expect"),
        "Target.expect registration disappeared"
    );
    target.insert("expect".into());

    let expected_members = [
        (
            "Region",
            registered_names(registration_function(&lib, "register_region")),
        ),
        ("Target", target),
        (
            "Match",
            registered_names(registration_function(&lib, "register_match")),
        ),
        ("Expect", expect),
        (
            "WindowExpect",
            registered_names(registration_function(&lib, "register_window_expect")),
        ),
    ];
    assert_eq!(
        catalog.members.len(),
        expected_members.len(),
        "completion catalog has an unknown or missing owner type"
    );
    for (owner, registered) in expected_members {
        let cataloged = catalog
            .members
            .get(owner)
            .unwrap_or_else(|| panic!("completion catalog is missing {owner}"));
        assert_eq!(registered, catalog_set(cataloged), "{owner} API drift");
    }
}

#[test]
fn rustdoc_covers_completion_catalog() {
    let docs = std::fs::read_to_string(workspace_file("api/generated/rhai-api.md"))
        .expect("generated Rhai API Markdown is readable");
    let catalog = read_catalog();

    assert!(
        docs.contains("DO NOT EDIT") && docs.contains("api/rhai-api.toml"),
        "generated Markdown must identify its canonical source"
    );

    for name in &catalog.globals {
        assert!(
            docs.contains(&format!("`{name}(")),
            "Rhai rustdoc is missing global function {name}"
        );
    }

    for (owner, names) in &catalog.members {
        assert!(
            docs.contains(&format!("## {owner}")),
            "Rhai rustdoc is missing the {owner} section"
        );
        for name in names {
            let method = format!("`{name}(");
            let property = format!("`{name}:");
            assert!(
                docs.contains(&method) || docs.contains(&property),
                "Rhai rustdoc is missing {owner}.{name}"
            );
        }
    }
}
