/**
 * MQTT live-feed page — minimal DOM handlers.
 *
 * Rendering is done server-side; HTMX SSE (hx-ext="sse") handles the
 * connection and swaps pre-rendered HTML card fragments into the feed.
 *
 * This file only handles:
 *   - filter input: show/hide cards by topic substring
 *   - card scrollback: trim cards beyond MAX_CARDS after each insertion
 */

const MAX_CARDS = 200;

// DOM binding — only runs in the browser
if (typeof document !== 'undefined') {
    const feed = document.getElementById('mqtt-feed');
    const filterInput = document.getElementById('mqtt-filter');
    let filterText = '';

    filterInput?.addEventListener('input', () => {
        filterText = filterInput.value.toLowerCase();
        for (const card of feed?.children ?? []) {
            const topic = (card.dataset.topic ?? '').toLowerCase();
            card.hidden = filterText ? !topic.includes(filterText) : false;
        }
    });

    // Apply filter to newly inserted cards and enforce the scrollback limit.
    document.addEventListener('htmx:afterSwap', (e) => {
        if (e.detail.target?.id !== 'mqtt-feed') return;
        const card = e.detail.target.firstElementChild;
        if (card && filterText && !(card.dataset.topic ?? '').toLowerCase().includes(filterText)) {
            card.hidden = true;
        }
        while (feed?.children.length > MAX_CARDS) {
            feed.removeChild(feed.lastChild);
        }
    });
}
