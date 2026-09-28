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

export const getGuardStatus = () => invoke('get_guard_status');
export const runGuardNow = () => invoke('run_guard_now');
export const getAgentUsage = () => invoke('get_agent_usage');
export const getHookStatus = () => invoke('get_hook_status');
export const installHooks = () => invoke('install_hooks');
export const uninstallHooks = () => invoke('uninstall_hooks');
export const getMcpSnippet = () => invoke('get_mcp_snippet');
export const setLaunchAtLogin = (enabled) => invoke('set_launch_at_login', { enabled });

/** Guard mode's verdict after every check (timer or "Check now"). */
export const onGuardStatus = (fn) => listen('guard-status', (e) => fn(e.payload));
/** A path Guard mode's automatic cleanup removed, to drop from Results. */
export const onGuardCleaned = (fn) => listen('guard-cleaned', (e) => fn(e.payload));
/** History changed outside the UI (an automatic cleanup finished). */
export const onHistoryChanged = (fn) => listen('history-changed', () => fn());
/** A tray menu request, e.g. `"idle-worktrees"`. */
export const onTrayAction = (fn) => listen('tray-action', (e) => fn(e.payload));
