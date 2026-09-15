# Rhai API maintenance

`rhai-api.toml` is the canonical public contract for the Rhai API. It is the
only place where API names, signatures, descriptions, constraints, errors, and
examples are maintained by hand.

After editing the master, regenerate its consumers:

```text
cargo run -p mekiki-api-gen
```

Check that committed outputs are current without changing files:

```text
cargo run -p mekiki-api-gen -- --check
```

Generated outputs are deliberately limited to:

- `api/generated/rhai-api.md`, embedded by rustdoc and the MCP server
- `ide/src/mekiki-api.generated.json`, consumed by IDE completion and the MCP server

Both outputs identify their source and say `DO NOT EDIT`. Never repair a drift
test by editing an output. Update `rhai-api.toml`, regenerate, and review all
resulting changes. Runtime behavior remains Rust code; registration/catalog
conformance tests connect that implementation to this public contract.
