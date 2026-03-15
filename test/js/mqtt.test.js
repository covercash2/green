import { test } from 'node:test';
import assert from 'node:assert/strict';
import { connectMqtt } from '../../assets/js/mqtt.js';

// ── mock EventSource ──────────────────────────────────────────────────────────

// Returns an object with:
//   ES       — constructor to pass as the EventSource dep
//   instance — getter that returns the most recently constructed instance
//              (populated when connectMqtt calls `new ES(url)`)
function makeMockES() {
    let _instance;
    class MockEventSource {
        constructor(url) {
            this.url = url;
            this.closed = false;
            this.onopen = null;
            this.onerror = null;
            this.onmessage = null;
            _instance = this;
        }
        close() { this.closed = true; }
        // Helpers for triggering events in tests.
        triggerOpen()        { this.onopen?.({ type: 'open' }); }
        triggerError()       { this.onerror?.({ type: 'error' }); }
        triggerMessage(data) { this.onmessage?.({ data: JSON.stringify(data) }); }
        triggerRaw(data)     { this.onmessage?.({ data }); }
    }
    // Use a getter so `mock.instance` always returns the value set during construction,
    // even though the instance doesn't exist until connectMqtt calls `new ES(url)`.
    return { ES: MockEventSource, get instance() { return _instance; } };
}

// ── tests ─────────────────────────────────────────────────────────────────────

test('creates EventSource with the given URL', () => {
    const mock = makeMockES();
    connectMqtt('/api/mqtt/stream', { EventSource: mock.ES });
    assert.equal(mock.instance.url, '/api/mqtt/stream');
});

test('calls onStatus("connected") when EventSource opens', () => {
    const mock = makeMockES();
    const statuses = [];
    connectMqtt('/api/mqtt/stream', { EventSource: mock.ES, onStatus: (s) => statuses.push(s) });
    mock.instance.triggerOpen();
    assert.deepEqual(statuses, ['connected']);
});

test('calls onStatus("error") when EventSource errors', () => {
    const mock = makeMockES();
    const statuses = [];
    connectMqtt('/api/mqtt/stream', { EventSource: mock.ES, onStatus: (s) => statuses.push(s) });
    mock.instance.triggerError();
    assert.deepEqual(statuses, ['error']);
});

test('calls onMessage with parsed message on valid JSON frame', () => {
    const mock = makeMockES();
    const messages = [];
    connectMqtt('/api/mqtt/stream', { EventSource: mock.ES, onMessage: (m) => messages.push(m) });
    mock.instance.triggerMessage({ topic: 'home/temp', payload: '21.5', received_at: '2026-03-15T12:00:00Z' });
    assert.equal(messages.length, 1);
    assert.equal(messages[0].topic, 'home/temp');
    assert.equal(messages[0].payload, '21.5');
    assert.equal(messages[0].received_at, '2026-03-15T12:00:00Z');
});

test('silently ignores malformed JSON frames', () => {
    const mock = makeMockES();
    const messages = [];
    connectMqtt('/api/mqtt/stream', { EventSource: mock.ES, onMessage: (m) => messages.push(m) });
    mock.instance.triggerRaw('not valid json {{{');
    assert.equal(messages.length, 0);
});

test('delivers multiple messages in order', () => {
    const mock = makeMockES();
    const messages = [];
    connectMqtt('/api/mqtt/stream', { EventSource: mock.ES, onMessage: (m) => messages.push(m) });
    mock.instance.triggerMessage({ topic: 'a', payload: '1', received_at: 't1' });
    mock.instance.triggerMessage({ topic: 'b', payload: '2', received_at: 't2' });
    mock.instance.triggerMessage({ topic: 'c', payload: '3', received_at: 't3' });
    assert.equal(messages.length, 3);
    assert.equal(messages[0].topic, 'a');
    assert.equal(messages[2].topic, 'c');
});

test('close() closes the underlying EventSource', () => {
    const mock = makeMockES();
    const { close } = connectMqtt('/api/mqtt/stream', { EventSource: mock.ES });
    assert.equal(mock.instance.closed, false);
    close();
    assert.equal(mock.instance.closed, true);
});
