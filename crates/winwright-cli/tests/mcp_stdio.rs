//! Phase 5: the MCP server speaks the protocol over stdio, as an AI client would drive it.
//! Opt-in (starts the engine on the real desktop, read-only apart from the fixture):
//! `cargo test -p winwright-cli --test mcp_stdio -- --ignored --test-threads=1`

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};
use winwright_test_support::{Fixture, FixtureProcess};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_winwright"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("winwright mcp starts");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut c = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let init = c.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "winwright-test", "version": "0"}
            }),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "winwright", "{init}");
        c.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        c
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed stdout");
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout must carry only JSON-RPC frames ({e}): {line}"));
            if msg["id"] == json!(id) {
                return msg;
            }
        }
    }

    /// Calls a tool and returns (is_error, first text block).
    fn call(&mut self, name: &str, args: Value) -> (bool, String) {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        let result = &r["result"];
        assert!(result.is_object(), "protocol error calling {name}: {r}");
        let text = result["content"]
            .as_array()
            .and_then(|c| c.iter().find(|b| b["type"] == "text"))
            .and_then(|b| b["text"].as_str())
            .unwrap_or_default()
            .to_owned();
        (result["isError"] == json!(true), text)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "starts the engine on the interactive desktop"]
fn lists_tools_with_object_schemas() {
    let mut c = Client::start();
    let tools = c.request("tools/list", json!({}));
    let tools = tools["result"]["tools"].as_array().expect("tool list");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for expected in [
        "desktop_snapshot",
        "desktop_find",
        "desktop_click",
        "desktop_fill",
        "desktop_wait_for",
        "desktop_screenshot",
        "window_control",
        "overlay_highlight",
        "app_launch",
        "filesystem_operation",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    assert!(names.len() <= 25, "keep the tool surface small: {}", names.len());
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        assert!(t["description"].as_str().is_some_and(|d| !d.is_empty()));
    }
}

#[test]
#[ignore = "starts the engine and the fixture on the interactive desktop"]
fn model_style_session_on_the_fixture() {
    let fx = FixtureProcess::launch(Fixture::Win32).expect("fixture");
    let mut c = Client::start();
    let window = fx.title.clone();

    let (err, snap) = c.call("desktop_snapshot", json!({"window": window}));
    assert!(!err, "{snap}");
    assert!(snap.starts_with("snapshot s_1"), "{snap}");
    assert!(snap.contains("BUTTON \"Target\""), "{snap}");

    let (err, found) = c.call("desktop_find", json!({"role": "Button", "name": "Target", "window": window}));
    assert!(!err, "{found}");
    let target_ref = found
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .expect("a ref")
        .to_owned();

    let (err, clicked) = c.call("desktop_click", json!({"ref": target_ref}));
    assert!(!err, "{clicked}");
    let clicked: Value = serde_json::from_str(&clicked).unwrap();
    assert_eq!(clicked["method"], "InvokePattern");

    let (err, waited) = c.call(
        "desktop_wait_for",
        json!({"state": "text", "automationId": "140", "value": "Target clicked 1", "window": window, "timeoutMs": 5000}),
    );
    assert!(!err, "{waited}");

    let (err, filled) = c.call("desktop_fill", json!({"label": "Name", "role": "Edit", "value": "Ada", "window": window}));
    assert!(!err, "{filled}");
    assert!(filled.contains("\"verified\":true"), "{filled}");

    // Policy is enforced through MCP exactly as in the engine.
    let (err, blocked) = c.call("desktop_click", json!({"role": "Button", "name": "Submit", "window": window}));
    assert!(err, "{blocked}");
    assert!(blocked.contains("CONFIRMATION_REQUIRED"), "{blocked}");

    let (err, diff) = c.call("desktop_snapshot", json!({"window": window, "diff": true}));
    assert!(!err, "{diff}");
    assert!(diff.contains("DIFF s_1 -> s_2"), "{diff}");
    assert!(diff.contains("Target clicked 1"), "{diff}");

    let (err, bad) = c.call("desktop_click", json!({}));
    assert!(err && bad.contains("INVALID_REQUEST"), "{bad}");
}
