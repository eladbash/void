//! The `void` subcommands other than `hook` and `mcp`.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use deepclean_core::attribution;
use deepclean_core::config::AppConfig;
use deepclean_core::guard::DiskState;
use deepclean_core::model::{CleanableItem, Ecosystem};
use deepclean_core::plan::{apply_plan, describe_method, Plan, PlanFilter};
use deepclean_core::testkit::{self, FakeHome};

use crate::ctx::{absolute, Ctx};
use crate::output::{self, name_of, print_json, print_table, size, stale, tilde};

/// Filters shared by `void plan` and the MCP `plan_cleanup` tool, as strings.
#[derive(Debug, Default, Clone)]
pub struct PlanArgs {
    pub ecosystems: Vec<String>,
    pub kinds: Vec<String>,
    pub max_risk: Option<String>,
    pub stale_days: Option<u64>,
    pub paths: Vec<PathBuf>,
    pub item_ids: Vec<String>,
    pub limit: Option<String>,
    pub limit_bytes: Option<u64>,
}

impl PlanArgs {
    pub fn ecosystems(&self) -> Result<Vec<Ecosystem>, String> {
        self.ecosystems
            .iter()
            .map(|s| output::parse_ecosystem(s))
            .collect()
    }

    pub fn filter(&self, ctx: &Ctx) -> Result<PlanFilter, String> {
        let max_risk = match &self.max_risk {
            Some(r) => output::parse_max_risk(r)?,
            None => deepclean_core::model::RiskLevel::Safe,
        };
        let limit_bytes = match (&self.limit, self.limit_bytes) {
            (Some(s), _) => Some(output::parse_size(s)?),
            (None, n) => n,
        };
        Ok(PlanFilter {
            ecosystems: self.ecosystems()?,
            kinds: self
                .kinds
                .iter()
                .map(|s| output::parse_kind(s))
                .collect::<Result<_, _>>()?,
            max_risk,
            min_days_stale: self.stale_days,
            paths: self.paths.iter().map(|p| absolute(p)).collect(),
            item_ids: self
                .item_ids
                .iter()
                .map(|s| {
                    uuid::Uuid::parse_str(s.trim()).map_err(|_| format!("`{s}` is not an item id"))
                })
                .collect::<Result<_, _>>()?,
            prefer_trash: ctx.config.ui.prefer_trash,
            limit_bytes,
        })
    }
}

fn parse_ecosystems(names: &[String]) -> Result<Vec<Ecosystem>, String> {
    names.iter().map(|s| output::parse_ecosystem(s)).collect()
}

pub async fn scan(ctx: &Ctx, ecosystems: &[String]) -> Result<i32, String> {
    let only = parse_ecosystems(ecosystems)?;
    let items = ctx.scan(&only, true).await;
    if ctx.json {
        print_json(&items);
        return Ok(0);
    }
    if items.is_empty() {
        println!("Nothing to clean.");
        return Ok(0);
    }
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|i| {
            vec![
                i.ecosystem.display_name().to_string(),
                name_of(&i.kind),
                i.size_display.clone(),
                stale(i.days_stale),
                tilde(&ctx.home, &i.path),
            ]
        })
        .collect();
    print_table(&["ECOSYSTEM", "KIND", "SIZE", "STALE", "PATH"], &rows);
    let total = deepclean_core::model::reclaimable_bytes(&items);
    println!("\n{} items, {} reclaimable.", items.len(), size(total));
    println!("Next: `void plan` builds a dry-run plan of the safe cleanups.");
    Ok(0)
}

