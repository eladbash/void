//! `void mcp`: a Model Context Protocol server over stdio, so coding agents
//! can check disk space, see what is taking it, and — with the user's
//! explicit approval — clean it.
//!
//! Hand-rolled JSON-RPC 2.0, one message per line. stdout carries only
//! protocol messages; anything diagnostic goes to stderr.
//!
//! The agent can never clean anything it did not first show the user as a
//! plan: `apply_plan` takes only a saved plan id, requires `confirm: true`,
//! and refuses any plan holding a Danger action.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use deepclean_core::attribution;
use deepclean_core::model::{CleanableItem, Ecosystem, RiskLevel};
use deepclean_core::plan::apply_plan;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::commands::PlanArgs;
use crate::ctx::Ctx;
use crate::output::{self, size};

const DEFAULT_PROTOCOL: &str = "2025-06-18";
const TOP_ITEMS: usize = 20;

struct CachedScan {
    items: Vec<CleanableItem>,
    /// `None` = every enabled ecosystem.
    ecosystems: Option<HashSet<Ecosystem>>,
    scanned_at: DateTime<Utc>,
}

pub struct Server {
    ctx: Ctx,
    cache: Option<CachedScan>,
}

pub async fn serve(ctx: Ctx) -> Result<i32, String> {
    let mut server = Server { ctx, cache: None };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    eprintln!("void mcp: ready (home {})", server.ctx.home.display());

    while let Some(line) = lines.next_line().await.map_err(|err| err.to_string())? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(line) {
            Ok(Value::Array(batch)) => {
                let mut out = Vec::new();
                for msg in batch {
                    if let Some(r) = server.handle(msg).await {
                        out.push(r);
                    }
                }
                (!out.is_empty()).then_some(Value::Array(out))
            }
            Ok(msg) => server.handle(msg).await,
            Err(err) => Some(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {err}"),
            )),
        };
        if let Some(response) = response {
            let mut text = serde_json::to_string(&response).map_err(|err| err.to_string())?;
            text.push('\n');
            stdout
                .write_all(text.as_bytes())
                .await
                .map_err(|err| err.to_string())?;
            stdout.flush().await.map_err(|err| err.to_string())?;
        }
    }
    Ok(0)
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn tool_text(value: &Value, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(value).unwrap_or_default()}],
        "isError": is_error,
    })
}

impl Server {
    async fn handle(&mut self, msg: Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // Notifications (no id) never get a response.
        let id = match id {
            Some(id) if !id.is_null() => id,
            _ => {
                if !method.starts_with("notifications/") && !method.is_empty() {
                    eprintln!("void mcp: ignoring notification {method}");
                }
                return None;
            }
        };

        Some(match method {
            "initialize" => {
                let version = params
                    .get("protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_PROTOCOL);
                result_response(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "void", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": "Void finds and cleans developer disk bloat: build folders, package caches, idle agent worktrees, agent transcripts, local AI models. Use disk_status before heavy installs; scan and plan_cleanup to show the user what could be freed. Never call apply_plan unless the user explicitly approved that exact plan.",
                    }),
                )
            }
            "ping" => result_response(id, json!({})),
            "tools/list" => result_response(id, json!({"tools": tool_definitions()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({}));
                match self.call_tool(name, &args).await {
                    None => error_response(id, -32602, &format!("unknown tool `{name}`")),
                    Some(Ok(value)) => result_response(id, tool_text(&value, false)),
                    Some(Err(err)) => result_response(id, tool_text(&json!({"error": err}), true)),
                }
            }
            "" => error_response(id, -32600, "invalid request: missing method"),
            other => error_response(id, -32601, &format!("method not found: {other}")),
        })
    }

    async fn call_tool(&mut self, name: &str, args: &Value) -> Option<Result<Value, String>> {
        Some(match name {
            "disk_status" => self.disk_status(),
            "scan" => self.scan(args).await,
            "explain_item" => self.explain_item(args),
            "plan_cleanup" => self.plan_cleanup(args).await,
            "apply_plan" => self.apply_plan(args).await,
            "usage_by_agent" => self.usage_by_agent().await,
            _ => return None,
        })
    }

    fn disk_status(&self) -> Result<Value, String> {
        let status = self
            .ctx
            .guard_status(None)
            .ok_or("could not determine the volume holding home")?;
        let mut v = serde_json::to_value(&status).map_err(|err| err.to_string())?;
        v["free"] = json!(size(status.free_bytes));
        v["total"] = json!(size(status.total_bytes));
        Ok(v)
    }

