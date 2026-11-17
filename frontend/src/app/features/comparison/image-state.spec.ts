import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { IMAGE_DIGEST, ImageState } from './image-state';
import { displayHash, imageEvidence, imageResponse, imageScope, proofHeader } from './image.testing';
import type { ImageEvidence } from '../../core/api/comparison-api';

describe('Private comparison image URLs', () => {
  let state: ImageState, http: HttpTestingController, hash: ReturnType<typeof vi.fn<(blob: Blob) => Promise<string>>>;
  let create: ReturnType<typeof vi.fn<(blob: Blob | MediaSource) => string>>, revoke: ReturnType<typeof vi.fn<(url: string) => void>>;
  beforeEach(() => {
    hash = vi.fn<(blob: Blob) => Promise<string>>().mockResolvedValue(displayHash);
    create = vi.fn<(blob: Blob | MediaSource) => string>().mockReturnValue('blob:http://localhost/private'); revoke = vi.fn<(url: string) => void>();
    vi.stubGlobal('URL', class extends URL { static override createObjectURL(blob: Blob | MediaSource): string { return create(blob) as string; }
      static override revokeObjectURL(url: string): void { revoke(url); } });
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting(), ImageState, { provide: IMAGE_DIGEST, useValue: hash }] });
    state = TestBed.inject(ImageState); http = TestBed.inject(HttpTestingController);
  });
  afterEach(() => { state.clear(); http.verify(); vi.unstubAllGlobals(); });
  const request = () => http.expectOne(req => req.url.endsWith('/image'));
  async function flush() { const response = imageResponse(); request().flush(response.body, { headers: response.headers }); await Promise.resolve(); }

  it('captures exact actor/snapshot/file/side and creates a URL only after proof and body hash verification', async () => {
    let finish!: (value: string) => void; hash.mockImplementation(() => new Promise<string>(resolve => finish = resolve));
    const scope = imageScope(); state.open(scope); scope.file.actor = 'changed';
    const first = request(); expect(first.request.headers.get('X-Expected-Actor')).toBe(imageScope().file.actor);
    expect(first.request.responseType).toBe('blob'); expect(first.request.params.keys().sort()).toEqual(['side', 'snapshot']);
    const response = imageResponse(); first.flush(response.body, { headers: response.headers }); expect(create).not.toHaveBeenCalled();
    finish(displayHash); await Promise.resolve(); expect(state.data()?.url).toBe('blob:http://localhost/private');
    expect(state.loaded()).toBe(false); state.decoded(state.data()!.url, 1, 1); expect(state.loaded()).toBe(true);
    state.clear(); expect(revoke).toHaveBeenCalledWith('blob:http://localhost/private');
  });
  it('rejects foreign identity, source evidence, dimensions, MIME and byte counts before URL creation', async () => {
    const corrupt: Array<(proof: ImageEvidence) => void> = [
      p => p.actor_id = 'other', p => p.snapshot_id = 'other', p => p.file_id = '9'.repeat(64), p => p.head_sha = '9'.repeat(40),
      p => p.side = 'before', p => p.source_sha256 = '9'.repeat(64), p => p.source_blob_oid = '9'.repeat(40),
      p => p.source_bytes++, p => p.display_bytes++, p => p.width = 2049, p => p.height = 0,
    ];
    for (const change of corrupt) {
      state.clear(); state.open(imageScope()); const proof = imageEvidence(); change(proof);
      request().flush(new Blob(['PNG'], { type: 'image/png' }), { headers: { 'Content-Type': 'image/png', 'X-Comparison-Image': proofHeader(proof) } });
      await Promise.resolve(); expect(state.data()).toBeNull(); expect(state.error()).not.toBe('');
    }
    state.clear(); state.open(imageScope()); request().flush(new Blob(['<svg/>'], { type: 'image/svg+xml' }), {
      headers: { 'Content-Type': 'image/svg+xml', 'X-Comparison-Image': proofHeader() } });
    await Promise.resolve(); expect(create).not.toHaveBeenCalled();
  });
  it('discards a body digest mismatch and malformed/missing proof without rendering pixels', async () => {
    hash.mockResolvedValue('9'.repeat(64)); state.open(imageScope()); await flush(); expect(state.data()).toBeNull();
    for (const header of ['', 'invalid%data', 'a'.repeat(4097)]) {
      state.clear(); state.open(imageScope()); request().flush(new Blob(['PNG'], { type: 'image/png' }), {
        headers: { 'Content-Type': 'image/png', 'X-Comparison-Image': header } }); await Promise.resolve();
      expect(state.error()).not.toBe(''); expect(state.data()).toBeNull();
    }
    expect(create).not.toHaveBeenCalled();
  });
  it('cancels pending requests and discards async hashing after scope loss', async () => {
    state.open(imageScope()); const first = request(); state.clear(); expect(first.cancelled).toBe(true);
    let finish!: (value: string) => void; hash.mockImplementation(() => new Promise<string>(resolve => finish = resolve));
    state.open(imageScope()); const response = imageResponse(); request().flush(response.body, { headers: response.headers });
    state.open(null); finish(displayHash); await Promise.resolve(); expect(create).not.toHaveBeenCalled(); expect(state.data()).toBeNull();
  });
  it('revokes URLs on retry/decode failure/destruction and retries only the captured immutable side', async () => {
    state.open(imageScope()); await flush(); const url = state.data()!.url;
    state.decoded(url, 2, 2); expect(state.data()).toBeNull(); expect(revoke).toHaveBeenCalledWith(url);
    state.retry(); const retry = request(); expect(retry.request.params.get('snapshot')).toBe(imageScope().file.snapshot);
    const response = imageResponse(); retry.flush(response.body, { headers: response.headers }); await Promise.resolve();
    state.ngOnDestroy(); expect(revoke).toHaveBeenCalledTimes(2);
  });
  it('keeps missing sides and unsupported media explicit without network requests', () => {
    state.open({ ...imageScope(), side: 'before' }); expect(state.absent()).toBe(true);
    const scope = imageScope(); scope.file.file.after!.path = 'assets/unsupported.svg'; state.open(scope);
    expect(state.error()).not.toBe(''); state.retry(); http.expectNone(req => req.url.endsWith('/image'));
  });
  it('requires identity refresh after access errors and never automatically refetches', async () => {
    state.open(imageScope()); const body = new Blob([], { type: 'application/json' });
    Object.defineProperty(body, 'text', { value: async () => JSON.stringify({ error: { code: 'SESSION_IDENTITY_CHANGED' } }) });
    request().flush(body, { status: 409, statusText: 'Conflict' });
    await Promise.resolve(); await Promise.resolve(); expect(state.authRequired()).toBe(true); state.retry(); http.expectNone(req => req.url.endsWith('/image'));
  });
});