fn print_plan(plan: &Plan) {
    println!("Plan {}  (~ = {})", plan.id, plan.home.display());
    println!(
        "Expires {} · {} entries · {}{}",
        plan.expires_at().format("%Y-%m-%d %H:%M UTC"),
        plan.entries.len(),
        if plan.estimated { "~" } else { "" },
        size(plan.total_bytes)
    );
    if plan.entries.is_empty() {
        println!("\nNothing matched. Try widening the filters (e.g. --max-risk caution).");
        return;
    }
    println!();
    for (n, e) in plan.entries.iter().enumerate() {
        println!(
            "{:>3}. [{}] {:>10}  {}",
            n + 1,
            name_of(&e.action.risk),
            size(e.action.estimated_savings_bytes),
            tilde(&plan.home, &e.item.path)
        );
        println!(
            "      {} — {}",
            e.action.label,
            describe_method(&e.action.method)
        );
    }
}

pub async fn plan(ctx: &Ctx, args: &PlanArgs) -> Result<i32, String> {
    let filter = args.filter(ctx)?;
    let items = ctx.scan(&filter.ecosystems, true).await;
    let plan = ctx.save_plan(&items, &filter)?;
    if ctx.json {
        print_json(&output::plan_view(&plan));
    } else {
        print_plan(&plan);
        if !plan.entries.is_empty() {
            println!("\nNothing has been changed. To apply exactly this plan:");
            println!("  void apply {}", plan.id);
        }
    }
    Ok(0)
}

pub async fn apply(ctx: &Ctx, id: &str, yes: bool) -> Result<i32, String> {
    let plan = ctx.load_applicable_plan(id)?;
    if !ctx.json {
        print_plan(&plan);
        println!();
    }
    if plan.entries.is_empty() {
        ctx.plans.remove(plan.id)?;
        if ctx.json {
            print_json(
                &serde_json::json!({"plan_id": plan.id, "succeeded": 0, "failed": [], "bytes_freed": 0}),
            );
        }
        return Ok(0);
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(
                "refusing to apply without confirmation: pass --yes (stdin is not a terminal)"
                    .into(),
            );
        }
        eprint!(
            "Apply these {} actions ({})? [y/N] ",
            plan.entries.len(),
            size(plan.total_bytes)
        );
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(|err| err.to_string())?;
        if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
            eprintln!("Cancelled; nothing was changed.");
            return Ok(1);
        }
    }

    let report = apply_plan(&plan, ctx.executor()).await;
    // Single use: the plan described the disk before this run.
    let _ = ctx.plans.remove(plan.id);
    if let Err(err) = ctx.record_history(report.run.clone(), "cli") {
        eprintln!("warning: {err}");
    }

    if ctx.json {
        print_json(&output::report_view(&report));
    } else {
        println!(
            "Done: {} succeeded, {} failed, {}{} freed.",
            report.succeeded,
            report.failed.len(),
            if report.estimated { "~" } else { "" },
            size(report.bytes_freed)
        );
        for f in &report.failed {
            println!("  failed {}: {}", f.path.display(), f.error);
        }
    }
    Ok(if report.failed.is_empty() { 0 } else { 1 })
}

/// `void worktrees` / `models` / `agents`: one ecosystem, with the details
/// that matter for it.
pub async fn focused(ctx: &Ctx, eco: Ecosystem) -> Result<i32, String> {
    let items = ctx.scan(&[eco], true).await;
    if ctx.json {
        print_json(&items.iter().map(output::item_summary).collect::<Vec<_>>());
        return Ok(0);
    }
    if items.is_empty() {
        println!("No {} found.", eco.display_name().to_lowercase());
        return Ok(0);
    }
    for item in &items {
        println!(
            "{:>10}  {:<22} {:>5}  {}{}",
            item.size_display,
            name_of(&item.kind),
            stale(item.days_stale),
            tilde(&ctx.home, &item.path),
            item.agent
                .as_deref()
                .map(|a| format!("  [{a}]"))
                .unwrap_or_default()
        );
        if !item.details.is_empty() {
            println!(
                "            {}",
                item.details
                    .iter()
                    .map(|d| format!("{}: {}", d.label, d.value.replace('\n', "\n              ")))
                    .collect::<Vec<_>>()
                    .join(" · ")
            );
        }
    }
    let total = deepclean_core::model::reclaimable_bytes(&items);
    println!("\n{} items, {} reclaimable.", items.len(), size(total));
    Ok(0)
}

