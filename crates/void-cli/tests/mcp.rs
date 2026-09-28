//! The MCP server over real stdio: JSON-RPC in, JSON-RPC out.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use common::*;
use deepclean_core::testkit::FakeHome;
use serde_json::{json, Value};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Mcp {
    fn start(home: &FakeHome) -> Self {
        let mut child = void_cmd(home.root())
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn void mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, msg: Value) {
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn recv(&self) -> Value {
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(120))
            .expect("a response line");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let response = self.recv();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], id, "responses arrive in order: {response}");
        response
    }

    /// `tools/call`, returning (isError, parsed text payload).
    fn call(&mut self, id: u64, name: &str, args: Value) -> (bool, Value) {
        let response = self.request(id, "tools/call", json!({"name": name, "arguments": args}));
        let result = &response["result"];
        assert_eq!(result["content"][0]["type"], "text", "{response}");
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"].as_bool().unwrap(),
            serde_json::from_str(text).unwrap(),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_session_scans_plans_and_applies_only_with_confirmation() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let project = rust_project(&home, "code/app");
    let mut mcp = Mcp::start(&home);

    let init = mcp.request(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "void");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    // A notification gets no response; the next line answers id 2.
    mcp.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let list = mcp.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    for expected in [
        "disk_status",
        "scan",
        "explain_item",
        "plan_cleanup",
        "apply_plan",
        "usage_by_agent",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
    let apply_tool = tools.iter().find(|t| t["name"] == "apply_plan").unwrap();
    assert!(apply_tool["description"]
        .as_str()
        .unwrap()
        .contains("Only call after the user explicitly approved this exact plan."));

    let pong = mcp.request(3, "ping", json!({}));
    assert!(pong["result"].is_object());

    let (err, disk) = mcp.call(4, "disk_status", json!({}));
    assert!(!err);
    assert!(disk["state"].is_string());

    let (err, scan) = mcp.call(5, "scan", json!({"ecosystems": ["rust"]}));
    assert!(!err, "{scan}");
    assert_eq!(scan["item_count"], 1);
    let item_id = scan["top_items"][0]["id"].as_str().unwrap().to_string();

    let (err, explained) = mcp.call(6, "explain_item", json!({"item_id": item_id}));
    assert!(!err, "{explained}");
    assert!(explained["actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["runs"].as_str().unwrap().contains("cargo clean")));

    let (err, refused) = mcp.call(7, "plan_cleanup", json!({"max_risk": "danger"}));
    assert!(err, "{refused}");

    let (err, plan) = mcp.call(
        8,
        "plan_cleanup",
        json!({"ecosystems": ["rust"], "paths": [project], "max_risk": "safe"}),
    );
    assert!(!err, "{plan}");
    assert_eq!(plan["entry_count"], 1);
    let plan_id = plan["plan_id"].as_str().unwrap().to_string();

    let (err, _) = mcp.call(
        9,
        "apply_plan",
        json!({"plan_id": plan_id, "confirm": false}),
    );
    assert!(err, "confirm=false must not apply");
    let (err, _) = mcp.call(10, "apply_plan", json!({"plan_id": plan_id}));
    assert!(err, "missing confirm must not apply");
    assert!(project.join("target").exists());

    let (err, report) = mcp.call(
        11,
        "apply_plan",
        json!({"plan_id": plan_id, "confirm": true}),
    );
    assert!(!err, "{report}");
    assert_eq!(report["succeeded"], 1);
    assert!(!project.join("target").exists());
    assert!(project.join("src/main.rs").exists());

    let (err, _) = mcp.call(12, "usage_by_agent", json!({}));
    assert!(!err);

    // Protocol errors.
    let missing = mcp.request(13, "no/such/method", json!({}));
    assert_eq!(missing["error"]["code"], -32601);
    let bad_tool = mcp.request(14, "tools/call", json!({"name": "rm_rf", "arguments": {}}));
    assert!(bad_tool["error"].is_object());
}

#[test]
fn mcp_never_applies_a_plan_holding_a_danger_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let home = FakeHome::at(tmp.path());
    let victim = home.dir("code/precious");
    home.file("code/precious/notes.md", "keep");
    let plan_id = danger_plan(&home, &victim);

    let mut mcp = Mcp::start(&home);
    mcp.request(1, "initialize", json!({}));
    let (err, result) = mcp.call(
        2,
        "apply_plan",
        json!({"plan_id": plan_id.to_string(), "confirm": true}),
    );
    assert!(err, "{result}");
    assert!(result["error"].as_str().unwrap().contains("Danger"));
    assert!(victim.join("notes.md").exists());
}
