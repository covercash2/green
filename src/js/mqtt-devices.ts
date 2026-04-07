/**
 * MQTT devices page — device panel loading and command publishing.
 *
 * `fetchDeviceMessages`, `sendCommand`, and `handleDeviceRowClick` are pure
 * exported functions with injected deps so they can be unit-tested without a
 * browser or network.
 *
 * DOM binding at the bottom wires them up and only runs in the browser.
 */

export interface SendResult {
    ok: boolean;
    status?: number | null;
}

/** Injectable controller for the open/close state of a single panel row. */
export interface PanelController {
    /** Was this row's panel open at the moment of the click? */
    readonly wasOpen: boolean;
    /** Close all open panels and remove their open markers. */
    closeAll(): void;
    /** Mark this row as open (called before the async fetch). */
    markOpen(): void;
    /** Is this row still marked open? May be false if another click ran closeAll during the fetch. */
    isStillOpen(): boolean;
    /** Insert the fetched HTML as a panel row after this device row. */
    insertPanel(html: string): void;
}

/** Fetch recent messages HTML for one device from the ring-buffer endpoint. */
export async function fetchDeviceMessages(
    integration: string,
    device: string,
    { fetch: fetchFn = globalThis.fetch }: { fetch?: typeof globalThis.fetch } = {},
): Promise<string> {
    const url = `/api/mqtt/device-messages?integration=${encodeURIComponent(integration)}&device=${encodeURIComponent(device)}`;
    const resp = await fetchFn(url);
    return resp.text();
}

/** POST a message to `/api/mqtt/publish`. Returns a typed result. */
export async function sendCommand(
    topic: string,
    payload: string,
    { fetch: fetchFn = globalThis.fetch }: { fetch?: typeof globalThis.fetch } = {},
): Promise<SendResult> {
    const resp = await fetchFn('/api/mqtt/publish', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ topic, payload }),
    });
    if (resp.ok) return { ok: true };
    return { ok: false, status: resp.status };
}

/**
 * Handle a click on a device row: toggle the panel open/closed.
 *
 * Checks `isStillOpen()` after the async fetch so that a second click (or a
 * click on another row) that runs `closeAll()` during the fetch prevents the
 * stale panel from being inserted.
 */
export async function handleDeviceRowClick(
    integration: string,
    device: string,
    ctrl: PanelController,
    { fetch: fetchFn = globalThis.fetch }: { fetch?: typeof globalThis.fetch } = {},
): Promise<void> {
    const wasOpen = ctrl.wasOpen;
    ctrl.closeAll();
    if (wasOpen) return;

    ctrl.markOpen();

    let html: string;
    try {
        html = await fetchDeviceMessages(integration, device, { fetch: fetchFn });
    } catch (e) {
        html = `<p class="leet-muted">failed to load messages: ${e instanceof Error ? e.message : String(e)}</p>`;
    }

    // Another click may have run closeAll() while we were awaiting — don't insert a stale panel.
    if (!ctrl.isStillOpen()) return;
    ctrl.insertPanel(html);
}

// --- DOM binding (browser only, not tested) ---

if (typeof document !== 'undefined') {
    function makePanelController(el: HTMLElement): PanelController {
        return {
            get wasOpen() { return el.hasAttribute('data-panel-open'); },
            closeAll() {
                document.querySelectorAll('.device-panel-row').forEach(p => { p.remove(); });
                document.querySelectorAll('[data-panel-open]').forEach(r => { r.removeAttribute('data-panel-open'); });
            },
            markOpen() { el.setAttribute('data-panel-open', ''); },
            isStillOpen() { return el.hasAttribute('data-panel-open'); },
            insertPanel(html: string) {
                const panelRow = document.createElement('tr');
                panelRow.className = 'device-panel-row';
                panelRow.innerHTML = `<td colspan="5"><div class="device-panel">${html}</div></td>`;
                el.after(panelRow);
            },
        };
    }

    // Row click — load device panel
    document.querySelectorAll('tr.device-row').forEach(row => {
        const el = row as HTMLElement;
        el.addEventListener('click', () => {
            const integration = el.dataset.integration ?? '';
            const device = el.dataset.device ?? '';
            handleDeviceRowClick(integration, device, makePanelController(el));
        });
    });

    // Form submit — event delegation so it works for dynamically inserted panels.
    document.addEventListener('submit', async (e) => {
        const form = (e.target as Element).closest('.device-cmd-form') as HTMLFormElement | null;
        if (!form) return;
        e.preventDefault();

        const topicEl = form.querySelector('.device-cmd-topic') as HTMLInputElement | null;
        const payloadEl = form.querySelector('.device-cmd-payload') as HTMLTextAreaElement | null;
        const statusEl = form.querySelector('.device-cmd-status') as HTMLElement | null;
        const btn = form.querySelector('button[type="submit"]') as HTMLButtonElement | null;

        const topic = topicEl?.value.trim() ?? '';
        const payload = payloadEl?.value.trim() ?? '';

        if (!topic) {
            if (statusEl) statusEl.textContent = 'topic required';
            return;
        }

        if (btn) btn.disabled = true;
        if (statusEl) statusEl.textContent = 'sending\u2026';

        try {
            const result = await sendCommand(topic, payload);
            if (result.ok) {
                if (statusEl) statusEl.textContent = 'sent';
                if (payloadEl) payloadEl.value = '';
            } else {
                if (statusEl) statusEl.textContent = `error ${result.status}`;
            }
        } catch (err) {
            if (statusEl) statusEl.textContent = `failed: ${err instanceof Error ? err.message : String(err)}`;
        } finally {
            if (btn) btn.disabled = false;
        }
    });
}
