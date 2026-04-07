import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyUpdate, escHtml, fetchStatuses, formatLastUpdated, renderCard } from "../../src/js/services.ts";
import type { ServiceStatus } from "../../src/js/services.ts";

function svc(overrides: Partial<ServiceStatus> = {}): ServiceStatus {
    return {
        name: "postgresql",
        description: "PostgreSQL Server",
        load_state: "loaded",
        active_state: "active",
        sub_state: "running",
        pid: 1234,
        since: "Sun 2026-03-29 10:48:26 CDT",
        health: "healthy",
        icon_url: null,
        url: null,
        ...overrides,
    };
}

test("renderCard: healthy service has svc-healthy class and running label", () => {
    const html = renderCard(svc());
    assert.ok(html.includes("svc-healthy"));
    assert.ok(html.includes("● running"));
    assert.ok(html.includes("postgresql"));
});

test("renderCard: failed service has svc-failed class and failed label", () => {
    const html = renderCard(svc({ health: "failed", active_state: "failed", sub_state: "failed", pid: null, since: null }));
    assert.ok(html.includes("svc-failed"));
    assert.ok(html.includes("✕ failed"));
});

test("renderCard: inactive service has svc-inactive class", () => {
    const html = renderCard(svc({ health: "inactive", active_state: "inactive", sub_state: "dead", pid: null, since: null }));
    assert.ok(html.includes("svc-inactive"));
    assert.ok(html.includes("○ inactive"));
});

test("renderCard: degraded service has svc-degraded class", () => {
    const html = renderCard(svc({ health: "degraded", active_state: "active", sub_state: "exited", pid: null }));
    assert.ok(html.includes("svc-degraded"));
    assert.ok(html.includes("● exited"));
});

test("renderCard: shows pid when present", () => {
    const html = renderCard(svc({ pid: 9999 }));
    assert.ok(html.includes("9999"));
});

test("renderCard: omits pid row when null", () => {
    const html = renderCard(svc({ pid: null }));
    assert.ok(!html.includes(">pid<"));
});

test("renderCard: omits since row when null", () => {
    const html = renderCard(svc({ since: null }));
    assert.ok(!html.includes(">since<"));
});

test("renderCard: omits description div when empty", () => {
    const html = renderCard(svc({ description: "" }));
    assert.ok(!html.includes("svc-description"));
});

test("renderCard: escapes HTML in name and description", () => {
    const html = renderCard(svc({ name: "<evil>", description: '<script>alert(1)</script>' }));
    assert.ok(!html.includes("<evil>"));
    assert.ok(!html.includes("<script>"));
    assert.ok(html.includes("&lt;evil&gt;"));
    assert.ok(html.includes("&lt;script&gt;"));
});

test("renderCard: name is a link when url is set", () => {
    const html = renderCard(svc({ url: "https://example.com" }));
    assert.ok(html.includes(`href="https://example.com"`));
    assert.ok(html.includes("svc-link"));
});

test("renderCard: name is a span when url is null", () => {
    const html = renderCard(svc({ url: null }));
    assert.ok(!html.includes(`href=`));
});

test("renderCard: renders icon img when icon_url is set", () => {
    const html = renderCard(svc({ icon_url: "https://example.com/icon.svg" }));
    assert.ok(html.includes(`src="https://example.com/icon.svg"`));
    assert.ok(html.includes("svc-icon"));
});

test("renderCard: uses fallback icon when icon_url is null", () => {
    const html = renderCard(svc({ icon_url: null }));
    assert.ok(html.includes("svc-icon"));
    assert.ok(html.includes("/assets/img/service.svg"));
});

test("escHtml: encodes all five special characters", () => {
    assert.equal(escHtml(`<>&"'`), "&lt;&gt;&amp;&quot;&#x27;");
});

test("escHtml: leaves safe strings unchanged", () => {
    assert.equal(escHtml("hello world"), "hello world");
});

test("formatLastUpdated: returns a non-empty string", () => {
    const result = formatLastUpdated(new Date("2026-03-29T10:48:00Z"));
    assert.equal(typeof result, "string");
    assert.ok(result.length > 0);
});

test("fetchStatuses: returns parsed statuses from fetch", async () => {
    const statuses = [svc(), svc({ name: "mosquitto", health: "inactive" })];
    const mockFetch = (_url: string) =>
        Promise.resolve(new Response(JSON.stringify(statuses), { status: 200 }));
    const result = await fetchStatuses(mockFetch as typeof fetch);
    assert.equal(result.length, 2);
    assert.equal(result[0].name, "postgresql");
    assert.equal(result[1].health, "inactive");
});

test("fetchStatuses: throws on non-200 response", async () => {
    const mockFetch = (_url: string) =>
        Promise.resolve(new Response("", { status: 403 }));
    await assert.rejects(() => fetchStatuses(mockFetch as typeof fetch));
});

test("applyUpdate: sets grid innerHTML and updates timestamp", () => {
    const grid = { innerHTML: "" } as HTMLElement;
    const lastUpdated = { textContent: "" } as HTMLElement;
    applyUpdate(grid, lastUpdated, [svc()]);
    assert.ok(grid.innerHTML.includes("svc-card"));
    assert.ok(lastUpdated.textContent?.length > 0);
});