pub async fn usage(ctx: &Ctx) -> Result<i32, String> {
    let items: Vec<CleanableItem> = ctx.scan(&[], true).await;
    let usage = attribution::by_agent(&items);
    if ctx.json {
        print_json(&output::usage_view(&usage));
    } else if usage.is_empty() {
        println!("No space attributed to AI tools.");
    } else {
        for line in output::usage_lines(&usage) {
            println!("{line}");
        }
    }
    Ok(0)
}

pub fn guard_check(ctx: &Ctx) -> Result<i32, String> {
    let status = ctx.guard_status(None).ok_or_else(|| {
        format!(
            "could not determine the volume holding {}",
            ctx.home.display()
        )
    })?;
    if ctx.json {
        print_json(&status);
    } else {
        // The guard's message already states the free amount; only the
        // volume size is added here.
        println!(
            "{}: {} (volume {})",
            name_of(&status.state),
            status.message,
            size(status.total_bytes),
        );
        println!(
            "Thresholds: warn below {}%, critical below {}% (from {})",
            ctx.config.guard.warn_free_percent,
            ctx.config.guard.critical_free_percent,
            ctx.config_path.display()
        );
    }
    Ok(match status.state {
        DiskState::Ok => 0,
        DiskState::Low => 1,
        DiskState::Critical => 2,
    })
}

/// Marker written into every seeded sandbox.
const SANDBOX_MARKER: &str = ".void-sandbox";

pub fn dev_seed(dir: &Path) -> Result<i32, String> {
    let dir = absolute(dir);
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    let dir = dir.canonicalize().unwrap_or(dir);
    // Seeding writes fake repos, transcripts and models; never into a real home.
    if let Some(real) = dirs::home_dir() {
        let real = real.canonicalize().unwrap_or(real);
        if real.starts_with(&dir) {
            return Err(format!(
                "refusing to seed {}: it is your home directory or contains it",
                dir.display()
            ));
        }
    }
    let home = FakeHome::at(&dir);
    let seeded = std::panic::catch_unwind(|| testkit::seed_all(&home));
    if seeded.is_err() {
        return Err("seeding failed (is `git` installed?)".into());
    }
    std::fs::write(home.path(SANDBOX_MARKER), "seeded by `void dev seed`\n")
        .map_err(|err| err.to_string())?;
    // The sandbox's own config (what `--home` reads when no --config is
    // given): the seeded duplicate models are tiny, so report duplicates of
    // any size.
    let config_path = home
        .path(crate::ctx::SANDBOX_CONFIG)
        .join(AppConfig::FILE_NAME);
    let (mut config, _) = AppConfig::load(&config_path);
    config.scan_roots = vec![home.root().to_path_buf()];
    config.ai.min_duplicate_model_mb = 0;
    config
        .save(&config_path)
        .map_err(|err| format!("could not write {}: {err}", config_path.display()))?;
    println!("Seeded a sandbox home at {}", home.root().display());
    println!("Try:");
    println!("  void --home {} scan", home.root().display());
    println!(
        "  void --home {} plan --max-risk caution",
        home.root().display()
    );
    println!(
        "  void --home {} apply <plan-id> --yes",
        home.root().display()
    );
    println!("Everything (config, history, plans, Trash) stays inside that directory.");
    Ok(0)
}

pub fn mcp_config() -> Result<i32, String> {
    let exe = current_exe()?;
    println!("{}", deepclean_core::hooks::mcp_config_snippet(&exe));
    println!();
    println!("Or register it with Claude Code directly:");
    println!("  claude mcp add void -- {} mcp", quote_arg(&exe));
    Ok(0)
}

pub fn current_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|err| format!("could not locate void: {err}"))?;
    Ok(exe.canonicalize().unwrap_or(exe))
}

fn quote_arg(path: &Path) -> String {
    let s = path.display().to_string();
    if s.contains([' ', '\'', '"']) {
        format!("'{}'", s.replace('\'', r"'\''"))
    } else {
        s
    }
}
