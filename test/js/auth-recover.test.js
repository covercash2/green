import { test } from 'node:test';
import assert from 'node:assert/strict';
import { startRecovery, verifyRecovery } from '../../assets/js/auth-recover.js';

// ── helpers ──────────────────────────────────────────────────────────────────

function okJson(body) {
    return { ok: true, json: async () => body, text: async () => JSON.stringify(body) };
}

function err(msg, status = 400) {
    return { ok: false, status, text: async () => msg, json: async () => ({ error: msg }) };
}

// ── startRecovery tests ───────────────────────────────────────────────────────

test('startRecovery rejects empty username', async () => {
    await assert.rejects(
        () => startRecovery('', { fetch: () => {} }),
        /enter your username/,
    );
});

test('startRecovery rejects whitespace-only username', async () => {
    await assert.rejects(
        () => startRecovery('   ', { fetch: () => {} }),
        /enter your username/,
    );
});

test('startRecovery success returns ok', async () => {
    const mockFetch = async (url, opts) => {
        assert.equal(url, '/auth/recover');
        assert.equal(JSON.parse(opts.body).username, 'chrash');
        return okJson({ ok: true });
    };
    const result = await startRecovery('chrash', { fetch: mockFetch });
    assert.deepEqual(result, { ok: true });
});

test('startRecovery throws on fetch error', async () => {
    const mockFetch = async () => err('server error');
    await assert.rejects(
        () => startRecovery('chrash', { fetch: mockFetch }),
        /server error/,
    );
});

test('startRecovery throws with fallback message when body is empty', async () => {
    const mockFetch = async () => err('');
    await assert.rejects(
        () => startRecovery('chrash', { fetch: mockFetch }),
        /failed to send code/,
    );
});

// ── verifyRecovery tests ──────────────────────────────────────────────────────

test('verifyRecovery success returns redirect URL', async () => {
    const mockFetch = async (url, opts) => {
        assert.equal(url, '/auth/recover/verify');
        const body = JSON.parse(opts.body);
        assert.equal(body.username, 'chrash');
        assert.equal(body.code, 'A3K7QP');
        return okJson({});
    };
    const redirect = await verifyRecovery('chrash', 'A3K7QP', { fetch: mockFetch });
    assert.equal(redirect, '/');
});

test('verifyRecovery normalises code to uppercase', async () => {
    const mockFetch = async (url, opts) => {
        const body = JSON.parse(opts.body);
        assert.equal(body.code, 'A3K7QP');
        return okJson({});
    };
    await verifyRecovery('chrash', 'a3k7qp', { fetch: mockFetch });
});

test('verifyRecovery throws on bad code', async () => {
    const mockFetch = async () => err('invalid or expired recovery code');
    await assert.rejects(
        () => verifyRecovery('chrash', 'WRONG1', { fetch: mockFetch }),
        /invalid or expired recovery code/,
    );
});

test('verifyRecovery throws with fallback message when body is empty', async () => {
    const mockFetch = async () => err('');
    await assert.rejects(
        () => verifyRecovery('chrash', 'ABCDEF', { fetch: mockFetch }),
        /invalid or expired code/,
    );
});

test('verifyRecovery rejects empty code', async () => {
    await assert.rejects(
        () => verifyRecovery('chrash', '', { fetch: () => {} }),
        /enter the recovery code/,
    );
});

test('verifyRecovery rejects whitespace-only code', async () => {
    await assert.rejects(
        () => verifyRecovery('chrash', '   ', { fetch: () => {} }),
        /enter the recovery code/,
    );
});
