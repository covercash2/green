import { assertEquals } from "jsr:@std/assert";
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
        ...overrides,
    };
}

Deno.test("renderCard: healthy service has svc-healthy class and running label", () => {
    const html = renderCard(svc());
    assertEquals(html.includes("svc-healthy"), true);
    assertEquals(html.includes("● running"), true);
    assertEquals(html.includes("postgresql"), true);
});

Deno.test("renderCard: failed service has svc-failed class and failed label", () => {
    const html = renderCard(svc({ health: "failed", active_state: "failed", sub_state: "failed", pid: null, since: null }));
    assertEquals(html.includes("svc-failed"), true);
    assertEquals(html.includes("✕ failed"), true);
});

Deno.test("renderCard: inactive service has svc-inactive class", () => {
    const html = renderCard(svc({ health: "inactive", active_state: "inactive", sub_state: "dead", pid: null, since: null }));
    assertEquals(html.includes("svc-inactive"), true);
    assertEquals(html.includes("○ inactive"), true);
});

Deno.test("renderCard: degraded service has svc-degraded class", () => {
    const html = renderCard(svc({ health: "degraded", active_state: "active", sub_state: "exited", pid: null }));
    assertEquals(html.includes("svc-degraded"), true);
    assertEquals(html.includes("● exited"), true);
});

Deno.test("renderCard: shows pid when present", () => {
    const html = renderCard(svc({ pid: 9999 }));
    assertEquals(html.includes("9999"), true);
});

Deno.test("renderCard: omits pid row when null", () => {
    const html = renderCard(svc({ pid: null }));
    assertEquals(html.includes(">pid<"), false);
});

Deno.test("renderCard: omits since row when null", () => {
    const html = renderCard(svc({ since: null }));
    assertEquals(html.includes(">since<"), false);
});

Deno.test("renderCard: omits description div when empty", () => {
    const html = renderCard(svc({ description: "" }));
    assertEquals(html.includes("svc-description"), false);
});

Deno.test("renderCard: escapes HTML in name and description", () => {
    const html = renderCard(svc({ name: "<evil>", description: '<script>alert(1)</script>' }));
    assertEquals(html.includes("<evil>"), false);
    assertEquals(html.includes("<script>"), false);
    assertEquals(html.includes("&lt;evil&gt;"), true);
    assertEquals(html.includes("&lt;script&gt;"), true);
});

Deno.test("escHtml: encodes all five special characters", () => {
    assertEquals(escHtml(`<>&"'`), "&lt;&gt;&amp;&quot;&#x27;");
});

Deno.test("escHtml: leaves safe strings unchanged", () => {
    assertEquals(escHtml("hello world"), "hello world");
});

Deno.test("formatLastUpdated: returns a non-empty string", () => {
    const result = formatLastUpdated(new Date("2026-03-29T10:48:00Z"));
    assertEquals(typeof result, "string");
    assertEquals(result.length > 0, true);
});

Deno.test("fetchStatuses: returns parsed statuses from fetch", async () => {
    const statuses = [svc(), svc({ name: "mosquitto", health: "inactive" })];
    const mockFetch = (_url: string) =>
        Promise.resolve(new Response(JSON.stringify(statuses), { status: 200 }));
    const result = await fetchStatuses(mockFetch as typeof fetch);
    assertEquals(result.length, 2);
    assertEquals(result[0].name, "postgresql");
    assertEquals(result[1].health, "inactive");
});

Deno.test("fetchStatuses: throws on non-200 response", async () => {
    const mockFetch = (_url: string) =>
        Promise.resolve(new Response("", { status: 403 }));
    let threw = false;
    try {
        await fetchStatuses(mockFetch as typeof fetch);
    } catch {
        threw = true;
    }
    assertEquals(threw, true);
});

Deno.test("applyUpdate: sets grid innerHTML and updates timestamp", () => {
    const grid = { innerHTML: "" } as HTMLElement;
    const lastUpdated = { textContent: "" } as HTMLElement;
    applyUpdate(grid, lastUpdated, [svc()]);
    assertEquals(grid.innerHTML.includes("svc-card"), true);
    assertEquals(lastUpdated.textContent!.length > 0, true);
});
