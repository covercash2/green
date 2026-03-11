/**
 * Passkey recovery flow via one-time code sent through ntfy.
 *
 * `startRecovery` and `verifyRecovery` are pure logic functions — all
 * side-effectful dependencies (fetch) are injected so they can be
 * unit-tested without a browser or network.
 *
 * The DOM binding at the bottom wires them to the actual page elements
 * and only runs in a browser context.
 */

/**
 * @param {string} username
 * @param {{ fetch?: Function }} deps
 * @returns {Promise<{ ok: boolean }>}
 * @throws {Error} on any failure
 */
export async function startRecovery(username, { fetch: fetchFn = fetch } = {}) {
    username = username.trim();
    if (!username) {
        throw new Error('enter your username');
    }

    const res = await fetchFn('/auth/recover', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username }),
    });
    if (!res.ok) {
        const msg = await res.text();
        throw new Error(msg || 'failed to send code');
    }
    return await res.json();
}

/**
 * @param {string} username
 * @param {string} code
 * @param {{ fetch?: Function }} deps
 * @returns {Promise<string>} redirect URL on success
 * @throws {Error} on any failure
 */
export async function verifyRecovery(username, code, { fetch: fetchFn = fetch } = {}) {
    code = code.trim().toUpperCase();
    if (!code) {
        throw new Error('enter the recovery code');
    }

    const res = await fetchFn('/auth/recover/verify', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, code }),
    });
    if (!res.ok) {
        const msg = await res.text();
        throw new Error(msg || 'invalid or expired code');
    }
    return '/';
}

// DOM binding — only runs in the browser
if (typeof document !== 'undefined') {
    const step1 = document.getElementById('step-1');
    const step2 = document.getElementById('step-2');
    const sendBtn = document.getElementById('send-code-btn');
    const verifyBtn = document.getElementById('verify-btn');

    if (sendBtn && step1 && step2) {
        const usernameInput = document.getElementById('username');
        const sendErr = document.getElementById('send-error');

        sendBtn.addEventListener('click', async () => {
            sendErr.style.display = 'none';
            try {
                await startRecovery(usernameInput.value, { fetch });
                step1.style.display = 'none';
                step2.style.display = '';
                document.getElementById('code')?.focus();
            } catch (err) {
                sendErr.textContent = err.message || String(err);
                sendErr.style.display = '';
            }
        });
    }

    if (verifyBtn) {
        const usernameInput = document.getElementById('username');
        const codeInput = document.getElementById('code');
        const verifyErr = document.getElementById('verify-error');

        verifyBtn.addEventListener('click', async () => {
            verifyErr.style.display = 'none';
            try {
                const redirect = await verifyRecovery(usernameInput.value.trim(), codeInput.value, { fetch });
                window.location.href = redirect;
            } catch (err) {
                verifyErr.textContent = err.message || String(err);
                verifyErr.style.display = '';
            }
        });
    }
}
