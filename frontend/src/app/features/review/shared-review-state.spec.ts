import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { actor, proposal, snapshot, other, history, metadata, files, media } from '../../../testing/review-fixtures';
import { SharedReviewState } from './shared-review-state';
import { validHistory } from './snapshot-evidence';
import { reviewLocation } from './review-location';
import { convertToParamMap } from '@angular/router';

describe('Shared review snapshot isolation', () => {
  let state: SharedReviewState, http: HttpTestingController;
  const root = `/api/v1/proposals/${proposal}/review/snapshots`, selected = `${root}/${snapshot}`;
  beforeEach(() => { TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), SharedReviewState] });
    state = TestBed.inject(SharedReviewState); http = TestBed.inject(HttpTestingController); });
  afterEach(() => { state.clear(); http.verify(); });
  function open() { state.open(actor, proposal, { snapshot, files: 0, media: 0 }); http.expectOne(req => req.url === root).flush(history()); }
  it('loads immutable history and the complete metadata/file/media page as one scoped result without Git work', () => {
    open(); expect(state.snapshot()).toBeNull();
    const requests = [http.expectOne(selected), http.expectOne(req => req.url === `${selected}/files`), http.expectOne(req => req.url === `${selected}/media`)];
    for (const request of requests) { expect(request.request.method).toBe('GET'); expect(request.request.headers.get('X-Expected-Actor')).toBe(actor); }
    requests[0].flush(metadata()); requests[1].flush(files()); expect(state.snapshot()).toBeNull();
    requests[2].flush(media()); expect(state.phase()).toBe('ready'); expect(state.snapshot()?.snapshot_id).toBe(snapshot);
  });
  it.each(['scope', 'count', 'media', 'history', 'version'])('rejects mixed %s evidence instead of displaying a partial review', fault => {
    open(); const value = metadata(), page = files(), receipts = media();
    if (fault === 'scope') page.snapshot_id = other;
    if (fault === 'count') page.total = 2;
    if (fault === 'media') receipts.total = 1;
    if (fault === 'history') value.title = 'Different immutable title';
    if (fault === 'version') value.metadata.summary.configuration_version = 7;
    http.expectOne(selected).flush(value); http.expectOne(req => req.url === `${selected}/files`).flush(page);
    http.expectOne(req => req.url === `${selected}/media`).flush(receipts);
    expect(state.phase()).toBe('error'); expect(state.snapshot()).toBeNull(); expect(state.files()).toBeNull(); expect(state.media()).toBeNull();
  });
  it('cancels stale snapshot reads and purges data after permission loss', () => {
    open(); const old = http.expectOne(selected), oldFiles = http.expectOne(req => req.url === `${selected}/files`), oldMedia = http.expectOne(req => req.url === `${selected}/media`);
    state.open(other, proposal, { snapshot, files: 0, media: 0 }); expect(old.cancelled && oldFiles.cancelled && oldMedia.cancelled).toBe(true);
    const fresh = http.expectOne(req => req.url === root); expect(fresh.request.headers.get('X-Expected-Actor')).toBe(other);
    fresh.flush({ error: { code: 'NOT_FOUND' } }, { status: 404, statusText: 'Not Found' });
    expect(state.history()).toBeNull(); expect(state.snapshot()).toBeNull(); expect(state.phase()).toBe('error');
  });
  it('keeps empty history explicit and never creates a comparison or submission', () => {
    state.open(actor, proposal, { files: 0, media: 0 });
    http.expectOne(req => req.url === root).flush({ ...history(), active_snapshot_id: null, snapshots: [] });
    expect(state.phase()).toBe('ready'); expect(state.snapshot()).toBeNull(); http.expectNone(req => req.method !== 'GET');
  });
  it('validates stable UTC microsecond ordering instead of collapsing timestamps to milliseconds', () => {
    const value = history(), first = value.snapshots[0];
    first.created_at = '2026-11-18T00:00:00.000002+00:00'; first.snapshot_id = proposal;
    value.snapshots.push({ ...first, snapshot_id: snapshot, created_at: '2026-11-18T00:00:00.000001+00:00' });
    expect(validHistory(value, proposal)).toBe(true);
    value.snapshots.reverse(); expect(validHistory(value, proposal)).toBe(false);
  });
  it.each(['unknown', 'duplicate', 'offset', 'unscoped', 'job'])('rejects ambiguous %s locations before requests', fault => {
    const query: Record<string, string | string[]> = { snapshot };
    if (fault === 'unknown') query['head'] = 'a'.repeat(40);
    if (fault === 'duplicate') query['snapshot'] = [snapshot, other];
    if (fault === 'offset') query['media'] = '20001';
    if (fault === 'unscoped') { delete query['snapshot']; query['files'] = '50'; }
    if (fault === 'job') query['job'] = 'not-a-uuid';
    expect(reviewLocation(convertToParamMap(query))).toBeNull();
  });
});
