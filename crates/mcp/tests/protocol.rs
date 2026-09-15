//! End-to-end checks over the real stdio transport.
//!
//! The server is started as a child process and driven with hand-written
//! JSON-RPC, which is what an MCP host actually does. Anything short of that —
//! calling the tool functions directly, say — would miss the failures that
//! matter most here: a broken handshake, a schema the macro generated wrongly,
//! or **a stray byte on stdout**, which silently destroys the session and is
//! invisible from inside the process.
//!
//! No tool that touches the desktop is called. These run in CI and on a
//! developer's machine, and a test that moved the mouse would be intolerable in
//! both.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

mod common;

/// A server child process being driven over stdio.
struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// Responses read while waiting for a different id.
    ///
    /// Needed because a test may have two requests in flight — sending `stop`
    /// while `run_script` is still running is the whole point of one of them —
    /// and the server answers concurrently, so the replies can arrive in either
    /// order.
    pending: Vec<serde_json::Value>,
    instructions: String,
}

impl Server {
    fn start(base: &std::path::Path, extra_args: &[&str]) -> Self {
        std::fs::create_dir_all(base).unwrap();

        let mut command = Command::new(server_binary());
        command.arg("--base").arg(base);
        // These tests start many servers and kill some of them. A tray icon
        // per server would litter the notification area with stale icons.
        command.arg("--no-tray");
        command.args(extra_args);
        // Protocol tests never inspect pixels. Force the CPU fallback so a
        // weak or driver-limited GPU cannot dominate their runtime or make an
        // otherwise transport-only test flaky. Dedicated capture tests cover
        // backend selection separately.
        command.env("MEKIKI_CAPTURE", "gdi");
        // The engine logs a lot on startup and none of it should reach the
        // test's own output.
        command.env("RUST_LOG", "error");
        command.env(
            "MEKIKI_DESKTOP_LEASE_NAME",
            format!(
                "Local\\mekiki-mcp-test-{}",
                base.file_name().unwrap().to_string_lossy()
            ),
        );
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = command.spawn().expect("cannot start mekiki-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());

        let mut server = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
            pending: Vec::new(),
            instructions: String::new(),
        };
        server.handshake();
        server
    }

    /// Kill the child if the test is still going after `limit`.
    ///
    /// The interruption tests exist because a bug there means "never returns",
    /// and a blocking `read_line` would turn that into a hung CI job rather than
    /// a failing one. Killing the child closes stdout, which turns the hang into
    /// a clear assertion failure instead.
    fn kill_after(&self, limit: Duration) {
        let pid = self.child.id();
        std::thread::spawn(move || {
            std::thread::sleep(limit);
            #[cfg(windows)]
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F", "/T"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            #[cfg(not(windows))]
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
        });
    }