    /// Scan (or reuse the cached scan when it covers `wanted`).
    async fn items_for(&mut self, wanted: &[Ecosystem], force: bool) -> &CachedScan {
        let covered = match &self.cache {
            Some(c) if !force => match &c.ecosystems {
                None => true,
                Some(set) => !wanted.is_empty() && wanted.iter().all(|e| set.contains(e)),
            },
            _ => false,
        };
        if !covered {
            let items = self.ctx.scan(wanted, false).await;
            self.cache = Some(CachedScan {
                items,
                ecosystems: (!wanted.is_empty()).then(|| wanted.iter().copied().collect()),
                scanned_at: Utc::now(),
            });
        }
        // Just filled above when absent.
        self.cache.get_or_insert_with(|| CachedScan {
            items: Vec::new(),
            ecosystems: None,
            scanned_at: Utc::now(),
        })
    }

    async fn scan(&mut self, args: &Value) -> Result<Value, String> {
        let wanted = ecosystems_arg(args)?;
        let cache = self.items_for(&wanted, true).await;
        let items: Vec<&CleanableItem> = cache
            .items
            .iter()
            .filter(|i| wanted.is_empty() || wanted.contains(&i.ecosystem))
            .collect();

        let mut per_eco: Vec<(Ecosystem, usize, u64)> = Vec::new();
        for item in &items {
            match per_eco.iter_mut().find(|(e, _, _)| *e == item.ecosystem) {
                Some(entry) => {
                    entry.1 += 1;
                    entry.2 += item.size_bytes;
                }
                None => per_eco.push((item.ecosystem, 1, item.size_bytes)),
            }
        }
        per_eco.sort_by_key(|e| std::cmp::Reverse(e.2));
        let total = deepclean_core::model::reclaimable_bytes(items.iter().copied());

        Ok(json!({
            "scanned_at": cache.scanned_at,
            "item_count": items.len(),
            "total_bytes": total,
            "total": size(total),
            "ecosystems": per_eco.iter().map(|(e, n, b)| json!({
                "ecosystem": e, "name": e.display_name(), "items": n, "total_bytes": b, "total": size(*b)
            })).collect::<Vec<_>>(),
            "top_items": items.iter().take(TOP_ITEMS).map(|i| output::item_summary(i)).collect::<Vec<_>>(),
            "next": "Nothing was changed. Use explain_item for details, or plan_cleanup to build a dry-run plan to show the user.",
        }))
    }

    fn explain_item(&self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("item_id")
            .and_then(Value::as_str)
            .ok_or("item_id is required")?;
        let id = uuid::Uuid::parse_str(id).map_err(|_| format!("`{id}` is not an item id"))?;
        let cache = self.cache.as_ref().ok_or("no scan yet; call scan first")?;
        let item =
            cache.items.iter().find(|i| i.id == id).ok_or(
                "unknown item id (ids change with every scan; use one from the latest scan)",
            )?;
        Ok(output::item_explained(item))
    }

    async fn plan_cleanup(&mut self, args: &Value) -> Result<Value, String> {
        let strings = |key: &str| -> Result<Vec<String>, String> {
            match args.get(key) {
                None | Some(Value::Null) => Ok(Vec::new()),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| format!("{key} must be an array of strings"))
                    })
                    .collect(),
                Some(_) => Err(format!("{key} must be an array of strings")),
            }
        };
        let plan_args = PlanArgs {
            ecosystems: strings("ecosystems")?,
            kinds: strings("kinds")?,
            max_risk: args
                .get("max_risk")
                .and_then(Value::as_str)
                .map(str::to_string),
            stale_days: args.get("stale_days").and_then(Value::as_u64),
            paths: strings("paths")?.into_iter().map(Into::into).collect(),
            item_ids: strings("item_ids")?,
            limit: None,
            limit_bytes: args.get("limit_bytes").and_then(Value::as_u64),
        };
        let filter = plan_args.filter(&self.ctx)?;
        let items = self
            .items_for(&filter.ecosystems, false)
            .await
            .items
            .clone();
        let plan = self.ctx.save_plan(&items, &filter)?;
        let mut view = output::plan_view(&plan);
        view["next"] = json!(
            "Nothing was changed. Show the user every entry above (path, size, and what it runs). Call apply_plan with this plan_id and confirm=true only after the user explicitly approves this exact plan."
        );
        Ok(view)
    }

    async fn apply_plan(&mut self, args: &Value) -> Result<Value, String> {
        let id = args
            .get("plan_id")
            .and_then(Value::as_str)
            .ok_or("plan_id is required")?;
        if args.get("confirm") != Some(&Value::Bool(true)) {
            return Err("not applied: confirm must be true, and only after the user explicitly approved this exact plan".into());
        }
        let plan = self.ctx.load_applicable_plan(id)?;
        // Belt and braces: MCP never runs Danger, whatever the plan file says.
        if plan
            .entries
            .iter()
            .any(|e| e.action.risk > RiskLevel::Caution)
        {
            return Err("not applied: the plan contains a Danger action".into());
        }
        let report = apply_plan(&plan, self.ctx.executor()).await;
        let _ = self.ctx.plans.remove(plan.id);
        if let Err(err) = self.ctx.record_history(report.run.clone(), "mcp") {
            eprintln!("void mcp: {err}");
        }
        if let Some(cache) = &mut self.cache {
            let applied: HashSet<_> = plan.entries.iter().map(|e| e.item.id).collect();
            cache.items.retain(|i| !applied.contains(&i.id));
        }
        Ok(output::report_view(&report))
    }

    async fn usage_by_agent(&mut self) -> Result<Value, String> {
        let items = &self.items_for(&[], false).await.items;
        Ok(output::usage_view(&attribution::by_agent(items)))
    }
}

