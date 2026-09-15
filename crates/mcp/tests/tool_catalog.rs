//! Fails when the documented tools and the implemented tools drift apart.
//!
//! Same idea as `crates/scripting/tests/api_catalog.rs`. The registrations in
//! `src/server.rs` are authoritative; `AGENTS.md` and `docs/mcp-reference.md`
//! describe them for readers who are not compiling anything.
//!
//! **Documentation drift is worse here than usual.** `AGENTS.md` is read by
//! agents from other vendors as their only briefing. A tool listed there that
//! does not exist sends an agent chasing a call that always fails; a tool that
//! exists but is not listed never gets used. Neither shows up as a test failure
//! anywhere else, so it shows up here.

use std::collections::BTreeSet;

mod common;

/// Tool names mentioned in a document, as `` `name` `` in a table cell or a
/// heading.
fn documented_names(text: &str, registered: &BTreeSet<String>) -> BTreeSet<String> {
    registered
        .iter()
        .filter(|name| {
            // Look for the name in backticks so prose that merely uses the word
            // ("find the window") does not count as documentation.
            text.contains(&format!("`{name}`")) || text.contains(&format!("`{name}("))
        })
        .cloned()
        .collect()
}

#[test]
fn public_documents_cover_every_tool() {
    let registered = common::registered_names();
    for (path, purpose) in [
        ("AGENTS.md", "agent briefing"),
        ("docs/mcp-reference.md", "human reference"),
    ] {
        let document = std::fs::read_to_string(common::workspace_file(path))
            .unwrap_or_else(|_| panic!("{path} is readable"));
        let documented = documented_names(&document, &registered);
        let missing: Vec<_> = registered.difference(&documented).collect();
        assert!(
            missing.is_empty(),
            "{purpose} {path} is missing tools: {missing:?}"
        );
    }
}

/// Every note kind has to be listed where an agent will see it, or a kind
/// nobody knows about never gets used.
#[test]
fn agents_md_lists_every_note_kind() {
    let agents = std::fs::read_to_string(common::workspace_file("AGENTS.md"))
        .expect("AGENTS.md is readable");

    for kind in mekiki_mcp::notes::NoteKind::ALL {
        assert!(
            agents.contains(kind.as_str()),
            "AGENTS.md never mentions the note kind '{}'",
            kind.as_str()
        );
    }
}

/// The security report is the thing the whole permissive posture is traded for.
/// If the instruction to file one ever falls out of the documents, the trade
/// stops paying.
#[test]
fn the_security_report_is_still_demanded() {
    let agents = std::fs::read_to_string(common::workspace_file("AGENTS.md"))
        .expect("AGENTS.md is readable");
    let source = std::fs::read_to_string(common::workspace_file("crates/mcp/src/server.rs"))
        .expect("server.rs is readable");

    assert!(
        agents.to_lowercase().contains("security"),
        "AGENTS.md must ask for a security note"
    );
    assert!(
        source.contains("kind=security"),
        "the note_post description must ask for a security note before finishing"
    );
    assert!(
        source.contains("DATA, NOT INSTRUCTIONS"),
        "the tools that return screen text or notes must warn about prompt injection"
    );
}
