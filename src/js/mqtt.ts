/**
 * MQTT live-feed page — EventSource + pagination.
 *
 * Pure exported functions (applyFilter, getPage, totalPages, renderControls)
 * are tested in test/js/mqtt.test.ts.
 *
 * DOM binding at the bottom only runs in the browser.
 */

const PAGE_SIZE = 20;
const MAX_CARDS = 500;

// --- Pure functions (exported for testing) ---

interface Card {
    dataset?: { topic?: string };
    cloneNode(deep?: boolean): Node;
}

/** Cards whose data-topic matches filterText (case-insensitive substring). */
export function applyFilter(cards: Card[], filterText: string): Card[] {
    if (!filterText) return cards;
    const lower = filterText.toLowerCase();
    return cards.filter(c => (c.dataset?.topic ?? '').toLowerCase().includes(lower));
}

/** Slice of cards for a given 0-indexed page. */
export function getPage<T>(cards: T[], page: number, pageSize: number): T[] {
    const start = page * pageSize;
    return cards.slice(start, start + pageSize);
}

/** Total page count (minimum 1). */
export function totalPages(cards: Card[], pageSize: number): number {
    return Math.max(1, Math.ceil(cards.length / pageSize));
}

/** Compute which page indices (0-based) to show; null means ellipsis. */
function pageWindow(currentPage: number, total: number): (number | null)[] {
    if (total <= 7) return Array.from({ length: total }, (_, i) => i);
    let start = Math.max(1, currentPage - 2);
    let end   = Math.min(total - 2, currentPage + 2);
    if (end - start < 4) {
        if (start === 1) end   = Math.min(total - 2, start + 4);
        else             start = Math.max(1, end - 4);
    }
    const pages: (number | null)[] = [0];
    if (start > 1) pages.push(null);
    for (let i = start; i <= end; i++) pages.push(i);
    if (end < total - 2) pages.push(null);
    pages.push(total - 1);
    return pages;
}

/**
 * Render pagination controls as an HTML string.
 * Badge `(+N)` appears on the page-1 button when newCount > 0 and currentPage > 0.
 */
export function renderControls(currentPage: number, total: number, newCount: number): string {
    const parts: string[] = [];
    const prevDisabled = currentPage === 0 ? ' disabled' : '';
    parts.push(
        `<button class="mqtt-page-btn" data-page="${currentPage - 1}"${prevDisabled}>[ ← ]</button>`
    );
    for (const p of pageWindow(currentPage, total)) {
        if (p === null) {
            parts.push(`<span class="mqtt-page-ellipsis">…</span>`);
        } else {
            const active = p === currentPage ? ' mqtt-page-active' : '';
            const badge  = (p === 0 && newCount > 0 && currentPage > 0)
                ? `<span class="mqtt-page-new">(+${newCount})</span>`
                : '';
            parts.push(
                `<button class="mqtt-page-btn${active}" data-page="${p}">[ ${p + 1}${badge} ]</button>`
            );
        }
    }
    const nextDisabled = currentPage === total - 1 ? ' disabled' : '';
    parts.push(
        `<button class="mqtt-page-btn" data-page="${currentPage + 1}"${nextDisabled}>[ → ]</button>`
    );
    return parts.join('');
}

// --- DOM binding (not tested) ---

if (typeof document !== 'undefined') {
    const feed        = document.getElementById('mqtt-feed');
    const statusBar   = document.getElementById('mqtt-status-bar');
    const filterInput = document.getElementById('mqtt-filter') as HTMLInputElement | null;
    const controls    = document.getElementById('mqtt-controls');

    let allCards: Card[] = [];
    let currentPage = 0;
    let newCount    = 0;
    let filterText  = '';

    function render() {
        const filtered = applyFilter(allCards, filterText);
        const total    = totalPages(filtered, PAGE_SIZE);
        if (currentPage >= total) currentPage = Math.max(0, total - 1);
        const page = getPage(filtered, currentPage, PAGE_SIZE);

        if (feed) {
            feed.innerHTML = '';
            for (const card of page) {
                feed.appendChild(card.cloneNode(true));
            }
        }
        if (controls) {
            controls.innerHTML = renderControls(currentPage, total, newCount);
        }
    }

    controls?.addEventListener('click', (e) => {
        const btn = (e.target as Element).closest('[data-page]') as HTMLButtonElement | null;
        if (!btn || btn.disabled) return;
        const page     = parseInt(btn.dataset.page!, 10);
        const filtered = applyFilter(allCards, filterText);
        const total    = totalPages(filtered, PAGE_SIZE);
        if (page < 0 || page >= total) return;
        currentPage = page;
        newCount    = 0;
        render();
    });

    filterInput?.addEventListener('input', () => {
        filterText  = filterInput.value.toLowerCase();
        currentPage = 0;
        newCount    = 0;
        render();
    });

    const es = new EventSource('/api/mqtt/stream');

    es.addEventListener('broker', (e) => {
        if (statusBar) statusBar.innerHTML = (e as MessageEvent).data;
    });

    es.addEventListener('message', (e) => {
        const tmp  = document.createElement('div');
        tmp.innerHTML = (e as MessageEvent).data;
        const card = tmp.firstElementChild;
        if (!card) return;
        allCards.unshift(card as unknown as Card);
        if (allCards.length > MAX_CARDS) allCards.length = MAX_CARDS;
        if (currentPage === 0) {
            render();
        } else {
            newCount++;
            const filtered = applyFilter(allCards, filterText);
            const total    = totalPages(filtered, PAGE_SIZE);
            if (controls) controls.innerHTML = renderControls(currentPage, total, newCount);
        }
    });
}
