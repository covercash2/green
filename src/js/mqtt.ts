/**
 * MQTT live-feed page — EventSource + pagination.
 *
 * Pure exported functions (applyFilter, getPage, totalPages, renderControls)
 * are tested in test/js/mqtt.test.ts.
 *
 * DOM binding at the bottom only runs in the browser.
 */

import { batch, computed, effect, signal } from '@preact/signals-core';

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

// --- DOM binding (browser only, not tested) ---

if (typeof document !== 'undefined') {
    const feed        = document.getElementById('mqtt-feed');
    const statusBar   = document.getElementById('mqtt-status-bar');
    const filterInput = document.getElementById('mqtt-filter') as HTMLInputElement | null;
    const controls    = document.getElementById('mqtt-controls');

    // State
    const allCards    = signal<Card[]>([]);
    const pageCards   = signal<Card[]>([]);  // feed snapshot — set explicitly, not derived
    const currentPage = signal(0);
    const newCount    = signal(0);
    const filterText  = signal('');

    // Derived
    const filtered   = computed(() => applyFilter(allCards.value, filterText.value));
    const totalCount = computed(() => totalPages(filtered.value, PAGE_SIZE));

    // Recompute pageCards from current state (call inside batch).
    function refreshPage() {
        const total = totalCount.value;
        if (currentPage.value >= total) currentPage.value = Math.max(0, total - 1);
        pageCards.value = getPage(filtered.value, currentPage.value, PAGE_SIZE);
    }

    // Feed DOM — only re-renders when pageCards changes.
    effect(() => {
        if (!feed) return;
        feed.innerHTML = '';
        for (const card of pageCards.value) {
            feed.appendChild(card.cloneNode(true));
        }
    });

    // Controls DOM — re-renders when page, total, or newCount changes.
    effect(() => {
        if (!controls) return;
        controls.innerHTML = renderControls(currentPage.value, totalCount.value, newCount.value);
    });

    controls?.addEventListener('click', (e) => {
        const btn = (e.target as Element).closest('[data-page]') as HTMLButtonElement | null;
        if (!btn || btn.disabled) return;
        const page = parseInt(btn.dataset.page!, 10);
        if (page < 0 || page >= totalCount.value) return;
        batch(() => {
            currentPage.value = page;
            newCount.value    = 0;
            refreshPage();
        });
    });

    filterInput?.addEventListener('input', () => {
        batch(() => {
            filterText.value  = filterInput.value.toLowerCase();
            currentPage.value = 0;
            newCount.value    = 0;
            refreshPage();
        });
    });

    const es = new EventSource('/api/mqtt/stream');

    es.addEventListener('broker', (e) => {
        if (statusBar) statusBar.innerHTML = (e as MessageEvent).data;
    });

    es.addEventListener('message', (e) => {
        const tmp = document.createElement('div');
        tmp.innerHTML = (e as MessageEvent).data;
        const card = tmp.firstElementChild;
        if (!card) return;

        const cards = allCards.value.slice();
        cards.unshift(card as unknown as Card);
        if (cards.length > MAX_CARDS) cards.length = MAX_CARDS;

        if (currentPage.value === 0) {
            batch(() => {
                allCards.value = cards;
                refreshPage();
            });
        } else {
            // Don't update the feed — user is reading a different page.
            // Update allCards (so totalCount stays accurate) and badge newCount.
            batch(() => {
                allCards.value = cards;
                newCount.value++;
            });
        }
    });
}
