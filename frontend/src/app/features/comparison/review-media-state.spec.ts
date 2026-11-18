import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from '../../core/auth/session-state';
import { SharedReviewBodyApi } from '../../core/api/shared-review-body-api';
import { IMAGE_DIGEST } from './image-state';
import { ReviewMediaState } from './review-media-state';
import { mediaScope, mediaResponse, mediaId, displayHash } from './review-media.testing';

describe('Sealed referenced-image lifecycle', () => {
  let state: ReviewMediaState, http: HttpTestingController;
  let profile: ReturnType<typeof signal<{ user_id: string } | null>>;
  let hash: ReturnType<typeof vi.fn<(blob: Blob) => Promise<string>>>;
  let create: ReturnType<typeof vi.spyOn>, revoke: ReturnType<typeof vi.spyOn>;
  const settle = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };
  beforeEach(() => {
    profile = signal<{ user_id: string } | null>({ user_id: mediaScope().actor });
    hash = vi.fn<(blob: Blob) => Promise<string>>(async blob => blob.type === 'image/png' ? displayHash : mediaId);
    create = vi.spyOn(URL, 'createObjectURL').mockReturnValue('blob:sealed-media'); revoke = vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {});
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ReviewMediaState,
      { provide: SessionState, useValue: { profile } }, { provide: IMAGE_DIGEST, useValue: hash }] });
    state = TestBed.inject(ReviewMediaState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.clear(); http.verify(); vi.restoreAllMocks(); });
  function request() { return http.expectOne(req => req.url.includes('/media/') && req.url.endsWith('/image')); }
  async function flush() { const response = mediaResponse(); request().flush(response.body, { headers: response.headers }); await settle(); }
  it('waits for explicit/viewport loading, captures the actor and creates a URL only after domain and body hashes', async () => {
    const scope = mediaScope(); state.open(scope); http.expectNone(() => true); state.load(); scope.actor = 'changed';
    const pending = request(); expect(pending.request.headers.get('X-Expected-Actor')).toBe(mediaScope().actor);
    expect(pending.request.params.keys()).toEqual([]); expect(pending.request.url).toContain(`/media/${mediaScope().path.slice(7)}/image`);
    const response = mediaResponse(); let finish!: (value: string) => void;
    hash.mockImplementationOnce(() => new Promise<string>(resolve => finish = resolve));
    pending.flush(response.body, { headers: response.headers }); expect(create).not.toHaveBeenCalled();
    expect(await hash.mock.calls[0][0].text()).toBe(`specspace-review-media-v1\0${mediaScope().path}`);
    finish(mediaId); await settle(); expect(create).toHaveBeenCalledOnce(); expect(state.loaded()).toBe(false);
    state.decoded('blob:sealed-media', 1, 1); expect(state.loaded()).toBe(true);
  });
  it.each(['domain', 'body'])('discards a %s hash mismatch without creating an object URL', async target => {
    hash.mockImplementation(async blob => target === 'domain' && blob.type !== 'image/png' || target === 'body' && blob.type === 'image/png'
      ? '9'.repeat(64) : blob.type === 'image/png' ? displayHash : mediaId);
    state.open(mediaScope()); state.load(); await flush(); expect(state.data()).toBeNull(); expect(state.error()).not.toBe(''); expect(create).not.toHaveBeenCalled();
  });
  it('cancels pending reads and discards late hashing on selection loss', async () => {
    state.open(mediaScope()); state.load(); const pending = request(); state.clear(); expect(pending.cancelled).toBe(true);
    state.open(mediaScope()); state.load(); let finish!: (value: string) => void;
    hash.mockImplementationOnce(() => new Promise<string>(resolve => finish = resolve));
    const response = mediaResponse(); request().flush(response.body, { headers: response.headers }); state.open(null); finish(mediaId); await settle();
    expect(create).not.toHaveBeenCalled(); expect(state.data()).toBeNull();
  });
  it('checks the live account again after hashing even before reactive cleanup runs', async () => {
    state.open(mediaScope()); state.load(); let finish!: (value: string) => void;
    hash.mockImplementationOnce(() => new Promise<string>(resolve => finish = resolve));
    const response = mediaResponse(); request().flush(response.body, { headers: response.headers });
    profile.set({ user_id: '00000000-0000-4000-8000-000000000099' }); finish(mediaId); await settle();
    expect(create).not.toHaveBeenCalled(); expect(hash).toHaveBeenCalledOnce(); expect(state.owns(mediaScope())).toBe(false); state.load(); http.expectNone(() => true);
  });
  it('revokes pixels on decode mismatch and destruction and retries only after an explicit load', async () => {
    state.open(mediaScope()); state.load(); await flush(); state.decoded('blob:sealed-media', 2, 2);
    expect(revoke).toHaveBeenCalledWith('blob:sealed-media'); expect(state.data()).toBeNull(); http.expectNone(() => true);
    state.load(); await flush(); state.ngOnDestroy(); expect(revoke).toHaveBeenCalledTimes(2); expect(state.data()).toBeNull();
  });
  it('keeps source backoff explicit and latches access denial for the page owner', async () => {
    state.open(mediaScope()); state.load(); request().flush(new Blob([JSON.stringify({ error: { message_he: 'המתנה לספק' } })]), { status: 503, statusText: 'Backoff' });
    await settle(); expect(state.error()).toBe('המתנה לספק'); expect(TestBed.inject(SharedReviewBodyApi).denied()).toBe(false); http.expectNone(() => true);
    state.load(); request().flush(new Blob(), { status: 403, statusText: 'Denied' }); await settle();
    expect(TestBed.inject(SharedReviewBodyApi).denied()).toBe(true); state.load(); http.expectNone(() => true);
  });
});
