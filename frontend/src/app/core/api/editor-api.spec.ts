import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { EditorApi, type SaveDraft } from './editor-api';

describe('Editor request identity', () => {
  it('retains the captured actor on an exact recovery retry after another request uses a different actor', () => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    const api = TestBed.inject(EditorApi);
    const http = TestBed.inject(HttpTestingController);
    const body: SaveDraft = {
      save_id: '00000000-0000-4000-8000-000000000003', document_id: 'DOC-ONE', base_head_sha: '1'.repeat(40),
      expected_draft_version: 0, content_utf8: 'private unfinished source',
    };
    api.saveRecovery('original-actor', 'proposal', body).subscribe({ error: () => {} });
    const uncertain = http.expectOne('/api/v1/proposals/proposal/draft');
    expect(uncertain.request.headers.get('X-Expected-Actor')).toBe('original-actor');
    uncertain.error(new ProgressEvent('error'));
    api.recovery('new-actor', 'proposal').subscribe();
    const other = http.expectOne('/api/v1/proposals/proposal/draft');
    expect(other.request.headers.get('X-Expected-Actor')).toBe('new-actor');
    other.flush({});
    api.saveRecovery('original-actor', 'proposal', body).subscribe({ error: () => {} });
    const retry = http.expectOne('/api/v1/proposals/proposal/draft');
    expect(retry.request.headers.get('X-Expected-Actor')).toBe('original-actor');
    expect(retry.request.body).toEqual(body);
    retry.flush({ error: { code: 'SESSION_IDENTITY_CHANGED' } }, { status: 409, statusText: 'Conflict' });
    http.verify();
  });
});
