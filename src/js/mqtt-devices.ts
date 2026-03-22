/**
 * MQTT devices page — device panel loading and command publishing.
 *
 * `fetchDeviceMessages` and `sendCommand` are pure exported functions with
 * injected deps so they can be unit-tested without a browser or network.
 *
 * DOM binding at the bottom wires them up and only runs in the browser.
 */

export interface SendResult {
    ok: boolean;
    status?: number | null;
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

// --- DOM binding (browser only, not tested) ---

if (typeof document !== 'undefined') {
    // Row click — load device panel
    document.querySelectorAll('tr.device-row').forEach(row => {
        (row as HTMLElement).addEventListener('click', async () => {
            const el = row as HTMLElement;
            const integration = el.dataset.integration ?? '';
            const device = el.dataset.device ?? '';
            const wasOpen = el.hasAttribute('data-panel-open');

            // Close all open panels and clear markers.
            document.querySelectorAll('.device-panel-row').forEach(p => p.remove());
            document.querySelectorAll('[data-panel-open]').forEach(r => r.removeAttribute('data-panel-open'));

            if (wasOpen) return;

            el.setAttribute('data-panel-open', '');

            let html: string;
            try {
                html = await fetchDeviceMessages(integration, device);
            } catch (e) {
                html = `<p class="leet-muted">failed to load messages: ${e instanceof Error ? e.message : String(e)}</p>`;
            }

            const panelRow = document.createElement('tr');
            panelRow.className = 'device-panel-row';
            panelRow.innerHTML = `<td colspan="5"><div class="device-panel">${html}</div></td>`;
            el.after(panelRow);
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
