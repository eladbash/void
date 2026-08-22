/** Thin wrapper over the Tauri bridge, so screens never touch it directly. */

const tauri = window.__TAURI__;
export const invoke = tauri?.core?.invoke ?? (async () => { throw new Error('Tauri bridge unavailable'); });
export const listen = tauri?.event?.listen ?? (async () => () => {});

export const startScan = () => invoke('start_scan');
export const executeClean = (selections) => invoke('execute_clean', { request: { selections } });
export const cancelClean = () => invoke('cancel_clean');
export const getConfig = () => invoke('get_config');
export const updateConfig = (config) => invoke('update_config', { config });
export const getConfigPath = () => invoke('get_config_path');
export const revealItem = (itemId) => invoke('reveal_item', { itemId });
export const getDiskUsage = () => invoke('get_disk_usage');
export const getHistory = () => invoke('get_history');
export const clearHistory = () => invoke('clear_history');
export const takeStartupWarnings = () => invoke('take_startup_warnings');

export const onScanEvent = (fn) => listen('scan-event', (e) => fn(e.payload));
export const onActionEvent = (fn) => listen('action-event', (e) => fn(e.payload));
export const onAppWarning = (fn) => listen('app-warning', (e) => fn(e.payload));
