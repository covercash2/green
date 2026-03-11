/**
 * WebAuthn authentication flow for the login page.
 *
 * `authenticate` is a pure logic function — all side-effectful dependencies
 * (fetch, startAuthentication) are injected so it can be unit-tested without
 * a browser or network.
 *
 * The DOM binding at the bottom wires it up to the actual page elements and
 * only runs in a browser context.
 */

/**
 * @param {string} username
 * @param {{ startAuthentication: Function, fetch?: Function }} deps
 * @returns {Promise<string>} redirect URL on success
 * @throws {Error} on any failure
 */
export async function authenticate(username, { startAuthentication, fetch: fetchFn = fetch } = {}) {
    username = username.trim();
    if (!username) {
        throw new Error('enter your username');
    }

    const challengeRes = await fetchFn('/auth/login/challenge', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username }),
    });
    if (!challengeRes.ok) {
        const msg = await challengeRes.text();
        throw new Error(msg || 'challenge failed');
    }
    const options = await challengeRes.json();

    // webauthn-rs wraps options in { publicKey: ... }; simplewebauthn wants the inner object
    const credential = await startAuthentication({ optionsJSON: options.publicKey });

    const finishRes = await fetchFn('/auth/login/finish', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, credential }),
    });
    if (!finishRes.ok) {
        const msg = await finishRes.text();
        throw new Error(msg || 'authentication failed');
    }

    return '/';
}

// DOM binding — only runs in the browser
if (typeof document !== 'undefined') {
    const btn = document.getElementById('auth-btn');
    if (btn) {
        const usernameInput = document.getElementById('username');
        const errEl = document.getElementById('auth-error');

        btn.addEventListener('click', async () => {
            const username = usernameInput.value.trim();
            errEl.style.display = 'none';

            try {
                const redirect = await authenticate(username, {
                    startAuthentication: SimpleWebAuthnBrowser.startAuthentication,
                });
                window.location.href = redirect;
            } catch (err) {
                errEl.textContent = err.message || String(err);
                errEl.style.display = '';
            }
        });
    }
}
