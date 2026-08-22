/**
 * Inline SVG sprite. 16x16 grid, 1.5 stroke, currentColor.
 *
 * Self-contained on purpose: the CSP forbids remote origins, and a disk tool
 * should render identically with no network at all.
 */

const SPRITE = `
<svg width="0" height="0" style="position:absolute" aria-hidden="true" focusable="false"><defs>
<symbol id="i-scan" viewBox="0 0 16 16"><path d="M8 1.5a6.5 6.5 0 1 0 6.5 6.5"/><path d="M8 4.5a3.5 3.5 0 1 0 3.5 3.5"/><path d="M8 8 12.6 3.4"/><circle cx="8" cy="8" r=".95" data-solid/></symbol>
<symbol id="i-stop" viewBox="0 0 16 16"><rect x="3.25" y="3.25" width="9.5" height="9.5" rx="1.75"/></symbol>
<symbol id="i-trash" viewBox="0 0 16 16"><path d="M2.6 4.1h10.8"/><path d="M6.4 4.1V2.85a.85.85 0 0 1 .85-.85h1.5a.85.85 0 0 1 .85.85V4.1"/><path d="M12.4 4.1v8.55A1.35 1.35 0 0 1 11.05 14h-6.1A1.35 1.35 0 0 1 3.6 12.65V4.1"/><path d="M6.5 6.9v4.2M9.5 6.9v4.2"/></symbol>
<symbol id="i-folder" viewBox="0 0 16 16"><path d="M2 4.35c0-.75.6-1.35 1.35-1.35h2.28c.42 0 .82.2 1.08.54l.83 1.09c.25.34.65.54 1.08.54h3.03c.75 0 1.35.6 1.35 1.35v5.13c0 .75-.6 1.35-1.35 1.35H3.35C2.6 13 2 12.4 2 11.65z"/></symbol>
<symbol id="i-file" viewBox="0 0 16 16"><path d="M9.1 1.9H4.6a1.4 1.4 0 0 0-1.4 1.4v9.4A1.4 1.4 0 0 0 4.6 14.1h6.8a1.4 1.4 0 0 0 1.4-1.4V5.3z"/><path d="M9.1 1.9v2.4a1 1 0 0 0 1 1h2.7"/></symbol>
<symbol id="i-warning" viewBox="0 0 16 16"><path d="M7.02 2.62a1.13 1.13 0 0 1 1.96 0l5.16 8.88c.44.76-.11 1.71-.98 1.71H2.84c-.87 0-1.42-.95-.98-1.71z"/><path d="M8 6.3v3.05"/><circle cx="8" cy="11.35" r=".78" data-solid/></symbol>
<symbol id="i-shield-check" viewBox="0 0 16 16"><path d="M8 1.75 3.1 3.55v3.98c0 3.05 2.04 5.24 4.9 6.47 2.86-1.23 4.9-3.42 4.9-6.47V3.55z"/><path d="m5.95 7.85 1.45 1.45 2.85-2.85"/></symbol>
<symbol id="i-clock" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.1"/><path d="M8 4.55V8l2.35 1.4"/></symbol>
<symbol id="i-settings" viewBox="0 0 16 16"><path d="M13.3 4.7H7.4"/><path d="M9.3 11.3H3.3"/><circle cx="4.7" cy="4.7" r="2"/><circle cx="11.3" cy="11.3" r="2"/></symbol>
<symbol id="i-chevron-right" viewBox="0 0 16 16"><path d="M6.25 3.5 10.75 8l-4.5 4.5"/></symbol>
<symbol id="i-chevron-down" viewBox="0 0 16 16"><path d="M3.5 6.25 8 10.75l4.5-4.5"/></symbol>
<symbol id="i-chevron-up" viewBox="0 0 16 16"><path d="M3.5 9.75 8 5.25l4.5 4.5"/></symbol>
<symbol id="i-close" viewBox="0 0 16 16"><path d="M11.9 4.1 4.1 11.9M4.1 4.1l7.8 7.8"/></symbol>
<symbol id="i-search" viewBox="0 0 16 16"><circle cx="7.2" cy="7.2" r="4.7"/><path d="m10.65 10.65 3 3"/></symbol>
<symbol id="i-filter" viewBox="0 0 16 16"><path d="M2.5 3.5h11L9.2 8.6v4.32L6.8 14.1V8.6z"/></symbol>
<symbol id="i-history" viewBox="0 0 16 16"><path d="M2.35 8a5.65 5.65 0 1 0 1.7-4.03"/><path d="M2.05 2.4v3h3"/><path d="M8 5.1V8l2.05 1.2"/></symbol>
<symbol id="i-external-link" viewBox="0 0 16 16"><path d="M9.6 2.4h4v4"/><path d="M13.6 2.4 7.85 8.15"/><path d="M12.5 9.5v3a1.5 1.5 0 0 1-1.5 1.5H3.5A1.5 1.5 0 0 1 2 12.5V5a1.5 1.5 0 0 1 1.5-1.5h3"/></symbol>
<symbol id="i-terminal" viewBox="0 0 16 16"><rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.9"/><path d="m4.85 6.3 1.85 1.9-1.85 1.9"/><path d="M8.7 10.35h2.65"/></symbol>
<symbol id="i-hard-drive" viewBox="0 0 16 16"><rect x="1.75" y="4.25" width="12.5" height="7.5" rx="1.9"/><path d="M1.75 8.5h12.5"/><circle cx="11.7" cy="10.1" r=".8" data-solid/><path d="M4.15 10.1h3.1"/></symbol>
<symbol id="i-check" viewBox="0 0 16 16"><path d="m3.4 8.4 3.05 3.05L12.6 5.3"/></symbol>
<symbol id="i-minus" viewBox="0 0 16 16"><path d="M4 8h8"/></symbol>
<symbol id="i-info" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.1"/><path d="M8 7.4v3.6"/><circle cx="8" cy="5.2" r=".8" data-solid/></symbol>
<symbol id="i-more-horizontal" viewBox="0 0 16 16"><circle cx="3.4" cy="8" r="1.1" data-solid/><circle cx="8" cy="8" r="1.1" data-solid/><circle cx="12.6" cy="8" r="1.1" data-solid/></symbol>
<symbol id="i-sidebar-toggle" viewBox="0 0 16 16"><rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.9"/><path d="M10 2.75v10.5"/></symbol>
<symbol id="i-refresh" viewBox="0 0 16 16"><path d="M13.6 8a5.6 5.6 0 1 1-1.64-3.96"/><path d="M13.9 2.5v3h-3"/></symbol>
<symbol id="i-copy" viewBox="0 0 16 16"><rect x="5.6" y="5.6" width="8.4" height="8.4" rx="1.6"/><path d="M11.1 5.6V3.6A1.6 1.6 0 0 0 9.5 2H3.6A1.6 1.6 0 0 0 2 3.6v5.9a1.6 1.6 0 0 0 1.6 1.6h2"/></symbol>
<symbol id="i-plus" viewBox="0 0 16 16"><path d="M8 3.6v8.8M3.6 8h8.8"/></symbol>
<symbol id="i-arrow-right" viewBox="0 0 16 16"><path d="M2.6 8h10.4M9.3 4.3 13 8l-3.7 3.7"/></symbol>
<symbol id="i-lock" viewBox="0 0 16 16"><rect x="3.1" y="6.9" width="9.8" height="7.1" rx="1.7"/><path d="M5.4 6.9V4.9a2.6 2.6 0 0 1 5.2 0v2"/></symbol>
<symbol id="i-alert-circle" viewBox="0 0 16 16"><circle cx="8" cy="8" r="6.1"/><path d="M8 4.9v3.6"/><circle cx="8" cy="11" r=".8" data-solid/></symbol>
<symbol id="i-eco-rust" viewBox="0 0 16 16"><circle cx="8" cy="8" r="5.15"/><circle cx="8" cy="8" r="2.25"/><path d="M8 2.85V1.3M8 14.7v-1.55M13.15 8h1.55M1.3 8h1.55M11.65 4.35 12.75 3.25M3.25 12.75l1.1-1.1M11.65 11.65l1.1 1.1M3.25 3.25l1.1 1.1"/></symbol>
<symbol id="i-eco-node" viewBox="0 0 16 16"><path d="M8 1.55 13.6 4.78v6.44L8 14.45 2.4 11.22V4.78z"/><path d="M6.2 6.3v3.05a1.35 1.35 0 0 1-2.05 1.16"/><path d="M11.85 6.9a2.6 2.6 0 0 0-1.55-.5c-.95 0-1.55.42-1.55 1.06 0 1.42 3.2.6 3.2 2.1 0 .68-.66 1.14-1.7 1.14a2.9 2.9 0 0 1-1.6-.48"/></symbol>
<symbol id="i-eco-apple" viewBox="0 0 16 16"><path d="M8 4.85c-1.28-1.5-3.35-1.28-4.4.2-1.2 1.7-.63 4.45.95 6.35.63.75 1.4 1.5 2.25 1.2.5-.18.8-.5 1.2-.5s.7.32 1.2.5c.85.3 1.62-.45 2.25-1.2 1.58-1.9 2.15-4.65.95-6.35-1.05-1.48-3.12-1.7-4.4-.2z"/><path d="M8 4.85c.05-1.15.95-2.2 2.2-2.45"/></symbol>
<symbol id="i-eco-docker" viewBox="0 0 16 16"><rect x="2.1" y="6.9" width="2.9" height="2.9" rx=".7"/><rect x="5.55" y="6.9" width="2.9" height="2.9" rx=".7"/><rect x="9" y="6.9" width="2.9" height="2.9" rx=".7"/><rect x="5.55" y="3.6" width="2.9" height="2.9" rx=".7"/><path d="M1.4 11.5c1.1 1.7 3.1 2.5 5.4 2.5 4 0 6.6-1.9 7.6-5.2"/></symbol>
<symbol id="i-eco-go" viewBox="0 0 16 16"><path d="M1.2 6.3h3M.6 8h3.1M1.2 9.7h3"/><circle cx="9.9" cy="8" r="4.5"/><path d="M11.8 6.3a2.3 2.3 0 1 0 .35 2.55h-1.6"/></symbol>
<symbol id="i-eco-system" viewBox="0 0 16 16"><rect x="4" y="4" width="8" height="8" rx="1.6"/><rect x="6.4" y="6.4" width="3.2" height="3.2" rx=".8"/><path d="M6.2 1.8v2.2M9.8 1.8v2.2M6.2 12v2.2M9.8 12v2.2M1.8 6.2h2.2M1.8 9.8h2.2M12 6.2h2.2M12 9.8h2.2"/></symbol>
<symbol id="i-eco-python" viewBox="0 0 16 16"><path d="M4.4 3.3h4.1a2.35 2.35 0 0 1 0 4.7h-1a2.35 2.35 0 0 0 0 4.7h4.1"/><circle cx="5.35" cy="4.5" r=".62" data-solid/></symbol>
<symbol id="i-eco-java" viewBox="0 0 16 16"><path d="M2.5 6.6h8.15v3.6a2.7 2.7 0 0 1-2.7 2.7H5.2a2.7 2.7 0 0 1-2.7-2.7z"/><path d="M10.65 7.5h1.15a1.75 1.75 0 0 1 0 3.5h-1.15"/><path d="M5.5 4.5c.65-.62.65-1.28 0-1.9M8.1 4.5c.65-.62.65-1.28 0-1.9"/></symbol>
<symbol id="i-eco-homebrew" viewBox="0 0 16 16"><path d="M8 2.2c2.6 0 4.6 2 4.6 4.6 0 3.4-2.7 6.05-4.6 7.2C6.1 12.85 3.4 10.2 3.4 6.8 3.4 4.2 5.4 2.2 8 2.2z"/><path d="M8 5v6.6"/><path d="m5.6 6.5 2.4 1.85 2.4-1.85"/><path d="m5.8 9.2 2.2 1.7 2.2-1.7"/></symbol>
<symbol id="i-eco-jet_brains" viewBox="0 0 16 16"><rect x="1.9" y="1.9" width="12.2" height="12.2" rx="2.3"/><path d="M4.4 11.9h4.3"/><path d="M9.5 4.5v3.35a1.75 1.75 0 0 1-3.4.55"/></symbol>
<symbol id="i-eco-dot_net" viewBox="0 0 16 16"><circle cx="2.3" cy="12" r="1.15" data-solid/><path d="M5.6 12.5V3.5l5.6 9V3.5"/><path d="M12.9 3.5h1.6M13.7 3.5v9"/></symbol>
</defs></svg>`;

/** The brand mark: a ring with a gap. Also the scan indicator and tray glyph. */
export const MARK = `<svg viewBox="0 0 20 20" width="20" height="20" aria-hidden="true">
  <circle cx="10" cy="10" r="6.75" fill="none" stroke="currentColor" stroke-width="2.5"
    stroke-linecap="round" pathLength="100" stroke-dasharray="88 12" stroke-dashoffset="-81"/></svg>`;

export function installSprite() {
  document.body.insertAdjacentHTML('afterbegin', SPRITE);
}

/** Icon markup by id, e.g. icon('scan') or icon('trash', 'icon icon-sm'). */
export function icon(id, cls = 'icon') {
  return `<svg class="${cls}" aria-hidden="true"><use href="#i-${id}"/></svg>`;
}

/**
 * Ecosystem glyph id. Keys match the backend's snake_case serialization —
 * `jet_brains`, not `jetbrains`, which is what the old palette got wrong and
 * why JetBrains rendered grey.
 */
export function ecoIcon(ecosystem, cls = 'icon icon-sm') {
  return icon(`eco-${ecosystem}`, cls);
}
