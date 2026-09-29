use deepclean_core::model::ScanEvent;
use deepclean_core::scanner::{registry, ScanOrchestrator};

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
}
