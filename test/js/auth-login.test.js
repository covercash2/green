import { test } from 'node:test';
import assert from 'node:assert/strict';
import { authenticateDiscoverable } from '../../assets/js/auth-login.js';

// ── helpers ──────────────────────────────────────────────────────────────────

function okJson(body) {
    return { ok: true, json: async () => body, text: async () => JSON.stringify(body) };
}

function okEmpty() {
    return { ok: true, json: async () => ({}), text: async () => '' };
}

function err(msg, status = 400) {
    return { ok: false, status, text: async () => msg, json: async () => ({ error: msg }) };
}

// ── authenticateDiscoverable ──────────────────────────────────────────────────

test('authenticateDiscoverable posts to discoverable endpoints', async () => {
    const urls = [];
    const mockFetch = async (url) => {
        urls.push(url);
        if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'id-1' });
        return okEmpty();
    };
    const redirect = await authenticateDiscoverable({
        startAuthentication: async () => ({ id: 'cred' }),
        fetch: mockFetch,
    });
    assert.equal(redirect, '/');
    assert.equal(urls[0], '/auth/login/challenge/discoverable');
    assert.equal(urls[1], '/auth/login/finish/discoverable');
});

test('authenticateDiscoverable does NOT pass useBrowserAutofill', async () => {
    let capturedOpts;
    await authenticateDiscoverable({
        startAuthentication: async (opts) => { capturedOpts = opts; return {}; },
        fetch: async (url) => {
            if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'x' });
            return okEmpty();
        },
    });
    assert.equal(capturedOpts.useBrowserAutofill, undefined);
});

test('authenticateDiscoverable returns / by default', async () => {
    const redirect = await authenticateDiscoverable({
        startAuthentication: async () => ({}),
        fetch: async (url) => {
            if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'x' });
            return okEmpty();
        },
    });
    assert.equal(redirect, '/');
});

test('authenticateDiscoverable returns the provided next URL', async () => {
    const redirect = await authenticateDiscoverable({
        startAuthentication: async () => ({}),
        fetch: async (url) => {
            if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'x' });
            return okEmpty();
        },
        next: '/breaker',
    });
    assert.equal(redirect, '/breaker');
});

test('authenticateDiscoverable passes challengeId to finish endpoint', async () => {
    const bodies = [];
    await authenticateDiscoverable({
        startAuthentication: async () => ({ id: 'cred' }),
        fetch: async (url, opts) => {
            if (opts?.body) bodies.push(JSON.parse(opts.body));
            if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'uuid-123' });
            return okEmpty();
        },
    });
    assert.equal(bodies[0].challenge_id, 'uuid-123');
});

test('authenticateDiscoverable throws on challenge failure', async () => {
    await assert.rejects(
        () => authenticateDiscoverable({
            startAuthentication: async () => ({}),
            fetch: async () => err('server error'),
        }),
        /server error/,
    );
});

test('authenticateDiscoverable throws with fallback message when challenge body is empty', async () => {
    await assert.rejects(
        () => authenticateDiscoverable({
            startAuthentication: async () => ({}),
            fetch: async () => err(''),
        }),
        /challenge failed/,
    );
});

test('authenticateDiscoverable throws when startAuthentication rejects', async () => {
    await assert.rejects(
        () => authenticateDiscoverable({
            startAuthentication: async () => { throw new Error('user cancelled'); },
            fetch: async (url) => {
                if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'x' });
                return okEmpty();
            },
        }),
        /user cancelled/,
    );
});

test('authenticateDiscoverable throws on finish failure', async () => {
    await assert.rejects(
        () => authenticateDiscoverable({
            startAuthentication: async () => ({}),
            fetch: async (url) => {
                if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'x' });
                return err('auth rejected');
            },
        }),
        /auth rejected/,
    );
});

test('authenticateDiscoverable throws with fallback message when finish body is empty', async () => {
    await assert.rejects(
        () => authenticateDiscoverable({
            startAuthentication: async () => ({}),
            fetch: async (url) => {
                if (!url.includes('finish')) return okJson({ publicKey: {}, challenge_id: 'x' });
                return err('');
            },
        }),
        /authentication failed/,
    );
});
