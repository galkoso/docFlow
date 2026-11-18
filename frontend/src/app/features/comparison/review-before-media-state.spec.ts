import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { SharedReviewBodyApi } from '../../core/api/shared-review-body-api';
import { IMAGE_DIGEST } from './image-state';
import { ReviewMediaState } from './review-media-state';
import { beforeMediaScope, beforeMediaResponse, displayHash } from './review-media.testing';

describe('Document-bound before-image lifecycle', () => {
  let state: ReviewMediaState, http: HttpTestingController;
  let profile: ReturnType<typeof signal<{ user_id: string } | null>>;
  let hash: ReturnType<typeof vi.fn<(blob: Blob) => Promise<string>>>;
  let create: ReturnType<typeof vi.spyOn>, revoke: ReturnType<typeof vi.spyOn>;
  const settle = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };
  beforeEach(() => {
    profile = signal<{ user_id: string } | null>({ user_id: beforeMediaScope().actor });
    hash = vi.fn<(blob: Blob) => Promise<string>>().mockResolvedValue(displayHash);
    create = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:before-doc'); revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ReviewMediaState,
      { provide: SessionState, useValue: { profile } }, { provide: IMAGE_DIGEST, useValue: hash }] });
    state = TestBed.inject(ReviewMediaState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.clear(); http.verify(); vi.restoreAllMocks(); });
  const request = () => http.expectOne(req => req.url.includes('/before-media/'));
  async function flush() { const response = beforeMediaResponse(); request().flush(response.body, { headers: response.headers }); await settle(); }
  it('loads only the captured before-document, waits for the PNG hash and binds decoded dimensions', async () => {
    const scope = beforeMediaScope(); state.open(scope); http.expectNone(() => true); state.load(); scope.beforeDocument!.file = 'changed';
    const pending = request(); expect(pending.request.url).toContain(`/files/${beforeMediaScope().beforeDocument!.file}/before-media/`);
    expect(pending.request.headers.get('X-Expected-Actor')).toBe(beforeMediaScope().actor); expect(pending.request.params.keys()).toEqual([]);
    let finish!: (value: string) => void; hash.mockImplementationOnce(() => new Promise<string>(resolve => finish = resolve));
    const response = beforeMediaResponse(); pending.flush(response.body, { headers: response.headers }); expect(create).not.toHaveBeenCalled();
    finish(displayHash); await settle(); expect(create).toHaveBeenCalledOnce(); expect(hash).toHaveBeenCalledOnce();
    expect(hash.mock.calls[0][0].type).toBe('image/png'); expect(state.loaded()).toBe(false);
    state.decoded('blob:before-doc', 1, 1); expect(state.loaded()).toBe(true);
  });
  it('rejects forged document hashes and wrong PNG hashes before creating pixels', async () => {
    state.open(beforeMediaScope()); state.load(); const response = beforeMediaResponse(), value = JSON.parse(atob(response.headers.get('X-Review-Before-Media')!));
    value.document_sha256 = 'f'.repeat(64); const bad = beforeMediaResponse(value); request().flush(bad.body, { headers: bad.headers }); await settle();
    expect(hash).not.toHaveBeenCalled(); expect(create).not.toHaveBeenCalled();
    hash.mockResolvedValue('9'.repeat(64)); state.load(); await flush(); expect(state.data()).toBeNull(); expect(create).not.toHaveBeenCalled();
  });
  it('does not publish an async digest after the live account changes', async () => {
    state.open(beforeMediaScope()); state.load(); let finish!: (value: string) => void;
    hash.mockImplementationOnce(() => new Promise<string>(resolve => finish = resolve));
    const response = beforeMediaResponse(); request().flush(response.body, { headers: response.headers });
    profile.set(null); finish(displayHash); await settle(); expect(create).not.toHaveBeenCalled(); expect(state.owns(beforeMediaScope())).toBe(false);
    state.load(); http.expectNone(() => true);
  });
  it('cancels old document requests and revokes old pixels when document evidence changes', async () => {
    const scope = beforeMediaScope(); state.open(scope); state.load(); const pending = request();
    const next = { ...scope, beforeDocument: { ...scope.beforeDocument!, sha256: 'f'.repeat(64) } };
    state.open(next); expect(pending.cancelled).toBe(true); state.open(scope); state.load(); await flush();
    expect(state.data()?.url).toBe('blob:before-doc'); state.open(next); expect(revoke).toHaveBeenCalledWith('blob:before-doc'); expect(state.data()).toBeNull();
  });
  it('keeps 422 reference/source fallbacks local and requires explicit retry', async () => {
    state.open(beforeMediaScope()); state.load();
    request().flush(new Blob([JSON.stringify({ error: { code: 'BEFORE_MEDIA_REFERENCE_NOT_FOUND', message_he: 'התמונה אינה בהפניות לפני' } })]),
      { status: 422, statusText: 'Unprocessable' }); await settle();
    expect(state.error()).toBe('התמונה אינה בהפניות לפני'); expect(TestBed.inject(SharedReviewBodyApi).denied()).toBe(false);
    http.expectNone(() => true); state.load(); const pending = request(); state.ngOnDestroy(); expect(pending.cancelled).toBe(true);
  });
  it('latches scoped denial and never falls back to the after-image endpoint', async () => {
    state.open(beforeMediaScope()); state.load(); request().flush(new Blob(), { status: 404, statusText: 'Denied' }); await settle();
    expect(TestBed.inject(SharedReviewBodyApi).denied()).toBe(true); expect(state.data()).toBeNull(); state.load(); http.expectNone(() => true);
  });
});
