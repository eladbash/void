use deepclean_core::model::ScanEvent;
use deepclean_core::scanner::{registry, ScanOrchestrator};
use uuid::Uuid;

use super::{Backend, UiEvent};

impl Backend {
    /// Start a scan of the configured roots, streaming its events.
    ///
    /// Refused while one is running: two rapid triggers — the button and the
    /// tray, say — must not both start a scan.
    pub fn start_scan(&self) -> Result<(), String> {
        if !self.state.begin_scan() {
            return Err("Scan already in progress".into());
        }
        self.state.results().clear();

        let config = self.state.config().clone();
        let scanners = registry::build(&config);
        let this = self.clone();
        self.rt.spawn(async move {
            let mut rx = ScanOrchestrator::new(scanners, config).start_scan();
            while let Some(event) = rx.recv().await {
                // A sandboxed app (VOID_HOME) must never show — or offer to
                // run — anything that reaches past the fake home.
                let event = match (event, deepclean_core::paths::home_override()) {
                    (ScanEvent::ItemFound { item }, Some(home)) => {
                        match deepclean_core::sandbox::confine(vec![item], &home).pop() {
                            Some(item) => ScanEvent::ItemFound { item },
                            None => continue,
                        }
                    }
                    (event, _) => event,
                };
                match &event {
                    ScanEvent::ItemFound { item } => this.state.results().push(item.clone()),
                    ScanEvent::ScanComplete { summary } => *this.state.summary() = summary.clone(),
                    _ => {}
                }
                this.emit(UiEvent::Scan(event));
            }
            // Released here rather than on ScanComplete: if the orchestrator
            // dies or the channel closes without a final event, the latch
            // would otherwise stay set and refuse every later scan.
            this.state.finish_scan();
            this.emit(UiEvent::ScanFinished);
            this.emit(UiEvent::TrayRefresh);
        });
        Ok(())
    }

    /// Re-analyze one item after a partial clean so its remaining actions
    /// and sizes reflect what is left. Docker's prunes overlap (`system
    /// prune` covers the others), so subtracting estimates is not enough.
    ///
    /// The item keeps its optimistic update if analysis fails, or if a full
    /// scan replaced the results in the meantime.
    pub fn refresh_item(&self, item_id: Uuid) {
        let Some(old) = self
            .state
            .results()
            .iter()
            .find(|i| i.id == item_id)
            .cloned()
        else {
            return;
        };
        let config = self.state.config().clone();
        let Some(scanner) = registry::build(&config)
            .into_iter()
            .find(|s| s.ecosystem() == old.ecosystem)
        else {
            return;
        };
        let this = self.clone();
        self.rt.spawn(async move {
            let Ok(found) = scanner.analyze_many(&old.path).await else {
                return;
            };
            let mut items: Vec<_> = found
                .into_iter()
                .map(deepclean_core::trash::add_trash_alternatives)
                .filter(|i| i.ecosystem == old.ecosystem && i.kind == old.kind)
                .collect();
            if let Some(home) = deepclean_core::paths::home_override() {
                items = deepclean_core::sandbox::confine(items, &home);
            }
            {
                let mut results = this.state.results();
                let Some(pos) = results.iter().position(|i| i.id == item_id) else {
                    return;
                };
                results.splice(pos..=pos, items.iter().cloned());
            }
            this.emit(UiEvent::ItemRefreshed { item_id, items });
            this.emit(UiEvent::TrayRefresh);
        });
    }
}
