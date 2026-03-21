import { test } from 'node:test';
import assert from 'node:assert/strict';
import { register } from '../../src/js/auth-register.ts';

// ── helpers ──────────────────────────────────────────────────────────────────

function okJson(body) {
    return { ok: true, json: async () => body, text: async () => JSON.stringify(body) };
}

function okEmpty() {
    return { ok: true, json: async () => ({}), text: async () => '' };
}

function err(msg) {
    return { ok: false, text: async () => msg };
}

// ── tests ─────────────────────────────────────────────────────────────────────

test('rejects empty username', async () => {
    await assert.rejects(
        () => register('', { startRegistration: () => {} }),
        /enter your username/,
    );
});

test('rejects whitespace-only username', async () => {
    await assert.rejects(
        () => register('   ', { startRegistration: () => {} }),
        /enter your username/,
    );
});

test('happy path returns redirect URL', async () => {
    const calls = [];
    const mockFetch = async (url, opts) => {
        calls.push({ url, body: JSON.parse(opts.body) });
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'xyz789', rp: { name: 'Green' } } });
        if (url.includes('finish')) return okEmpty();
    };
    const mockStartReg = async ({ optionsJSON }) => {
        assert.equal(optionsJSON.challenge, 'xyz789');
        return { id: 'new-cred-id', type: 'public-key' };
    };

    const redirect = await register('gm', {
        startRegistration: mockStartReg,
        fetch: mockFetch,
    });

    assert.equal(redirect, '/');
    assert.equal(calls.length, 2);
    assert.match(calls[0].url, /challenge/);
    assert.equal(calls[0].body.username, 'gm');
    assert.match(calls[1].url, /finish/);
    assert.equal(calls[1].body.username, 'gm');
    assert.deepEqual(calls[1].body.credential, { id: 'new-cred-id', type: 'public-key' });
});

test('throws on challenge HTTP error', async () => {
    const mockFetch = async () => err('username not in config');
    await assert.rejects(
        () => register('unknown', { startRegistration: () => {}, fetch: mockFetch }),
        /username not in config/,
    );
});

test('throws with fallback message when challenge body is empty', async () => {
    const mockFetch = async () => err('');
    await assert.rejects(
        () => register('gm', { startRegistration: () => {}, fetch: mockFetch }),
        /challenge failed/,
    );
});

test('throws when startRegistration rejects', async () => {
    const mockFetch = async (url) => {
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'abc' } });
        return okEmpty();
    };
    const mockStartReg = async () => { throw new Error('authenticator error'); };
    await assert.rejects(
        () => register('gm', { startRegistration: mockStartReg, fetch: mockFetch }),
        /authenticator error/,
    );
});

test('throws on finish HTTP error', async () => {
    const mockFetch = async (url) => {
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'abc' } });
        return err('invalid attestation');
    };
    const mockStartReg = async () => ({ id: 'cred' });
    await assert.rejects(
        () => register('gm', { startRegistration: mockStartReg, fetch: mockFetch }),
        /invalid attestation/,
    );
});

test('throws with fallback message when finish body is empty', async () => {
    const mockFetch = async (url) => {
        if (url.includes('challenge')) return okJson({ publicKey: { challenge: 'abc' } });
        return err('');
    };
    const mockStartReg = async () => ({ id: 'cred' });
    await assert.rejects(
        () => register('gm', { startRegistration: mockStartReg, fetch: mockFetch }),
        /registration failed/,
    );
});

test('posts to the correct endpoints', async () => {
    const urls = [];
    const mockFetch = async (url, _opts) => {
        urls.push(url);
        if (url.includes('challenge')) return okJson({ publicKey: {} });
        return okEmpty();
    };
    await register('alice', { startRegistration: async () => ({}), fetch: mockFetch });
    assert.equal(urls[0], '/auth/register/challenge');
    assert.equal(urls[1], '/auth/register/finish');
});
