import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fetchDeviceMessages, sendCommand } from '../../src/js/mqtt-devices.ts';

// Helpers
function ok(text: string) {
    return { ok: true, text: async () => text, status: 200 };
}

function fail(status: number) {
    return { ok: false, status, text: async () => 'error' };
}

// ── fetchDeviceMessages ───────────────────────────────────────────────────────

test('fetchDeviceMessages builds the correct URL', async () => {
    let calledUrl = '';
    await fetchDeviceMessages('zigbee2mqtt', '0xABCD', {
        fetch: async (url) => { calledUrl = url as string; return ok(''); },
    });
    assert.equal(calledUrl, '/api/mqtt/device-messages?integration=zigbee2mqtt&device=0xABCD');
});

test('fetchDeviceMessages URL-encodes integration with spaces', async () => {
    let calledUrl = '';
    await fetchDeviceMessages('Home Assistant', 'my_dev', {
        fetch: async (url) => { calledUrl = url as string; return ok(''); },
    });
    assert.ok(calledUrl.includes('Home%20Assistant'), 'spaces encoded in integration');
});

test('fetchDeviceMessages URL-encodes device with slashes', async () => {
    let calledUrl = '';
    await fetchDeviceMessages('z', 'sensor/temp', {
        fetch: async (url) => { calledUrl = url as string; return ok(''); },
    });
    assert.ok(calledUrl.includes('sensor%2Ftemp'), 'slash encoded in device');
});

test('fetchDeviceMessages returns the response text', async () => {
    const html = await fetchDeviceMessages('z', 'd', {
        fetch: async () => ok('<p>panel html</p>'),
    });
    assert.equal(html, '<p>panel html</p>');
});

test('fetchDeviceMessages propagates fetch rejection', async () => {
    await assert.rejects(
        () => fetchDeviceMessages('z', 'd', { fetch: async () => { throw new Error('network'); } }),
        /network/,
    );
});

// ── sendCommand ───────────────────────────────────────────────────────────────

test('sendCommand POSTs JSON to /api/mqtt/publish', async () => {
    let capturedUrl = '';
    let capturedBody = '';
    await sendCommand('home/light/set', '{"state":"ON"}', {
        fetch: async (url, opts) => {
            capturedUrl = url as string;
            capturedBody = opts?.body as string;
            return { ok: true, text: async () => '', status: 204 };
        },
    });
    assert.equal(capturedUrl, '/api/mqtt/publish');
    const body = JSON.parse(capturedBody);
    assert.equal(body.topic, 'home/light/set');
    assert.equal(body.payload, '{"state":"ON"}');
});

test('sendCommand sets Content-Type header', async () => {
    let capturedHeaders: HeadersInit | undefined;
    await sendCommand('t', 'p', {
        fetch: async (_, opts) => {
            capturedHeaders = opts?.headers;
            return { ok: true, text: async () => '', status: 204 };
        },
    });
    assert.equal((capturedHeaders as Record<string, string>)['Content-Type'], 'application/json');
});

test('sendCommand returns ok:true on 204', async () => {
    const result = await sendCommand('t', 'p', {
        fetch: async () => ({ ok: true, text: async () => '', status: 204 }),
    });
    assert.equal(result.ok, true);
});

test('sendCommand returns ok:false with status on 403', async () => {
    const result = await sendCommand('t', 'p', {
        fetch: async () => fail(403),
    });
    assert.equal(result.ok, false);
    assert.equal(result.status, 403);
});

test('sendCommand returns ok:false with status on 500', async () => {
    const result = await sendCommand('t', 'p', {
        fetch: async () => fail(500),
    });
    assert.equal(result.ok, false);
    assert.equal(result.status, 500);
});

test('sendCommand propagates fetch rejection', async () => {
    await assert.rejects(
        () => sendCommand('t', 'p', { fetch: async () => { throw new Error('network error'); } }),
        /network error/,
    );
});
