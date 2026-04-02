// Site-wide hamburger nav drawer.

export function initNav(
    hamburger: HTMLElement,
    drawer: HTMLElement,
    overlay: HTMLElement,
    doc: Pick<Document, "addEventListener"> = document,
): () => void {
    function open(): void {
        drawer.classList.add("is-open");
        hamburger.setAttribute("aria-expanded", "true");
        drawer.setAttribute("aria-hidden", "false");
    }

    function close(): void {
        drawer.classList.remove("is-open");
        hamburger.setAttribute("aria-expanded", "false");
        drawer.setAttribute("aria-hidden", "true");
    }

    function toggle(): void {
        if (drawer.classList.contains("is-open")) close(); else open();
    }

    function onKey(e: KeyboardEvent): void {
        if (e.key === "Escape") close();
    }

    hamburger.addEventListener("click", toggle);
    overlay.addEventListener("click", close);
    doc.addEventListener("keydown", onKey);

    return close;
}

if (typeof document !== "undefined") {
    const hamburger = document.getElementById("nav-hamburger");
    const drawer = document.getElementById("nav-drawer");
    const overlay = document.getElementById("nav-overlay");
    if (hamburger && drawer && overlay) initNav(hamburger, drawer, overlay);
}
