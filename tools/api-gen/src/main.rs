use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const MASTER: &str = "api/rhai-api.toml";
const MARKDOWN: &str = "api/generated/rhai-api.md";
const JSON: &str = "ide/src/mekiki-api.generated.json";
const GENERATED_NOTICE: &str =
    "Generated from api/rhai-api.toml by `cargo run -p mekiki-api-gen`. DO NOT EDIT.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    api_version: String,
    title: String,
    introduction: String,
    #[serde(default)]
    guides: Vec<Guide>,
    types: Vec<ApiType>,
    items: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Guide {
    title: String,
    body: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApiType {
    id: String,
    name: String,
    summary: String,
    #[serde(default)]
    details: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Item {
    id: String,
    #[serde(default)]
    owner: String,
    name: String,
    kind: ItemKind,
    #[serde(default)]
    params: Vec<Parameter>,
    returns: String,
    summary: String,
    #[serde(default)]
    details: String,
    #[serde(default)]
    constraints: Vec<String>,
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    examples: Vec<String>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ItemKind {
    Function,
    Method,
    Property,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Parameter {
    name: String,
    #[serde(rename = "type")]
    type_name: String,
    #[serde(default)]
    description: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JsonCatalog<'a> {
    _generated: &'static str,
    schema_version: u32,
    api_version: &'a str,
    globals: Vec<&'a str>,
    members: BTreeMap<&'a str, Vec<&'a str>>,
    types: &'a [ApiType],
    items: &'a [Item],
}

fn main() {
    if let Err(error) = run() {
        eprintln!("api-gen: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let check = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--check") => true,
        Some(other) => return Err(format!("unknown argument '{other}' (expected --check)")),
    };
    let root = workspace_root()?;
    let master_path = root.join(MASTER);
    let source = std::fs::read_to_string(&master_path)
        .map_err(|e| format!("cannot read {}: {e}", master_path.display()))?;
    let catalog: Catalog = toml::from_str(&source)
        .map_err(|e| format!("{} is invalid: {e}", master_path.display()))?;
    validate(&catalog)?;

    let markdown = render_markdown(&catalog);
    let json = render_json(&catalog)?;
    update(&root.join(MARKDOWN), &markdown, check)?;
    update(&root.join(JSON), &json, check)?;
    Ok(())
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .ok_or_else(|| "cannot locate workspace root".to_string())
}

fn validate(catalog: &Catalog) -> Result<(), String> {
    if catalog.schema_version != 1 {
        return Err(format!(
            "unsupported schema_version {} (expected 1)",
            catalog.schema_version
        ));
    }
    let type_names: BTreeSet<_> = catalog
        .types
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    if type_names.len() != catalog.types.len() {
        return Err("type names must be unique".into());
    }
    let mut ids = BTreeSet::new();
    for item in &catalog.items {
        if !ids.insert(item.id.as_str()) {
            return Err(format!("duplicate item id '{}'", item.id));
        }
        match item.kind {
            ItemKind::Function if !item.owner.is_empty() => {
                return Err(format!("global function '{}' has an owner", item.id));
            }
            ItemKind::Method | ItemKind::Property if !type_names.contains(item.owner.as_str()) => {
                return Err(format!("'{}' has unknown owner '{}'", item.id, item.owner));
            }
            _ => {}
        }
        if matches!(item.kind, ItemKind::Property) && !item.params.is_empty() {
            return Err(format!("property '{}' cannot have parameters", item.id));
        }
        if item.summary.trim().is_empty() {
            return Err(format!("'{}' needs a summary", item.id));
        }
    }
    let prose = format!(
        "{}\n{}\n{}\n{}",
        catalog.introduction,
        catalog
            .guides
            .iter()
            .map(|guide| format!("{}\n{}", guide.title, guide.body))
            .collect::<Vec<_>>()
            .join("\n"),
        catalog
            .types
            .iter()
            .map(|item| format!("{}\n{}", item.summary, item.details))
            .collect::<Vec<_>>()
            .join("\n"),
        catalog
            .items
            .iter()
            .map(|item| {
                format!(
                    "{}\n{}\n{}\n{}\n{}",
                    item.summary,
                    item.details,
                    item.constraints.join("\n"),
                    item.errors.join("\n"),
                    item.params
                        .iter()
                        .map(|param| param.description.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    );
    if prose.chars().any(is_japanese) {
        return Err("public API prose must be English; Japanese text was found".into());
    }
    Ok(())
}

fn is_japanese(ch: char) -> bool {
    matches!(ch, '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}')
}

fn render_markdown(catalog: &Catalog) -> String {
    let mut out = String::new();
    writeln!(out, "<!-- {GENERATED_NOTICE} -->\n").unwrap();
    writeln!(out, "# {}\n", catalog.title).unwrap();
    writeln!(out, "{}\n", catalog.introduction.trim()).unwrap();
    for guide in &catalog.guides {
        writeln!(out, "## {}\n\n{}\n", guide.title, guide.body.trim()).unwrap();
    }
    writeln!(out, "## Global functions\n").unwrap();
    for item in catalog.items.iter().filter(|item| item.owner.is_empty()) {
        render_item(&mut out, item, 3);
    }
    for api_type in &catalog.types {
        writeln!(out, "## {}\n\n{}\n", api_type.name, api_type.summary).unwrap();
        if !api_type.details.is_empty() {
            writeln!(out, "{}\n", api_type.details.trim()).unwrap();
        }
        for item in catalog
            .items
            .iter()
            .filter(|item| item.owner == api_type.name)
        {
            render_item(&mut out, item, 3);
        }
    }
    out
}

fn render_item(out: &mut String, item: &Item, level: usize) {
    let heading = "#".repeat(level);
    writeln!(out, "{heading} `{}`\n", signature(item)).unwrap();
    writeln!(out, "{}\n", item.summary.trim()).unwrap();
    if !item.details.is_empty() {
        writeln!(out, "{}\n", item.details.trim()).unwrap();
    }
    if !item.constraints.is_empty() {
        writeln!(out, "**Constraints**\n").unwrap();
        for value in &item.constraints {
            writeln!(out, "- {value}").unwrap();
        }
        out.push('\n');
    }
    if !item.errors.is_empty() {
        writeln!(out, "**Errors**\n").unwrap();
        for value in &item.errors {
            writeln!(out, "- {value}").unwrap();
        }
        out.push('\n');
    }
    for example in &item.examples {
        writeln!(out, "```rhai\n{}\n```\n", example.trim()).unwrap();
    }
}

fn signature(item: &Item) -> String {
    if matches!(item.kind, ItemKind::Property) {
        return format!("{}: {}", item.name, item.returns);
    }
    let params = item
        .params
        .iter()
        .map(|param| format!("{}: {}", param.name, param.type_name))
        .collect::<Vec<_>>()
        .join(", ");
    if item.returns == "unit" {
        format!("{}({params})", item.name)
    } else {
        format!("{}({params}) -> {}", item.name, item.returns)
    }
}

fn render_json(catalog: &Catalog) -> Result<String, String> {
    let mut globals = BTreeSet::new();
    let mut members: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for item in &catalog.items {
        if item.owner.is_empty() {
            globals.insert(item.name.as_str());
        } else {
            members
                .entry(item.owner.as_str())
                .or_default()
                .insert(item.name.as_str());
        }
    }
    let output = JsonCatalog {
        _generated: GENERATED_NOTICE,
        schema_version: catalog.schema_version,
        api_version: &catalog.api_version,
        globals: globals.into_iter().collect(),
        members: members
            .into_iter()
            .map(|(owner, names)| (owner, names.into_iter().collect()))
            .collect(),
        types: &catalog.types,
        items: &catalog.items,
    };
    serde_json::to_string_pretty(&output)
        .map(|mut value| {
            value.push('\n');
            value
        })
        .map_err(|e| format!("cannot render JSON: {e}"))
}

fn update(path: &Path, expected: &str, check: bool) -> Result<(), String> {
    let current = std::fs::read_to_string(path).ok();
    if current.as_deref() == Some(expected) {
        return Ok(());
    }
    if check {
        return Err(format!(
            "{} is stale; run `cargo run -p mekiki-api-gen`",
            path.display()
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, expected).map_err(|e| format!("cannot write {}: {e}", path.display()))
}
