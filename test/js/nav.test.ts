import { assertEquals } from "jsr:@std/assert";
import { initNav } from "../../src/js/nav.ts";

const mockDoc = { addEventListener() {} } as unknown as Document;

function makeDrawer(open = false): HTMLElement {
    let isOpen = open;
    return {
        classList: {
            contains: () => isOpen,
            add() { isOpen = true; },
            remove() { isOpen = false; },
        },
        setAttribute() {},
        getAttribute: () => null,
        get _isOpen() { return isOpen; },
    } as unknown as HTMLElement;
}

Deno.test("initNav: hamburger click opens drawer", () => {
    const listeners: Record<string, EventListener> = {};
    const hamburger = { addEventListener: (e: string, fn: EventListener) => { listeners[e] = fn; }, setAttribute() {} } as unknown as HTMLElement;
    const drawer = makeDrawer(false);
    const overlay = { addEventListener() {} } as unknown as HTMLElement;

    initNav(hamburger, drawer, overlay, mockDoc);
    listeners["click"]({} as Event);

    assertEquals((drawer as unknown as { _isOpen: boolean })._isOpen, true);
});

Deno.test("initNav: second click closes drawer", () => {
    const listeners: Record<string, EventListener> = {};
    const hamburger = { addEventListener: (e: string, fn: EventListener) => { listeners[e] = fn; }, setAttribute() {} } as unknown as HTMLElement;
    const drawer = makeDrawer(false);
    const overlay = { addEventListener() {} } as unknown as HTMLElement;

    initNav(hamburger, drawer, overlay, mockDoc);
    listeners["click"]({} as Event);
    listeners["click"]({} as Event);

    assertEquals((drawer as unknown as { _isOpen: boolean })._isOpen, false);
});

Deno.test("initNav: overlay click closes drawer", () => {
    const overlayListeners: Record<string, EventListener> = {};
    const hamburger = { addEventListener() {}, setAttribute() {} } as unknown as HTMLElement;
    const drawer = makeDrawer(true);
    const overlay = { addEventListener: (e: string, fn: EventListener) => { overlayListeners[e] = fn; } } as unknown as HTMLElement;

    initNav(hamburger, drawer, overlay, mockDoc);
    overlayListeners["click"]({} as Event);

    assertEquals((drawer as unknown as { _isOpen: boolean })._isOpen, false);
});

Deno.test("initNav: returns close function", () => {
    const hamburger = { addEventListener() {}, setAttribute() {} } as unknown as HTMLElement;
    const drawer = makeDrawer(true);
    const overlay = { addEventListener() {} } as unknown as HTMLElement;

    const close = initNav(hamburger, drawer, overlay, mockDoc);
    close();

    assertEquals((drawer as unknown as { _isOpen: boolean })._isOpen, false);
});
