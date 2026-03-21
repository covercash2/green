/**
 * WebAuthn registration flow for the register page.
 *
 * `register` is a pure logic function — all side-effectful dependencies
 * (fetch, startRegistration) are injected so it can be unit-tested without
 * a browser or network.
 *
 * The DOM binding at the bottom wires it up to the actual page elements and
 * only runs in a browser context.
 */

import * as v from '@valibot/valibot';

type StartRegistration = (opts: { optionsJSON: unknown }) => Promise<unknown>;

declare const SimpleWebAuthnBrowser: {
    startRegistration: StartRegistration;
};

const ChallengeSchema = v.object({
    publicKey: v.unknown(),
});

export async function register(
    username: string,
    {
        startRegistration,
        fetch: fetchFn = globalThis.fetch,
    }: {
        startRegistration: StartRegistration;
        fetch?: typeof globalThis.fetch;
    },
): Promise<string> {
    username = username.trim();
    if (!username) {
        throw new Error('enter your username');
    }

    const challengeRes = await fetchFn('/auth/register/challenge', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username }),
    });
    if (!challengeRes.ok) {
        const msg = await challengeRes.text();
        throw new Error(msg || 'challenge failed');
    }
    // webauthn-rs wraps options in { publicKey: ... }; simplewebauthn wants the inner object
    const { publicKey } = v.parse(ChallengeSchema, await challengeRes.json());

    const credential = await startRegistration({ optionsJSON: publicKey });

    const finishRes = await fetchFn('/auth/register/finish', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username, credential }),
    });
    if (!finishRes.ok) {
        const msg = await finishRes.text();
        throw new Error(msg || 'registration failed');
    }

    return '/';
}

// DOM binding — only runs in the browser
if (typeof document !== 'undefined') {
    const btn = document.getElementById('reg-btn');
    if (btn) {
        const usernameInput = document.getElementById('username') as HTMLInputElement | null;
        const errEl = document.getElementById('reg-error')!;

        btn.addEventListener('click', async () => {
            const username = usernameInput?.value.trim() ?? '';
            errEl.style.display = 'none';

            try {
                const redirect = await register(username, {
                    startRegistration: SimpleWebAuthnBrowser.startRegistration,
                });
                window.location.href = redirect;
            } catch (err) {
                errEl.textContent = err instanceof Error ? err.message : String(err);
                errEl.style.display = '';
            }
        });
    }
}
