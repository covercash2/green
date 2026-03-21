import { test } from 'node:test';
import assert from 'node:assert/strict';
import { applyFilter, getPage, totalPages, renderControls } from '../../src/js/mqtt.ts';

// Helpers
function cards(...topics) {
    return topics.map(t => ({ dataset: { topic: t } }));
}

// ── applyFilter ──────────────────────────────────────────────────────────────

test('applyFilter: empty filterText returns all cards', () => {
    const cs = cards('home/temp', 'home/humidity');
    assert.deepEqual(applyFilter(cs, ''), cs);
});

test('applyFilter: matches topic substring', () => {
    const cs = cards('home/temp', 'sensors/pressure', 'home/humidity');
    const result = applyFilter(cs, 'home');
    assert.equal(result.length, 2);
    assert.equal(result[0].dataset.topic, 'home/temp');
    assert.equal(result[1].dataset.topic, 'home/humidity');
});

test('applyFilter: case-insensitive match', () => {
    const cs = cards('Home/Temp', 'sensors/pressure');
    assert.equal(applyFilter(cs, 'home').length, 1);
    assert.equal(applyFilter(cs, 'HOME').length, 1);
});

test('applyFilter: no match returns empty array', () => {
    const cs = cards('home/temp', 'home/humidity');
    assert.equal(applyFilter(cs, 'xyz').length, 0);
});

// ── getPage ──────────────────────────────────────────────────────────────────

test('getPage: page 0 returns first pageSize items', () => {
    const cs = cards(...Array.from({ length: 25 }, (_, i) => `t/${i}`));
    const page = getPage(cs, 0, 20);
    assert.equal(page.length, 20);
    assert.equal(page[0].dataset.topic, 't/0');
    assert.equal(page[19].dataset.topic, 't/19');
});

test('getPage: page 1 returns next slice', () => {
    const cs = cards(...Array.from({ length: 25 }, (_, i) => `t/${i}`));
    const page = getPage(cs, 1, 20);
    assert.equal(page.length, 5);
    assert.equal(page[0].dataset.topic, 't/20');
});

test('getPage: last page with remainder', () => {
    const cs = cards(...Array.from({ length: 23 }, (_, i) => `t/${i}`));
    const page = getPage(cs, 1, 20);
    assert.equal(page.length, 3);
});

test('getPage: empty array returns empty', () => {
    assert.equal(getPage([], 0, 20).length, 0);
});

test('getPage: page beyond end returns empty', () => {
    const cs = cards('a', 'b');
    assert.equal(getPage(cs, 5, 20).length, 0);
});

// ── totalPages ───────────────────────────────────────────────────────────────

test('totalPages: 0 cards returns 1', () => {
    assert.equal(totalPages([], 20), 1);
});

test('totalPages: exact multiple', () => {
    const cs = cards(...Array.from({ length: 40 }, (_, i) => `t/${i}`));
    assert.equal(totalPages(cs, 20), 2);
});

test('totalPages: with remainder rounds up', () => {
    const cs = cards(...Array.from({ length: 21 }, (_, i) => `t/${i}`));
    assert.equal(totalPages(cs, 20), 2);
});

test('totalPages: fewer than pageSize returns 1', () => {
    const cs = cards('a', 'b', 'c');
    assert.equal(totalPages(cs, 20), 1);
});

// ── renderControls ───────────────────────────────────────────────────────────

test('renderControls: prev button disabled on page 0', () => {
    const html = renderControls(0, 3, 0);
    // The prev button should have disabled attr before the first page button
    const prevIdx  = html.indexOf('[ ← ]');
    const disabledIdx = html.indexOf('disabled');
    assert.ok(disabledIdx < prevIdx || html.indexOf('disabled') !== -1);
    // More specifically: the button containing ← should have disabled
    const prevBtn = html.match(/<button[^>]*>\[ ← \]<\/button>/);
    assert.ok(prevBtn, 'prev button found');
    assert.ok(prevBtn[0].includes('disabled'), 'prev button is disabled on page 0');
});

test('renderControls: next button disabled on last page', () => {
    const html = renderControls(2, 3, 0);
    const nextBtn = html.match(/<button[^>]*>\[ → \]<\/button>/);
    assert.ok(nextBtn, 'next button found');
    assert.ok(nextBtn[0].includes('disabled'), 'next button is disabled on last page');
});

test('renderControls: prev enabled and next enabled on middle page', () => {
    const html = renderControls(1, 3, 0);
    const prevBtn = html.match(/<button[^>]*>\[ ← \]<\/button>/);
    const nextBtn = html.match(/<button[^>]*>\[ → \]<\/button>/);
    assert.ok(!prevBtn[0].includes('disabled'), 'prev not disabled on middle page');
    assert.ok(!nextBtn[0].includes('disabled'), 'next not disabled on middle page');
});

test('renderControls: active class on current page button', () => {
    const html = renderControls(1, 3, 0);
    assert.ok(html.includes('mqtt-page-active'), 'active class present');
    // page 2 button (data-page="1") should have active class
    assert.ok(html.includes('mqtt-page-active" data-page="1"'), 'active on correct page');
});

test('renderControls: badge shown on page-1 button when newCount > 0 and currentPage > 0', () => {
    const html = renderControls(2, 5, 3);
    assert.ok(html.includes('mqtt-page-new'), 'badge span present');
    assert.ok(html.includes('(+3)'), 'badge count correct');
    // Badge should be inside the page-1 button (data-page="0")
    const page1BtnMatch = html.match(/<button[^>]*data-page="0"[^>]*>.*?<\/button>/s);
    assert.ok(page1BtnMatch, 'page-1 button found');
    assert.ok(page1BtnMatch[0].includes('mqtt-page-new'), 'badge on page-1 button');
});

test('renderControls: no badge when on page 0', () => {
    const html = renderControls(0, 5, 3);
    assert.ok(!html.includes('mqtt-page-new'), 'no badge when on first page');
});

test('renderControls: no badge when newCount is 0', () => {
    const html = renderControls(2, 5, 0);
    assert.ok(!html.includes('mqtt-page-new'), 'no badge when newCount is 0');
});

test('renderControls: single page has both prev and next disabled', () => {
    const html = renderControls(0, 1, 0);
    const buttons = [...html.matchAll(/<button[^>]*disabled[^>]*>/g)];
    assert.equal(buttons.length, 2, 'both nav buttons disabled for single page');
});
