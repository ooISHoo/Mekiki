use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub fn workspace_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("cannot find the workspace root")
        .join(relative)
}

/// Tool names as `#[tool(...)]` registers them: the function name.
///
/// Both the live protocol contract and the documentation contract use this
/// parser. Keeping one implementation prevents the two tests from silently
/// disagreeing about what counts as a tool.
pub fn registered_names() -> BTreeSet<String> {
    let source = std::fs::read_to_string(workspace_file("crates/mcp/src/server.rs"))
        .expect("server.rs is readable");
    let mut names = BTreeSet::new();
    let mut expecting = false;

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#[tool(") {
            expecting = true;
            continue;
        }
        if !expecting {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("async fn ") {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.insert(name);
            }
            expecting = false;
        }
    }
    assert!(
        names.len() >= 10,
        "the parser found too few tools ({}); it has probably stopped matching",
        names.len()
    );
    names
}
