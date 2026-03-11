import { test } from 'node:test';
import assert from 'node:assert/strict';
import { authenticate } from '../../assets/js/auth-login.js';

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

// ── tests ─────────────────────────────────────────────────────────────────────

test('rejects empty username', async () => {
    await assert.rejects(
        () => authenticate('', { startAuthentication: () => {} }),
        /enter your username/,
    );
});

test('rejects whitespace-only username', async () => {
    await assert.rejects(
        () => authenticate('   ', { startAuthentication: () => {} }),
        /enter your username/,
    );
});

test('happy path returns redirect URL', async () => {
    const calls = [];
    const mockFetch = async (url, opts) => {
        calls.push({ url, body: JSON.parse(opts.body) });
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'abc123', timeout: 60000 } });
        if (url.includes('finish')) return okEmpty();
    };
    const mockStartAuth = async ({ optionsJSON }) => {
        assert.deepEqual(optionsJSON, { challenge: 'abc123', timeout: 60000 });
        return { id: 'cred-id', type: 'public-key' };
    };

    const redirect = await authenticate('alice', {
        startAuthentication: mockStartAuth,
        fetch: mockFetch,
    });

    assert.equal(redirect, '/');
    assert.equal(calls.length, 2);
    assert.match(calls[0].url, /challenge/);
    assert.equal(calls[0].body.username, 'alice');
    assert.match(calls[1].url, /finish/);
    assert.equal(calls[1].body.username, 'alice');
    assert.deepEqual(calls[1].body.credential, { id: 'cred-id', type: 'public-key' });
});

test('throws on challenge HTTP error', async () => {
    const mockFetch = async () => err('unknown user');
    await assert.rejects(
        () => authenticate('alice', { startAuthentication: () => {}, fetch: mockFetch }),
        /unknown user/,
    );
});

test('throws with fallback message when challenge body is empty', async () => {
    const mockFetch = async () => err('');
    await assert.rejects(
        () => authenticate('alice', { startAuthentication: () => {}, fetch: mockFetch }),
        /challenge failed/,
    );
});

test('throws when startAuthentication rejects', async () => {
    const mockFetch = async (url) => {
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'abc' } });
        return okEmpty();
    };
    const mockStartAuth = async () => { throw new Error('user cancelled'); };
    await assert.rejects(
        () => authenticate('alice', { startAuthentication: mockStartAuth, fetch: mockFetch }),
        /user cancelled/,
    );
});

test('throws on finish HTTP error', async () => {
    const mockFetch = async (url) => {
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'abc' } });
        return err('server rejected credential');
    };
    const mockStartAuth = async () => ({ id: 'cred-id' });
    await assert.rejects(
        () => authenticate('alice', { startAuthentication: mockStartAuth, fetch: mockFetch }),
        /server rejected credential/,
    );
});

test('throws with fallback message when finish body is empty', async () => {
    const mockFetch = async (url) => {
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'abc' } });
        return err('');
    };
    const mockStartAuth = async () => ({ id: 'cred-id' });
    await assert.rejects(
        () => authenticate('alice', { startAuthentication: mockStartAuth, fetch: mockFetch }),
        /authentication failed/,
    );
});

test('posts to the correct endpoints', async () => {
    const urls = [];
    const mockFetch = async (url, opts) => {
        urls.push(url);
        if (url.includes('challenge')) return okJson({});
        return okEmpty();
    };
    await authenticate('bob', { startAuthentication: async () => ({}), fetch: mockFetch });
    assert.equal(urls[0], '/auth/login/challenge');
    assert.equal(urls[1], '/auth/login/finish');
});
