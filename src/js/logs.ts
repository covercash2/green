/**
 * Log viewer page — EventSource-based live tail of server log files.
 *
 * Pure exported functions (formatAppLine, isNearBottom) are tested in
 * test/js/logs.test.ts.
 *
 * DOM binding at the bottom only runs in the browser.
 */

// --- Types ---

/** Parsed fields from a tracing-subscriber JSON log line. */
export interface AppLogEntry {
    timestamp?: string;
    level?: string;
    fields?: { message?: string; [key: string]: unknown };
    target?: string;
    span?: { name?: string };
}

/** An element with scroll geometry, extracted for testability. */
export interface Scrollable {
    scrollHeight: number;
    scrollTop: number;
    clientHeight: number;
}

// --- Pure functions (exported for testing) ---

/** Fields that are already represented in the formatted prefix and should not be repeated. */
const SUPPRESSED_FIELDS = new Set(['message', 'summary']);

/**
 * Extract the most useful summary string from a tracing-subscriber JSON entry's
 * `fields` object. Prefers `message`, then `summary`, as the primary text, then
 * appends any remaining non-empty fields as `key=value` pairs so structured
 * context (e.g. `err=`, `topic=`) is always visible.
 */
export function extractMessage(fields: Record<string, unknown>): string {
    const primary = (typeof fields['message'] === 'string' && fields['message'])
        ? fields['message']
        : (typeof fields['summary'] === 'string' && fields['summary'])
            ? fields['summary']
            : '';

    const extra = Object.entries(fields)
        .filter(([k, v]) => !SUPPRESSED_FIELDS.has(k) && v !== null && v !== undefined && v !== '')
        .map(([k, v]) => `${k}=${JSON.stringify(v)}`)
        .join(' ');

    return [primary, extra].filter(Boolean).join(' ');
}

/**
 * Try to parse a JSON log line and format it as a human-readable string.
 * Returns the raw line and level `"raw"` on parse failure.
 */
export function formatAppLine(line: string): { text: string; level: string } {
    try {
        const entry = JSON.parse(line) as AppLogEntry;
        const time = entry.timestamp?.slice(11, 19) ?? '';
        const level = (entry.level ?? 'INFO').toUpperCase();
        const target = entry.target ? `[${entry.target}]` : '';
        const message = entry.fields ? extractMessage(entry.fields) : '';
        const text = [time, level, target, message].filter(Boolean).join(' ');
        return { text, level: level.toLowerCase() };
    } catch {
        return { text: line, level: 'raw' };
    }
}

/**
 * Returns true when `el` is scrolled within 100 px of its bottom edge.
 * Used to decide whether to auto-scroll on new log lines.
 */
export function isNearBottom(el: Scrollable): boolean {
    return el.scrollHeight - el.scrollTop - el.clientHeight < 100;
}

// --- DOM binding ---

if (typeof document !== 'undefined') {
    const feed = document.getElementById('log-feed') as HTMLElement | null;
    const autoscrollCheckbox = document.getElementById('logs-autoscroll') as HTMLInputElement | null;
    const streamUrl = feed?.dataset['stream'];

    if (feed && streamUrl) {
        const isAppLog = streamUrl.includes('/app/');
        const es = new EventSource(streamUrl);

        es.addEventListener('message', (ev: MessageEvent<string>) => {
            const line: string = ev.data;
            const div = document.createElement('div');
            div.className = 'log-line';

            if (isAppLog) {
                const { text, level } = formatAppLine(line);
                div.textContent = text;
                div.classList.add(`log-line-${level}`);
            } else {
                div.textContent = line;
                div.classList.add('log-line-raw');
            }

            const shouldScroll = autoscrollCheckbox?.checked && isNearBottom(feed);
            feed.appendChild(div);
            if (shouldScroll) {
                feed.scrollTop = feed.scrollHeight;
            }
        });
    }
}
