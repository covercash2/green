/**
 * MQTT live-feed page — SSE client.
 *
 * `connectMqtt` is a pure logic function with injected deps so it can be
 * unit-tested without a browser or network.
 *
 * The DOM binding at the bottom wires it up to the actual page elements and
 * only runs in a browser context.
 */

/**
 * @param {string} streamUrl
 * @param {{ EventSource?: typeof EventSource, onMessage?: Function, onStatus?: Function }} deps
 * @returns {{ close: Function }}
 */
export function connectMqtt(streamUrl, { EventSource: ES = EventSource, onMessage = () => {}, onStatus = () => {} } = {}) {
    const source = new ES(streamUrl);

    source.onopen = () => onStatus('connected');
    source.onerror = () => onStatus('error');

    source.onmessage = (event) => {
        try {
            const msg = JSON.parse(event.data);
            onMessage(msg);
        } catch (_) {
            // ignore malformed frames
        }
    };

    return { close: () => source.close() };
}

// DOM binding — only runs in the browser
if (typeof document !== 'undefined') {
    const dot = document.getElementById('mqtt-status-dot');
    const statusText = document.getElementById('mqtt-status-text');
    const tbody = document.getElementById('mqtt-tbody');

    const MAX_ROWS = 200;

    function setStatus(state) {
        if (!dot || !statusText) return;
        dot.className = 'mqtt-dot mqtt-dot-' + state;
        statusText.textContent = state;
    }

    function onMessage(msg) {
        if (!tbody) return;
        const tr = document.createElement('tr');
        tr.className = 'mqtt-row';

        const tdTopic = document.createElement('td');
        tdTopic.className = 'mqtt-td mqtt-topic';
        tdTopic.textContent = msg.topic ?? '';

        const tdPayload = document.createElement('td');
        tdPayload.className = 'mqtt-td mqtt-payload';
        tdPayload.textContent = msg.payload ?? '';

        const tdTime = document.createElement('td');
        tdTime.className = 'mqtt-td mqtt-ts';
        tdTime.textContent = msg.received_at ?? '';

        tr.appendChild(tdTopic);
        tr.appendChild(tdPayload);
        tr.appendChild(tdTime);

        tbody.prepend(tr);

        // Trim old rows to prevent unbounded DOM growth.
        while (tbody.rows.length > MAX_ROWS) {
            tbody.deleteRow(tbody.rows.length - 1);
        }
    }

    connectMqtt('/api/mqtt/stream', { onMessage, onStatus: setStatus });
}
