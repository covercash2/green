import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fetchDeviceMessages, sendCommand, handleDeviceRowClick, PanelController } from '../../src/js/mqtt-devices.ts';

// Helpers
function ok(text: string) {
    return { ok: true, text: async () => text, status: 200 };
}

function fail(status: number) {
    return { ok: false, status, text: async () => 'error' };
}

/** Build a controllable PanelController mock.
 *  `externalClose()` simulates another row's click calling closeAll during a fetch. */
function makeCtrl(initiallyOpen: boolean): PanelController & {
    closed: boolean;
    opened: boolean;
    inserted: string | null;
    externalClose(): void;
} {
    let open = initiallyOpen;
    return {
        get wasOpen() { return initiallyOpen; },
        closed: false,
        opened: false,
        inserted: null as string | null,
        closeAll() { this.closed = true; open = false; },
        markOpen() { this.opened = true; open = true; },
        isStillOpen() { return open; },
        insertPanel(html: string) { this.inserted = html; },
        externalClose() { open = false; },
    };
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

// ── handleDeviceRowClick ──────────────────────────────────────────────────────

test('click on closed row calls closeAll, markOpen, then insertPanel', async () => {
    const ctrl = makeCtrl(false);
    await handleDeviceRowClick('z', 'd', ctrl, {
        fetch: async () => ok('<p>messages</p>'),
    });
    assert.ok(ctrl.closed, 'closeAll called');
    assert.ok(ctrl.opened, 'markOpen called');
    assert.equal(ctrl.inserted, '<p>messages</p>');
});

test('click on open row calls closeAll and returns without opening', async () => {
    const ctrl = makeCtrl(true);
    let fetchCalled = false;
    await handleDeviceRowClick('z', 'd', ctrl, {
        fetch: async () => { fetchCalled = true; return ok(''); },
    });
    assert.ok(ctrl.closed, 'closeAll called');
    assert.ok(!ctrl.opened, 'markOpen NOT called');
    assert.ok(!fetchCalled, 'fetch NOT called');
    assert.equal(ctrl.inserted, null);
});

test('second click during fetch (race condition): panel is not inserted', async () => {
    const ctrl = makeCtrl(false);
    await handleDeviceRowClick('z', 'd', ctrl, {
        fetch: async () => {
            // Simulate a second click calling closeAll while the fetch is in flight.
            ctrl.externalClose();
            return ok('<p>stale</p>');
        },
    });
    assert.equal(ctrl.inserted, null, 'stale panel must not be inserted');
});

test('switching rows during fetch: panel not inserted after closeAll', async () => {
    const ctrl = makeCtrl(false);
    let fetchResolveFn!: () => void;
    const fetchStarted = new Promise<void>(resolve => { fetchResolveFn = resolve; });

    const clickPromise = handleDeviceRowClick('z', 'd', ctrl, {
        fetch: async () => {
            fetchResolveFn();
            // Another row is clicked; that handler calls closeAll on all rows.
            ctrl.externalClose();
            return ok('<p>row A html</p>');
        },
    });

    await fetchStarted;
    await clickPromise;

    assert.equal(ctrl.inserted, null, 'panel must not be inserted after external close');
});

test('fetch error inserts error message when row is still open', async () => {
    const ctrl = makeCtrl(false);
    await handleDeviceRowClick('z', 'd', ctrl, {
        fetch: async () => { throw new Error('timeout'); },
    });
    assert.ok(ctrl.inserted?.includes('timeout'), 'error message inserted');
});

test('fetch error does not insert when row was closed during fetch', async () => {
    const ctrl = makeCtrl(false);
    await handleDeviceRowClick('z', 'd', ctrl, {
        fetch: async () => {
            ctrl.externalClose();
            throw new Error('timeout');
        },
    });
    assert.equal(ctrl.inserted, null, 'no insert after external close + error');
});
