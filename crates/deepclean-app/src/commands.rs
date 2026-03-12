use std::sync::Arc;

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager, State};

use deepclean_core::model::*;
use deepclean_core::scanner::apple::AppleScanner;
use deepclean_core::scanner::docker::DockerScanner;
use deepclean_core::scanner::dotnet::DotNetScanner;
use deepclean_core::scanner::go::GoScanner;
use deepclean_core::scanner::homebrew::HomebrewScanner;
use deepclean_core::scanner::java::JavaScanner;
use deepclean_core::scanner::jetbrains::JetBrainsScanner;
use deepclean_core::scanner::node::NodeScanner;
use deepclean_core::scanner::python::PythonScanner;
use deepclean_core::scanner::rust::RustScanner;
use deepclean_core::scanner::system::SystemScanner;
use deepclean_core::scanner::{EcosystemScanner, ScanOrchestrator};

use crate::state::AppState;

#[tauri::command]
pub async fn start_scan(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    {
        let mut scanning = state.is_scanning.lock().unwrap();
        if *scanning {
            return Err("Scan already in progress".into());
        }
        *scanning = true;
    }

    // Clear previous results
    state.scan_results.lock().unwrap().clear();

    let config = state.config.lock().unwrap().clone();

    // Build scanners based on enabled ecosystems
    let mut scanners: Vec<Arc<dyn EcosystemScanner>> = vec![];
    for eco in &config.enabled_ecosystems {
        match eco {
            Ecosystem::Rust => scanners.push(Arc::new(RustScanner)),
            Ecosystem::Node => scanners.push(Arc::new(NodeScanner)),
            Ecosystem::Apple => scanners.push(Arc::new(AppleScanner)),
            Ecosystem::Docker => scanners.push(Arc::new(DockerScanner)),
            Ecosystem::Go => scanners.push(Arc::new(GoScanner)),
            Ecosystem::System => scanners.push(Arc::new(SystemScanner)),
            Ecosystem::Python => scanners.push(Arc::new(PythonScanner)),
            Ecosystem::Java => scanners.push(Arc::new(JavaScanner)),
            Ecosystem::Homebrew => scanners.push(Arc::new(HomebrewScanner)),
            Ecosystem::JetBrains => scanners.push(Arc::new(JetBrainsScanner)),
            Ecosystem::DotNet => scanners.push(Arc::new(DotNetScanner)),
        }
    }

    let orchestrator = ScanOrchestrator::new(scanners, config);
    let mut rx = orchestrator.start_scan();

    // Stream events to frontend
    let app_clone = app.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match &event {
                ScanEvent::ItemFound { item } => {
                    // Store result
                    if let Some(handle) = app_clone.try_state::<AppState>() {
                        handle.scan_results.lock().unwrap().push(item.clone());
                    }
                }
                ScanEvent::ScanComplete { summary } => {
                    if let Some(handle) = app_clone.try_state::<AppState>() {
                        *handle.scan_summary.lock().unwrap() = summary.clone();
                        *handle.is_scanning.lock().unwrap() = false;
                    }
                }
                _ => {}
            }
            let _ = app_clone.emit("scan-event", &event);
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn get_scan_results(
    state: State<'_, AppState>,
    ecosystem: Option<Ecosystem>,
    min_risk: Option<RiskLevel>,
) -> Result<Vec<CleanableItem>, String> {
    let results = state.scan_results.lock().unwrap();
    let filtered: Vec<CleanableItem> = results
        .iter()
        .filter(|item| {
            if let Some(eco) = &ecosystem {
                if &item.ecosystem != eco {
                    return false;
                }
            }
            if let Some(risk) = &min_risk {
                if &item.risk < risk {
                    return false;
                }
            }
            true
        })
        .cloned()
        .collect();
    Ok(filtered)
}

#[derive(Deserialize)]
pub struct CleanRequest {
    pub selections: Vec<CleanSelection>,
}

#[derive(Deserialize)]
pub struct CleanSelection {
    pub item_id: uuid::Uuid,
    pub action_id: uuid::Uuid,
}

#[tauri::command]
pub async fn execute_clean(
    app: AppHandle,
    state: State<'_, AppState>,
    request: CleanRequest,
) -> Result<(), String> {
    let results = state.scan_results.lock().unwrap();

    // Resolve selections to (item, action) pairs
    let mut pairs = vec![];
    for sel in &request.selections {
        if let Some(item) = results.iter().find(|i| i.id == sel.item_id) {
            if let Some(action) = item.available_actions.iter().find(|a| a.id == sel.action_id) {
                pairs.push((item.clone(), action.clone()));
            }
        }
    }
    drop(results);

    if pairs.is_empty() {
        return Err("No valid selections".into());
    }

    let executor = state.create_executor();
    let mut rx = executor.execute_batch(pairs);

    let app_clone = app.clone();
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let _ = app_clone.emit("action-event", &event);
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn get_summary(state: State<'_, AppState>) -> Result<ScanSummary, String> {
    let summary = state.scan_summary.lock().unwrap().clone();
    Ok(summary)
}

#[tauri::command]
pub async fn get_config(state: State<'_, AppState>) -> Result<deepclean_core::config::AppConfig, String> {
    let config = state.config.lock().unwrap().clone();
    Ok(config)
}

#[tauri::command]
pub async fn update_config(
    state: State<'_, AppState>,
    config: deepclean_core::config::AppConfig,
) -> Result<(), String> {
    *state.config.lock().unwrap() = config;
    Ok(())
}
