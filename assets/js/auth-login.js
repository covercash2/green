/**
 * WebAuthn authentication flow for the login page.
 *
 * `authenticateDiscoverable` is a pure exported function with injected deps
 * so it can be unit-tested without a browser or network.
 *
 * The DOM binding at the bottom wires it up to the actual page elements and
 * only runs in a browser context.
 */

/**
 * Sign in using a discoverable credential — no username required.
 * Shows the browser's modal passkey picker immediately when called.
 * Use this for button-click flows where the user hasn't typed a username.
 *
 * @param {{ startAuthentication: Function, fetch?: Function, next?: string }} deps
 * @returns {Promise<string>} redirect URL on success
 * @throws {Error} on any failure
 */
export async function authenticateDiscoverable({ startAuthentication, fetch: fetchFn = fetch, next = '/' } = {}) {
    const challengeRes = await fetchFn('/auth/login/challenge/discoverable', { method: 'POST' });
    if (!challengeRes.ok) {
        const msg = await challengeRes.text();
        throw new Error(msg || 'challenge failed');
    }
    const { publicKey, challenge_id: challengeId } = await challengeRes.json();

    const credential = await startAuthentication({ optionsJSON: publicKey });

    const finishRes = await fetchFn('/auth/login/finish/discoverable', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ challenge_id: challengeId, credential }),
    });
    if (!finishRes.ok) {
        const msg = await finishRes.text();
        throw new Error(msg || 'authentication failed');
    }

    return next;
}

// DOM binding — only runs in the browser
if (typeof document !== 'undefined') {
    const btn = document.getElementById('auth-btn');
    if (btn) {
        const form = document.getElementById('auth-form');
        const errEl = document.getElementById('auth-error');
        const next = form?.dataset.next || '/';

        btn.addEventListener('click', async () => {
            errEl.style.display = 'none';
            try {
                const redirect = await authenticateDiscoverable({
                    startAuthentication: SimpleWebAuthnBrowser.startAuthentication,
                    next,
                });
                window.location.href = redirect;
            } catch (err) {
                errEl.textContent = err.message || String(err);
                errEl.style.display = '';
            }
        });
    }
}
