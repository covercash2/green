import { test } from 'node:test';
import assert from 'node:assert/strict';
import { extractMessage, formatAppLine, isNearBottom } from '../../src/js/logs.ts';

// ── formatAppLine ─────────────────────────────────────────────────────────────

test('formatAppLine: parses valid NDJSON entry', () => {
    const line = JSON.stringify({
        timestamp: '2026-03-30T10:00:00Z',
        level: 'INFO',
        fields: { message: 'server started' },
        target: 'green',
    });
    const { text, level } = formatAppLine(line);
    assert.equal(level, 'info');
    assert.ok(text.includes('10:00:00'), 'text should include time');
    assert.ok(text.includes('INFO'), 'text should include level');
    assert.ok(text.includes('[green]'), 'text should include target');
    assert.ok(text.includes('server started'), 'text should include message');
});

test('formatAppLine: falls back to raw for non-JSON', () => {
    const line = 'Compiling green v0.1.0 (...)';
    const { text, level } = formatAppLine(line);
    assert.equal(text, line);
    assert.equal(level, 'raw');
});

test('formatAppLine: handles missing fields gracefully', () => {
    const line = JSON.stringify({ level: 'WARN' });
    const { text, level } = formatAppLine(line);
    assert.equal(level, 'warn');
    assert.ok(text.includes('WARN'));
});

test('formatAppLine: handles WARN level', () => {
    const line = JSON.stringify({ level: 'WARN', fields: { message: 'disk low' } });
    const { level } = formatAppLine(line);
    assert.equal(level, 'warn');
});

test('formatAppLine: handles ERROR level', () => {
    const line = JSON.stringify({ level: 'ERROR', fields: { message: 'crash' } });
    const { level } = formatAppLine(line);
    assert.equal(level, 'error');
});

test('formatAppLine: handles DEBUG level', () => {
    const line = JSON.stringify({ level: 'DEBUG', fields: { message: 'trace' } });
    const { level } = formatAppLine(line);
    assert.equal(level, 'debug');
});

test('formatAppLine: empty string is raw', () => {
    const { text, level } = formatAppLine('');
    assert.equal(level, 'raw');
    assert.equal(text, '');
});

// ── extractMessage ────────────────────────────────────────────────────────────

test('extractMessage: prefers message field', () => {
    assert.equal(extractMessage({ message: 'hello', summary: 'ignored' }), 'hello');
});

test('extractMessage: falls back to summary when message absent', () => {
    assert.equal(extractMessage({ summary: 'SELECT users' }), 'SELECT users');
});

test('extractMessage: appends extra fields after message', () => {
    const result = extractMessage({ message: 'eventloop error', err: 'connection reset' });
    assert.ok(result.startsWith('eventloop error'), 'message comes first');
    assert.ok(result.includes('err='), 'err field appended');
    assert.ok(result.includes('connection reset'), 'err value shown');
});

test('extractMessage: appends extra fields after summary', () => {
    const result = extractMessage({ summary: 'SELECT users', rows: 5 });
    assert.ok(result.startsWith('SELECT users'));
    assert.ok(result.includes('rows='));
});

test('extractMessage: falls back to key=value pairs when no message or summary', () => {
    const result = extractMessage({ rows_affected: 1, table: 'mqtt_devices' });
    assert.ok(result.includes('rows_affected'));
    assert.ok(result.includes('table'));
});

test('extractMessage: does not duplicate message in extra fields', () => {
    const result = extractMessage({ message: 'hello', other: 'world' });
    const count = result.split('hello').length - 1;
    assert.equal(count, 1, 'message text appears exactly once');
});

test('extractMessage: empty fields returns empty string', () => {
    assert.equal(extractMessage({}), '');
});

test('extractMessage: skips null/empty values', () => {
    const result = extractMessage({ message: 'hi', key: null, other: '' });
    assert.equal(result, 'hi');
});

// ── isNearBottom ──────────────────────────────────────────────────────────────

test('isNearBottom: true when at bottom', () => {
    assert.ok(isNearBottom({ scrollHeight: 1000, scrollTop: 900, clientHeight: 100 }));
});

test('isNearBottom: true when within 100px of bottom', () => {
    assert.ok(isNearBottom({ scrollHeight: 1000, scrollTop: 801, clientHeight: 100 }));
});

test('isNearBottom: false when far from bottom', () => {
    assert.ok(!isNearBottom({ scrollHeight: 1000, scrollTop: 0, clientHeight: 100 }));
});

test('isNearBottom: false when exactly 100px away', () => {
    assert.ok(!isNearBottom({ scrollHeight: 1000, scrollTop: 800, clientHeight: 100 }));
});