    fn handshake(&mut self) {
        let init = self.request(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mekiki-tests", "version": "0" }
            }),
        );
        assert_eq!(
            init["result"]["serverInfo"]["name"], "mekiki",
            "unexpected handshake: {init}"
        );
        self.instructions = init["result"]["instructions"]
            .as_str()
            .unwrap_or("")
            .to_string();
        self.notify("notifications/initialized");
    }

    fn send_line(&mut self, value: &serde_json::Value) {
        writeln!(self.stdin, "{value}").expect("cannot write to the server");
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str) {
        let msg = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        self.send_line(&msg);
    }

    /// Send a request without waiting for the answer. Returns its id.
    fn send_request(&mut self, method: &str, params: serde_json::Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send_line(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        id
    }

    /// Read until the response with this id arrives, keeping any others.
    fn await_response(&mut self, id: u64) -> serde_json::Value {
        if let Some(at) = self.pending.iter().position(|v| v["id"] == id) {
            return self.pending.remove(at);
        }

        for _ in 0..100 {
            let mut line = String::new();
            let read = self
                .stdout
                .read_line(&mut line)
                .expect("cannot read from the server");
            assert!(
                read > 0,
                "the server closed stdout while waiting for id {id} \
                 (it may have hung and been killed by the watchdog)"
            );
            if line.trim().is_empty() {
                continue;
            }

            // Every line on stdout has to be JSON-RPC. A log line here would be
            // the bug this assertion exists to catch.
            let value: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("non-JSON on stdout: {e}\nline: {line}"));

            if value["id"] == serde_json::json!(id) {
                return value;
            }
            self.pending.push(value);
        }
        panic!("no response to id {id} after 100 lines");
    }

    /// Send a request and wait for its response.
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.send_request(method, params);
        self.await_response(id)
    }

    fn wait_until_running(&mut self, limit: Duration) {
        let deadline = std::time::Instant::now() + limit;
        loop {
            let result = self.call_tool("status", serde_json::json!({}));
            let status: serde_json::Value =
                serde_json::from_str(&Self::text_of(&result)).expect("status JSON");
            if status["running"] == true {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "script did not enter running state within {limit:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn call_tool(&mut self, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.request(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
    }

    /// The first text block of a tool result.
    fn text_of(result: &serde_json::Value) -> String {
        result["result"]["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .unwrap_or_else(|| panic!("no text block in {result}"))
            .to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn server_binary() -> std::path::PathBuf {
    // The test binary lives next to the binaries built from this package.
    let mut path = std::env::current_exe().expect("cannot locate the test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(format!("mekiki-mcp{}", std::env::consts::EXE_SUFFIX))
}

fn temp_base(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mekiki-mcp-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The live stdio catalog must agree with the source registrations and expose
/// a usable schema. Documentation coverage is checked separately without
/// maintaining another copy of the tool-name list.
#[test]
fn handshake_and_tool_list() {
    let mut server = Server::start(&temp_base("tools"), &[]);
    let listed = server.request("tools/list", serde_json::json!({}));

    let mut names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .expect("no tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();

    let registered: Vec<_> = common::registered_names().into_iter().collect();
    assert_eq!(
        names, registered,
        "the live tool surface and #[tool] registrations differ"
    );

    // Contract checks intentionally avoid serialising the whole schemars
    // output. Exact snapshots are noisy across dependency upgrades; these
    // invariants protect the parts an MCP client actually plans around.
    for tool in listed["result"]["tools"].as_array().unwrap() {
        assert!(
            tool["description"].as_str().is_some_and(|s| s.len() > 40),
            "{} has no useful description: {:?}",
            tool["name"],
            tool["description"]
        );
        let schema = &tool["inputSchema"];
        assert_eq!(
            schema["type"], "object",
            "{} schema: {schema}",
            tool["name"]
        );
        let properties = schema["properties"].as_object().expect("properties object");
        if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
            for field in required {
                let field = field.as_str().unwrap();
                assert!(
                    properties.contains_key(field),
                    "{} requires absent property {field}",
                    tool["name"]
                );
            }
        }
    }

    let required = |name: &str| -> std::collections::BTreeSet<String> {
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()["inputSchema"]["required"]
            .as_array()
            .map(|fields| {
                fields
                    .iter()
                    .map(|field| field.as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(required("find"), ["locator".into()].into_iter().collect());
    assert_eq!(
        required("read_value"),
        ["locator".into(), "scope".into()].into_iter().collect()
    );
    assert_eq!(required("launch"), ["program".into()].into_iter().collect());
    assert_eq!(
        required("run_script"),
        ["source".into()].into_iter().collect()
    );
    assert_eq!(
        required("save_script"),
        ["name".into(), "source".into()].into_iter().collect()
    );
}

#[test]
fn rhai_api_is_available_as_resources_and_a_search_tool() {
    let mut server = Server::start(&temp_base("rhai-api"), &["--observe"]);

    let listed = server.request("resources/list", serde_json::json!({}));
    let resources = listed["result"]["resources"]
        .as_array()
        .expect("resources array");
    let uris: Vec<_> = resources
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"mekiki://rhai-api/reference.md"), "{listed}");
    assert!(uris.contains(&"mekiki://rhai-api/catalog.json"), "{listed}");

    let reference = server.request(
        "resources/read",
        serde_json::json!({ "uri": "mekiki://rhai-api/reference.md" }),
    );
    let text = reference["result"]["contents"][0]["text"]
        .as_str()
        .expect("reference text");
    assert!(text.contains("Target"), "{reference}");
    assert!(text.contains("click() -> Match"), "{reference}");

    let result = server.call_tool("script_api", serde_json::json!({ "query": "Target.click" }));
    let text = Server::text_of(&result);
    assert!(text.contains("Target.click() -> Match"), "{result}");
    assert!(text.contains("left-click"), "{result}");
}

#[test]
fn status_reports_safety_and_runtime_state() {
    let mut server = Server::start(&temp_base("status"), &["--observe"]);
    assert!(
        server.instructions.contains("could NOT be registered"),
        "{}",
        server.instructions
    );
    assert!(!server.instructions.contains("A human can stop everything"));

    let result = server.call_tool("status", serde_json::json!({}));
    let status: serde_json::Value = serde_json::from_str(&Server::text_of(&result)).unwrap();
    assert_eq!(status["emergency_stop_armed"], false);
    assert_eq!(status["running"], false);
    assert_eq!(status["busy"], false);
    assert!(status["capture_backend"].is_string(), "{status}");
    assert!(status["desktop_lease"].is_string(), "{status}");
    assert!(status["desktop_accessible"].is_boolean(), "{status}");
}

#[test]
fn second_server_is_automatically_observation_only() {
    let base = temp_base("desktop-lease");
    let mut owner = Server::start(&base, &[]);
    let owner_status = owner.call_tool("status", serde_json::json!({}));
    assert!(Server::text_of(&owner_status).contains("\"owner\""));

    let mut observer = Server::start(&base, &[]);
    assert!(observer.instructions.contains("observation-only"));
    let observer_status = observer.call_tool("status", serde_json::json!({}));
    assert!(Server::text_of(&observer_status).contains("\"observer\""));
    let listed = observer.request("tools/list", serde_json::json!({}));
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"run_script"), "{names:?}");
    assert!(names.contains(&"status"), "{names:?}");
}

/// The handover log has to survive a round trip through the protocol, since
/// that is the only way an agent ever touches it.
#[test]
fn notes_round_trip_through_the_protocol() {
    let base = temp_base("notes");
    let mut server = Server::start(&base, &[]);

    let posted = server.call_tool(
        "note_post",
        serde_json::json!({
            "agent": "test-agent",
            "kind": "limitation",
            "title": "no way to enumerate the UI tree",
            "body": "ui: can search but not list",
            "task": "explore"
        }),
    );
    assert!(
        Server::text_of(&posted).contains("recorded as limitation"),
        "{posted}"
    );

    let listed = server.call_tool("note_list", serde_json::json!({}));
    let notes: serde_json::Value = serde_json::from_str(&Server::text_of(&listed)).unwrap();
    assert_eq!(notes.as_array().unwrap().len(), 1, "{notes}");
    assert_eq!(notes[0]["title"], "no way to enumerate the UI tree");
    assert_eq!(notes[0]["kind"], "limitation");

    // Filtering by a kind that was never posted must come back empty rather
    // than returning everything.
    let filtered = server.call_tool("note_list", serde_json::json!({ "kind": "security" }));
    let notes: serde_json::Value = serde_json::from_str(&Server::text_of(&filtered)).unwrap();
    assert!(notes.as_array().unwrap().is_empty(), "{notes}");
}

/// A note written by one server has to be readable by the next one. Agents take
/// turns as separate processes, so a log that only worked in-process would be
/// useless for the thing it exists to do.
#[test]
fn notes_survive_a_server_restart() {
    let base = temp_base("notes-restart");

    {
        let mut first = Server::start(&base, &[]);
        first.call_tool(
            "note_post",
            serde_json::json!({
                "agent": "first",
                "kind": "workaround",
                "title": "click the label, not the checkbox"
            }),
        );
    }

    let mut second = Server::start(&base, &[]);
    let listed = second.call_tool("note_list", serde_json::json!({}));
    let notes: serde_json::Value = serde_json::from_str(&Server::text_of(&listed)).unwrap();
    assert_eq!(notes.as_array().unwrap().len(), 1, "{notes}");
    assert_eq!(notes[0]["agent"], "first");

    // The id has to keep going up, or replies_to would point at two notes.
    let posted = second.call_tool(
        "note_post",
        serde_json::json!({ "agent": "second", "kind": "answer", "title": "confirmed", "replies_to": 1 }),
    );
    assert!(Server::text_of(&posted).contains("note 2"), "{posted}");
}

/// Compiling is the inner loop of writing a script, and it must not need a
/// screen, a GPU or a window.
#[test]
fn check_script_reports_syntax_errors() {
    let mut server = Server::start(&temp_base("check"), &[]);

    let ok = server.call_tool(
        "check_script",
        serde_json::json!({ "source": "let x = 1; print(x);" }),
    );
    assert!(
        Server::text_of(&ok).to_lowercase().contains("syntax ok"),
        "{ok}"
    );

    let bad = server.call_tool(
        "check_script",
        serde_json::json!({ "source": "let x = ;;;" }),
    );
    // A script that does not compile is a normal answer carrying a diagnosis,
    // not a protocol error.
    assert_eq!(bad["result"]["isError"], serde_json::json!(true), "{bad}");
    assert!(Server::text_of(&bad).contains("does not compile"), "{bad}");
}

#[test]
fn save_script_writes_under_the_base_directory() {
    let base = temp_base("save");
    let mut server = Server::start(&base, &[]);

    let saved = server.call_tool(
        "save_script",
        serde_json::json!({ "name": "demo", "source": "print(\"hi\");" }),
    );
    assert!(Server::text_of(&saved).contains("saved to"), "{saved}");

    let written =
        std::fs::read_to_string(base.join("demo.rhai")).expect("the file was not written");
    assert_eq!(written, "print(\"hi\");");
}

/// `save_script` exists so hosts without file tools can leave a deliverable.
/// That is not a reason to hand out arbitrary writes to the filesystem.
#[test]
fn save_script_refuses_to_escape_the_base_directory() {
    let mut server = Server::start(&temp_base("save-escape"), &[]);

    for name in ["../outside", "sub/dir", "..\\outside", "C:\\Windows\\evil"] {
        let result = server.call_tool(
            "save_script",
            serde_json::json!({ "name": name, "source": "x" }),
        );
        assert!(
            result.get("error").is_some(),
            "'{name}' should have been rejected: {result}"
        );
    }
}

/// `--observe` is the read-only mode. It defaults off during the test phase,
/// but when it is on, nothing may move the desktop.
#[test]
fn observe_mode_refuses_to_act() {
    let mut server = Server::start(&temp_base("observe"), &["--observe"]);

    let typed = server.call_tool(
        "type_text",
        serde_json::json!({ "text": "should not happen" }),
    );
    assert!(
        typed.get("error").is_some(),
        "type_text must be refused under --observe: {typed}"
    );

    // Perception and the handover log stay available: the mode restricts what
    // can be changed, not what can be learned.
    let listed = server.call_tool("note_list", serde_json::json!({}));
    assert!(listed.get("error").is_none(), "{listed}");
}

/// Raw input must name its target. Three agent test rounds ended with keystrokes
/// landing in a window the agent had not named — the last time in a stale
/// message box, where the letters pressed its buttons — and every tool reported
/// success. An omitted scope is an error now, not a silent fallback, and the
/// error has to say what to pass instead.
///
/// Nothing here touches the desktop: every call is refused before any input is
/// sent, which is the point.
#[test]
fn raw_input_without_a_scope_is_refused() {
    let mut server = Server::start(&temp_base("raw-scope"), &[]);

    // Omitting the argument is the mistake an agent actually makes, so it has
    // to get the message that names the fix — not serde's bare
    // "missing field `scope`". The field is optional in the schema and the
    // refusal is ours, precisely so both spellings land here.
    for (tool, args) in [
        ("type_text", serde_json::json!({ "text": "hello" })),
        ("press", serde_json::json!({ "keys": "enter" })),
        ("scroll", serde_json::json!({ "v": 1 })),
    ] {
        let result = server.call_tool(tool, args);
        assert!(
            result.get("error").is_some(),
            "{tool} without scope must be refused: {result}"
        );
        let text = result.to_string();
        assert!(
            text.contains("scope is required") && text.contains("focused"),
            "{tool}: the refusal must explain window:/focused: {result}"
        );
    }

    // An empty scope is the same mistake spelled differently. The message has
    // to name the escape hatch, because "pass a scope" alone sends an agent
    // that really does want the focused window back into guessing.
    let result = server.call_tool(
        "press",
        serde_json::json!({ "keys": "enter", "scope": "   " }),
    );
    assert!(
        result.get("error").is_some(),
        "an empty scope must be refused: {result}"
    );
    let text = result.to_string();
    assert!(
        text.contains("scope is required") && text.contains("focused"),
        "the error must say scope is required and mention focused: {result}"
    );

    // A region is not a keyboard target either; the refusal comes from the
    // engine and must say so rather than typing somewhere.
    let result = server.call_tool(
        "press",
        serde_json::json!({ "keys": "enter", "scope": "region:0,0,10,10" }),
    );
    assert!(
        result.get("error").is_some(),
        "a region scope must be refused: {result}"
    );
    assert!(
        result.to_string().contains("cannot receive input"),
        "the error must explain why a region is refused: {result}"
    );
}

/// Under --observe the desktop-moving tools are gone from the catalog, not just
/// refused. An agent plans around what tools/list shows it.
#[test]
fn observe_mode_hides_the_action_tools_from_the_listing() {
    let mut server = Server::start(&temp_base("observe-list"), &["--observe"]);
    let listed = server.request("tools/list", serde_json::json!({}));

    let names: Vec<String> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();

    for gone in [
        "click",
        "hover",
        "type_text",
        "press",
        "scroll",
        "run_script",
    ] {
        assert!(
            !names.contains(&gone.to_string()),
            "--observe should not list {gone}: {names:?}"
        );
    }
    // The perception and handover tools remain.
    for kept in ["read_text", "ui_tree", "find", "note_list", "status"] {
        assert!(
            names.contains(&kept.to_string()),
            "--observe should still list {kept}: {names:?}"
        );
    }
}

/// An unknown tool has to fail cleanly rather than take the session down.
#[test]
fn an_unknown_tool_is_an_error_not_a_crash() {
    let mut server = Server::start(&temp_base("unknown"), &[]);

    let result = server.call_tool("no_such_tool", serde_json::json!({}));
    assert!(result.get("error").is_some(), "{result}");

    // The session has to still work afterwards.
    let listed = server.request("tools/list", serde_json::json!({}));
    assert!(listed["result"]["tools"].is_array(), "{listed}");
}

/// Arguments that do not match the schema are the agent's mistake to fix, and
/// it can only fix what it is told about.
///
/// rmcp reports these as a **tool result with `isError`**, not as a JSON-RPC
/// error — which is the MCP-correct split: the protocol worked, the call did
/// not. What matters is that the message names the offending field.
#[test]
fn bad_arguments_are_rejected_with_a_message() {
    let mut server = Server::start(&temp_base("bad-args"), &[]);

    // `title` is required.
    let result = server.call_tool(
        "note_post",
        serde_json::json!({ "agent": "x", "kind": "finding" }),
    );
    assert_eq!(
        result["result"]["isError"],
        serde_json::json!(true),
        "{result}"
    );
    assert!(
        Server::text_of(&result).contains("title"),
        "the message must name the missing field: {result}"
    );

    // `kind` has to be one of the known values.
    let result = server.call_tool(
        "note_post",
        serde_json::json!({ "agent": "x", "kind": "not-a-kind", "title": "t" }),
    );
    assert_eq!(
        result["result"]["isError"],
        serde_json::json!(true),
        "{result}"
    );
    assert!(
        Server::text_of(&result).contains("kind"),
        "the message must name the offending field: {result}"
    );

    // Nothing was written despite the failed calls.
    let listed = server.call_tool("note_list", serde_json::json!({}));
    let notes: serde_json::Value = serde_json::from_str(&Server::text_of(&listed)).unwrap();
    assert!(notes.as_array().unwrap().is_empty(), "{notes}");
}

/// stdout carries the protocol and nothing else. A log line, a `println!` or a
/// panic message there breaks every host, and nothing inside the process would
/// notice.
#[test]
fn stdout_carries_only_json_rpc() {
    let mut server = Server::start(&temp_base("stdout"), &[]);

    // Exercise several tools, including ones whose engine work logs on stderr.
    server.request("tools/list", serde_json::json!({}));
    server.call_tool(
        "check_script",
        serde_json::json!({ "source": "let x = 1;" }),
    );
    server.call_tool("note_list", serde_json::json!({}));
    server.call_tool("no_such_tool", serde_json::json!({}));

    // `request` parses every line it reads as JSON and panics otherwise, so
    // getting here having read all of those is the assertion. One more round
    // trip confirms the stream is still aligned.
    let final_check = server.request("tools/list", serde_json::json!({}));
    assert!(final_check["result"]["tools"].is_array());
}

// ---------------------------------------------------------------------------
// Interruption
//
// The first agent test found that the timeout, the `stop` tool and the
// emergency hotkey all raised a flag the running script never looked at, so a
// script that never called the engine could not be stopped by anything short of
// killing the process. Every other test in this file was green while that was
// true, because none of them ran a script that would not stop on its own.
//
// These need no desktop: a Rhai loop that never touches the engine is exactly
// the case that failed.
// ---------------------------------------------------------------------------

/// The deadline has to reach a script that never calls the engine.
///
/// A pure-compute loop is the hard case: the engine's own wait loops are never
/// entered, so the only thing that can stop it is Rhai's `on_progress`, which
/// looks at whichever flag was installed on the script host.
#[test]
fn a_pure_compute_loop_is_stopped_by_the_timeout() {
    let mut server = Server::start(&temp_base("timeout"), &[]);
    server.kill_after(Duration::from_secs(30));

    let started = std::time::Instant::now();
    let result = server.call_tool(
        "run_script",
        serde_json::json!({
            "source": "let x = 0; loop { x += 1; }",
            "timeout_ms": 500
        }),
    );
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(20),
        "run_script did not come back after the deadline ({elapsed:?})"
    );
    let text = Server::text_of(&result);
    assert!(
        text.contains("(timeout)"),
        "the result must say it was stopped by the timeout: {text}"
    );
}

/// `stop` has to reach a script that is already running.
///
/// This is the tool a human reaches for through the agent, and the hotkey raises
/// the same flag, so it stands in for both.
#[test]
fn stop_interrupts_a_running_script() {
    let mut server = Server::start(&temp_base("stop"), &[]);
    server.kill_after(Duration::from_secs(40));

    // Long enough that finishing on its own would be indistinguishable from
    // being stopped, but under the server's own 120s ceiling.
    let run_id = server.send_request(
        "tools/call",
        serde_json::json!({
            "name": "run_script",
            "arguments": { "source": "sleep(60000);" }
        }),
    );

    // Poll the state instead of assuming a fast workstation can start the
    // worker within a fixed 700ms. This remains deterministic on a weak laptop.
    server.wait_until_running(Duration::from_secs(30));

    let started = std::time::Instant::now();
    let stopped = server.call_tool("stop", serde_json::json!({}));
    assert!(
        Server::text_of(&stopped).contains("stop requested"),
        "{stopped}"
    );

    let result = server.await_response(run_id);
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(20),
        "the script kept running after stop ({elapsed:?})"
    );
    let text = Server::text_of(&result);
    assert!(
        text.contains("stopped on request"),
        "the result must say it was stopped: {text}"
    );
}

/// Being stopped must leave the server usable.
///
/// The busy guard is what makes a leak here so bad: if it is not released, every
/// engine tool answers `busy` forever and the only fix is killing the process —
/// which is exactly the state the interrupt bug produced.
#[test]
fn the_server_still_works_after_a_script_is_stopped() {
    let mut server = Server::start(&temp_base("recover"), &[]);
    server.kill_after(Duration::from_secs(40));

    let result = server.call_tool(
        "run_script",
        serde_json::json!({
            "source": "let x = 0; loop { x += 1; }",
            "timeout_ms": 500
        }),
    );
    assert!(Server::text_of(&result).contains("(timeout)"), "{result}");

    // `check_script` goes through the same queue and the same busy guard as
    // every other engine tool, so it fails if the guard leaked.
    let checked = server.call_tool(
        "check_script",
        serde_json::json!({ "source": "let x = 1;" }),
    );
    let text = Server::text_of(&checked);
    assert!(
        !text.contains("busy"),
        "the busy guard was not released after the interruption: {text}"
    );
    assert_ne!(
        checked["result"]["isError"],
        serde_json::json!(true),
        "a valid script failed to check after the interruption: {checked}"
    );

    // And a second script has to run normally: the stop must not have stuck to
    // the shared flag.
    let again = server.call_tool(
        "run_script",
        serde_json::json!({ "source": "print(\"after\");" }),
    );
    let text = Server::text_of(&again);
    assert!(text.contains("after"), "the second run did not run: {text}");
    assert!(
        !text.contains("stopped"),
        "the second run inherited the earlier stop: {text}"
    );
}

/// The server must not die when the host disconnects, nor hang forever.
#[test]
fn closing_stdin_shuts_the_server_down() {
    let base = temp_base("shutdown");
    let mut server = Server::start(&base, &[]);
    server.request("tools/list", serde_json::json!({}));

    // Dropping stdin is how a host signals it is done.
    let stdin = std::mem::replace(&mut server.stdin, {
        let mut placeholder = Command::new(server_binary())
            .arg("--help")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let s = placeholder.stdin.take().unwrap();
        let _ = placeholder.kill();
        let _ = placeholder.wait();
        s
    });
    drop(stdin);

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match server.child.try_wait().unwrap() {
            Some(_) => break,
            None if std::time::Instant::now() > deadline => {
                panic!("the server did not exit within 15s of stdin closing")
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}
