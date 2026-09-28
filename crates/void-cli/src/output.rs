//! Parsing user-supplied names and shaping output, shared by the CLI and the
//! MCP server so both describe items and plans the same way.

use bytesize::ByteSize;
use deepclean_core::attribution::AgentUsage;
use deepclean_core::model::{ArtifactKind, CleanableItem, Ecosystem, RiskLevel};
use deepclean_core::plan::{describe_method, ApplyReport, Plan};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The serde (snake_case) name of an enum value.
pub fn name_of<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Accept "agent_data", "agent-data", "AgentData", "Agent worktrees", "node.js"…
pub fn parse_ecosystem(s: &str) -> Result<Ecosystem, String> {
    let wanted = normalize(s);
    Ecosystem::ALL
        .into_iter()
        .find(|e| normalize(&name_of(e)) == wanted || normalize(e.display_name()) == wanted)
        .ok_or_else(|| {
            format!(
                "unknown ecosystem `{s}`; one of: {}",
                Ecosystem::ALL
                    .iter()
                    .map(name_of)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
}

fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.trim().chars().enumerate() {
        if ch == '-' || ch == ' ' {
            out.push('_');
        } else if ch.is_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn parse_serde<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(Value::String(to_snake(s))).ok()
}

/// Accept "node_modules", "node-modules", "NodeModules".
pub fn parse_kind(s: &str) -> Result<ArtifactKind, String> {
    parse_serde(s).ok_or_else(|| {
        format!("unknown kind `{s}` (kinds are snake_case, as in `void scan --json`, e.g. node_modules, target_dir, agent_worktree)")
    })
}

/// Only `safe` and `caution`: Danger is never available outside the app.
pub fn parse_max_risk(s: &str) -> Result<RiskLevel, String> {
    match parse_serde::<RiskLevel>(s) {
        Some(RiskLevel::Safe) => Ok(RiskLevel::Safe),
        Some(RiskLevel::Caution) => Ok(RiskLevel::Caution),
        Some(RiskLevel::Danger) => Err(
            "Danger actions are available only in the Void app, where they need a typed confirmation; use --max-risk safe or caution".into(),
        ),
        None => Err(format!("unknown risk `{s}`; use safe or caution")),
    }
}

/// "10GB", "500 MiB", "1024".
pub fn parse_size(s: &str) -> Result<u64, String> {
    s.trim()
        .parse::<ByteSize>()
        .map(|b| b.as_u64())
        .map_err(|err| format!("invalid size `{s}`: {err}"))
}

/// `path` with the home prefix shown as `~`, for human-readable output.
pub fn tilde(home: &std::path::Path, path: &std::path::Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn size(bytes: u64) -> String {
    ByteSize(bytes).to_string()
}

pub fn stale(days: Option<u64>) -> String {
    days.map(|d| format!("{d}d")).unwrap_or_else(|| "-".into())
}

/// Compact item for listings: enough to decide, with its id for follow-ups.
pub fn item_summary(item: &CleanableItem) -> Value {
    json!({
        "id": item.id,
        "path": item.path,
        "ecosystem": item.ecosystem,
        "kind": item.kind,
        "size_bytes": item.size_bytes,
        "size": item.size_display,
        "risk": item.risk,
        "days_stale": item.days_stale,
        "agent": item.agent,
        "project": item.project_name,
        "details": item.details.iter().map(|d| json!({"label": d.label, "value": d.value})).collect::<Vec<_>>(),
    })
}

/// Full item, every action with the exact method it runs.
pub fn item_explained(item: &CleanableItem) -> Value {
    let mut v = item_summary(item);
    v["last_modified"] = json!(item.last_modified);
    v["project_root"] = json!(item.project_root);
    v["actions"] = item
        .available_actions
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "label": a.label,
                "description": a.description,
                "risk": a.risk,
                "estimated_savings_bytes": a.estimated_savings_bytes,
                "estimated_savings": size(a.estimated_savings_bytes),
                "runs": describe_method(&a.method),
                "method": a.method,
            })
        })
        .collect();
    v
}