fn ecosystems_arg(args: &Value) -> Result<Vec<Ecosystem>, String> {
    match args.get("ecosystems") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| "ecosystems must be an array of strings".to_string())
                    .and_then(output::parse_ecosystem)
            })
            .collect(),
        Some(_) => Err("ecosystems must be an array of strings".into()),
    }
}

fn tool_definitions() -> Value {
    let ecosystems: Vec<String> = Ecosystem::ALL.iter().map(output::name_of).collect();
    let eco_schema = json!({
        "type": "array",
        "items": {"type": "string", "enum": ecosystems},
        "description": "Limit to these ecosystems (default: all enabled)."
    });
    json!([
        {
            "name": "disk_status",
            "description": "Free space on the volume holding the user's home, classified ok / low / critical by the user's Void guard thresholds. Cheap; call it before large installs, model downloads or creating worktrees.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        },
        {
            "name": "scan",
            "description": "Scan for reclaimable disk space (build folders, package caches, agent worktrees, agent transcripts, local AI models, stale projects). Read-only; can take a while on a large home. Returns totals per ecosystem and the 20 largest items with their ids. Nothing is deleted.",
            "inputSchema": {"type": "object", "properties": {"ecosystems": eco_schema}, "additionalProperties": false}
        },
        {
            "name": "explain_item",
            "description": "Full details of one item from the latest scan, including every available action, its risk, and the exact command or filesystem operation it would run. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {"item_id": {"type": "string", "description": "An item id from the latest scan."}},
                "required": ["item_id"],
                "additionalProperties": false
            }
        },
        {
            "name": "plan_cleanup",
            "description": "Build a dry-run cleanup plan (nothing is deleted) and return its plan_id, every entry with the exact action it would run, and the total. Uses the latest scan, scanning first if needed. Show the plan to the user and ask before applying it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ecosystems": eco_schema,
                    "kinds": {"type": "array", "items": {"type": "string"}, "description": "Artifact kinds, snake_case as in scan results (e.g. node_modules, target_dir, agent_worktree)."},
                    "max_risk": {"type": "string", "enum": ["safe", "caution"], "default": "safe", "description": "Riskiest action allowed. Danger is never available here."},
                    "stale_days": {"type": "integer", "minimum": 0, "description": "Only items untouched for at least this many days."},
                    "paths": {"type": "array", "items": {"type": "string"}, "description": "Only items under these absolute paths."},
                    "item_ids": {"type": "array", "items": {"type": "string"}, "description": "Only these items from the latest scan."},
                    "limit_bytes": {"type": "integer", "minimum": 0, "description": "Stop adding entries once this many bytes are planned."}
                },
                "additionalProperties": false
            }
        },
        {
            "name": "apply_plan",
            "description": "Execute a saved cleanup plan exactly as it was shown. Only call after the user explicitly approved this exact plan. Requires confirm=true; plans expire after an hour and are single-use; plans containing Danger actions are refused. Every path is re-checked by Void's safety checker before deletion.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "plan_id": {"type": "string", "description": "The plan_id returned by plan_cleanup."},
                    "confirm": {"type": "boolean", "const": true, "description": "Must be true: the user explicitly approved this plan."}
                },
                "required": ["plan_id", "confirm"],
                "additionalProperties": false
            }
        },
        {
            "name": "usage_by_agent",
            "description": "Disk space attributed to each AI tool (Claude Code, Cursor, Codex, Ollama, …), broken down by category. Read-only; uses the latest scan, scanning first if needed.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }
    ])
}
