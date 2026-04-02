// Service status dashboard — polls /api/services and updates the grid in place.

const POLL_INTERVAL_MS = 15_000;

export interface ServiceStatus {
    name: string;
    description: string;
    load_state: string;
    active_state: string;
    sub_state: string;
    pid: number | null;
    since: string | null;
    health: "healthy" | "degraded" | "inactive" | "failed";
    icon_url: string | null;
    url: string | null;
}

const HEALTH_CLASS: Record<ServiceStatus["health"], string> = {
    healthy: "svc-healthy",
    degraded: "svc-degraded",
    inactive: "svc-inactive",
    failed: "svc-failed",
};

const HEALTH_LABEL: Record<ServiceStatus["health"], string> = {
    healthy: "● running",
    degraded: "● exited",
    inactive: "○ inactive",
    failed: "✕ failed",
};

export function renderCard(svc: ServiceStatus): string {
    const healthClass = HEALTH_CLASS[svc.health] ?? "svc-failed";
    const label = HEALTH_LABEL[svc.health] ?? svc.health;
    const descRow = svc.description
        ? `<div class="svc-description">${escHtml(svc.description)}</div>`
        : "";
    const pidRow = svc.pid != null
        ? `<span class="svc-key">pid</span><span class="svc-val">${svc.pid}</span>`
        : "";
    const sinceRow = svc.since != null
        ? `<span class="svc-key">since</span><span class="svc-val svc-timestamp">${escHtml(svc.since)}</span>`
        : "";
    const iconSrc = svc.icon_url ?? "/assets/img/service.svg";
    const iconHtml = `<img src="${escHtml(iconSrc)}" alt="" class="svc-icon" aria-hidden="true" width="18" height="18">`;
    const nameHtml = svc.url
        ? `<a href="${escHtml(svc.url)}" class="svc-name svc-link">${escHtml(svc.name)}</a>`
        : `<span class="svc-name">${escHtml(svc.name)}</span>`;
    return `
<div class="svc-card ${healthClass}">
  <div class="svc-card-header">
    ${iconHtml}
    ${nameHtml}
    <span class="svc-badge ${healthClass}">${label}</span>
  </div>
  ${descRow}
  <div class="svc-fields">
    <span class="svc-key">state</span>
    <span class="svc-val">${escHtml(svc.active_state)}/${escHtml(svc.sub_state)}</span>
    ${pidRow}
    ${sinceRow}
  </div>
</div>`.trim();
}

export function escHtml(s: string): string {
    return s
        .replace(/&/g, "&amp;")
        .replace(/</g, "&lt;")
        .replace(/>/g, "&gt;")
        .replace(/"/g, "&quot;")
        .replace(/'/g, "&#x27;");
}

export function formatLastUpdated(date: Date): string {
    return date.toLocaleTimeString();
}

export async function fetchStatuses(
    fetchFn: typeof fetch = fetch,
): Promise<ServiceStatus[]> {
    const res = await fetchFn("/api/services");
    if (!res.ok) throw new Error(`/api/services returned ${res.status}`);
    return res.json() as Promise<ServiceStatus[]>;
}

export function applyUpdate(
    grid: HTMLElement,
    lastUpdatedEl: HTMLElement,
    statuses: ServiceStatus[],
): void {
    grid.innerHTML = statuses.map(renderCard).join("\n");
    lastUpdatedEl.textContent = formatLastUpdated(new Date());
}

if (typeof document !== "undefined") {
    const grid = document.getElementById("svc-grid");
    const lastUpdated = document.getElementById("svc-last-updated");
    const refreshBtn = document.getElementById("svc-refresh");

    async function refresh(): Promise<void> {
        try {
            const statuses = await fetchStatuses();
            if (grid && lastUpdated) applyUpdate(grid, lastUpdated, statuses);
        } catch (e) {
            console.error("services refresh failed:", e);
        }
    }

    if (refreshBtn) refreshBtn.addEventListener("click", () => { void refresh(); });

    setInterval(() => { void refresh(); }, POLL_INTERVAL_MS);
}