pub fn plan_view(plan: &Plan) -> Value {
    json!({
        "plan_id": plan.id,
        "created_at": plan.created_at,
        "expires_at": plan.expires_at(),
        "home": plan.home,
        "total_bytes": plan.total_bytes,
        "total": size(plan.total_bytes),
        "estimated": plan.estimated,
        "entry_count": plan.entries.len(),
        "entries": plan.entries.iter().map(|e| json!({
            "item_id": e.item.id,
            "path": e.item.path,
            "ecosystem": e.item.ecosystem,
            "kind": e.item.kind,
            "agent": e.item.agent,
            "size_bytes": e.action.estimated_savings_bytes,
            "size": size(e.action.estimated_savings_bytes),
            "days_stale": e.item.days_stale,
            "action": {
                "id": e.action.id,
                "label": e.action.label,
                "risk": e.action.risk,
                "method": e.action.method,
            },
            "runs": describe_method(&e.action.method),
        })).collect::<Vec<_>>(),
    })
}

pub fn report_view(report: &ApplyReport) -> Value {
    json!({
        "plan_id": report.plan_id,
        "succeeded": report.succeeded,
        "failed": report.failed,
        "bytes_freed": report.bytes_freed,
        "freed": size(report.bytes_freed),
        "estimated": report.estimated,
    })
}

pub fn usage_view(usage: &[AgentUsage]) -> Value {
    json!(usage
        .iter()
        .map(|u| json!({
            "agent": u.agent,
            "name": u.display_name,
            "total_bytes": u.total_bytes,
            "total": size(u.total_bytes),
            "items": u.item_count,
            "categories": u.categories.iter().map(|c| json!({
                "label": c.label, "bytes": c.bytes, "size": size(c.bytes), "count": c.count
            })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

/// One line per agent: "Claude Code  38 GB  (Worktrees 30 GB, Transcripts 6 GB)".
pub fn usage_lines(usage: &[AgentUsage]) -> Vec<String> {
    let width = usage
        .iter()
        .map(|u| u.display_name.chars().count())
        .max()
        .unwrap_or(0);
    usage
        .iter()
        .map(|u| {
            let cats = u
                .categories
                .iter()
                .map(|c| format!("{} {}", c.label, size(c.bytes)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{:<width$}  {:>10}  ({cats})",
                u.display_name,
                size(u.total_bytes)
            )
        })
        .collect()
}

/// Print a left-aligned table; the last column is never padded.
pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: &[String]| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                out.push_str(cell);
            } else {
                out.push_str(cell);
                let pad = widths[i].saturating_sub(cell.chars().count()) + 2;
                out.push_str(&" ".repeat(pad));
            }
        }
        out
    };
    println!(
        "{}",
        line(&headers.iter().map(|h| h.to_string()).collect::<Vec<_>>())
    );
    for row in rows {
        println!("{}", line(row));
    }
}

pub fn print_json(value: &impl Serialize) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(err) => eprintln!("error: could not encode JSON: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_friendly_names() {
        assert_eq!(parse_ecosystem("agent-data").unwrap(), Ecosystem::AgentData);
        assert_eq!(parse_ecosystem("Node.js").unwrap(), Ecosystem::Node);
        assert_eq!(parse_ecosystem("jetbrains").unwrap(), Ecosystem::JetBrains);
        assert!(parse_ecosystem("cobol").is_err());
        assert_eq!(
            parse_kind("node-modules").unwrap(),
            ArtifactKind::NodeModules
        );
        assert_eq!(
            parse_kind("AgentWorktree").unwrap(),
            ArtifactKind::AgentWorktree
        );
        assert_eq!(parse_max_risk("caution").unwrap(), RiskLevel::Caution);
        assert!(parse_max_risk("danger").is_err());
        assert_eq!(parse_size("1KiB").unwrap(), 1024);
        assert!(parse_size("lots").is_err());
    }
}
