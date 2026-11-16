import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { By } from '@angular/platform-browser';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, convertToParamMap, provideRouter, Router } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { BehaviorSubject } from 'rxjs';
import { EditorModel, EditorSource, SourcePreview, SaveJob } from '../../core/api/editor-api';
import { SessionState } from '../../core/auth/session-state';
import { EditorPage } from './editor-page';
import { RichEditor } from './rich-editor';
import { editorExit } from './editor-exit';
import contract from '../../../../../fixtures/editor-contract.json';

const proposal = '00000000-0000-4000-8000-000000000001';
const repository = '00000000-0000-4000-8000-000000000002';
const model = contract[0].model as unknown as EditorModel;
const sha = 'a'.repeat(40), doc = model.document_id;
const markdown = model.preamble + model.blocks.map(block => block.prefix + block.markdown).join('') + model.suffix;
const source: EditorSource = { proposal_id: proposal, repository_id: repository, number: 25, title: 'שינוי הרשאות',
  branch: 'change/25-permissions', target_branch: 'main', proposal_version: 1, recorded_head_sha: sha, snapshot_sha: sha,
  document_id: doc, can_edit: true, markdown, sha256: model.sha256, model, model_error: null, parse_error: null,
  findings: [], findings_total: 0, findings_truncated: false };
const context = { proposal_id: proposal, proposal_version: 1, proposal_state: 'DRAFT', current_head_sha: sha,
  can_edit: true, can_store_recovery: true, retention_days: 30 };
const buffer = { document_id: doc, base_head_sha: sha, version: 7, updated_at: '2026-11-16T00:00:00Z',
  expires_at: '2026-12-16T00:00:00Z', content_utf8: 'private recovery', base_is_current: true };

const savedHead = 'b'.repeat(40), savedHash = 'c'.repeat(64);
const saveTicket = { proposal_id: proposal, job_id: '00000000-0000-4000-8000-000000000003', operation_id: '00000000-0000-4000-8000-000000000004' };
const normalize = (text: string) => text.replace(/\r\n?/g, '\n').normalize('NFC');
function savedJob(noop = false): SaveJob {
  return { job_id: saveTicket.job_id, proposal_id: proposal, kind: 'SAVE_PROPOSAL', state: 'SUCCEEDED', attempts: 1, phase: 'READY',
    creation: null, error_code: null, save_failure: null, save: { proposal_id: proposal, head_sha: noop ? sha : savedHead,
      proposal_version: noop ? 1 : 2, no_op: noop, reconciled: false, committed_at: noop ? null : '2026-11-16T00:00:00Z',
      documents: noop ? [] : [{ document_id: doc, path: 'docs/one.md', sha256: savedHash, bytes: 100, normalization_changed: true, normalization_only: false }] } };
}
function savedSource(text: string): EditorSource {
  return { ...source, snapshot_sha: savedHead, recorded_head_sha: savedHead, proposal_version: 2, markdown: normalize(text), sha256: savedHash, model: null };
}
describe('Proposal editor page', () => {
  const geometry = new Map<string, PropertyDescriptor | undefined>();
  beforeEach(() => {
    for (const name of ['getClientRects', 'getBoundingClientRect']) {
      geometry.set(name, Object.getOwnPropertyDescriptor(Range.prototype, name));
      Object.defineProperty(Range.prototype, name, { configurable: true, value: () => name === 'getClientRects' ? [] : new DOMRect() });
    }
    vi.spyOn(window, 'scrollBy').mockImplementation(() => {});
  });
  afterEach(() => {
    vi.restoreAllMocks();
    for (const [name, descriptor] of geometry) {
      if (descriptor) Object.defineProperty(Range.prototype, name, descriptor); else Reflect.deleteProperty(Range.prototype, name);
    }
  });
  async function create() {
    const profile = signal<{ user_id: string } | null>({ user_id: 'actor-a' }), logoutEpoch = signal(0);
    const params = new BehaviorSubject(convertToParamMap({ id: proposal, document: doc }));
    const query = new BehaviorSubject(convertToParamMap({ commit: sha }));
    let logoutGuard: () => boolean = () => true;
    const session = { profile, logoutEpoch, status: () => profile() ? 'authenticated' : 'anonymous', refresh: vi.fn(),
      guardLogout: (guard: () => boolean) => { logoutGuard = guard; return vi.fn(); } };
    await TestBed.configureTestingModule({
      imports: [EditorPage], providers: [provideHttpClient(), provideHttpClientTesting(), provideRouter([]),
        { provide: SessionState, useValue: session },
        { provide: ActivatedRoute, useValue: { paramMap: params, queryParamMap: query,
          snapshot: { paramMap: params.value, queryParamMap: query.value } } }],
    }).compileComponents();
    const router = TestBed.inject(Router);
    const navigate = vi.spyOn(router, 'navigate').mockImplementation((_commands, extras) => {
      query.next(convertToParamMap({ commit: extras?.queryParams?.['commit'] ?? query.value.get('commit') }));
      return Promise.resolve(true);
    });
    const fixture = TestBed.createComponent(EditorPage), page = fixture.componentInstance;
    const http = TestBed.inject(HttpTestingController);
    const load = () => http.expectOne(request => request.url.endsWith(`/documents/${doc}/editor/source`));
    const recovery = () => http.expectOne(request => request.method === 'GET' && request.url.endsWith('/draft'));
    const rich = () => fixture.debugElement.query(By.directive(RichEditor))?.componentInstance as RichEditor | undefined;
    fixture.detectChanges();
    async function opened(value: EditorSource = source, buffers: object[] = []) {
      const request = load(); expect(request.request.params.get('commit')).toBe(sha);
      expect(request.request.headers.get('X-Expected-Actor')).toBe(profile()?.user_id);
      request.flush(value); recovery().flush({ context, buffers }); fixture.detectChanges(); await fixture.whenStable();
    }
    return { fixture, page, http, load, recovery, opened, rich, profile, logoutEpoch, session, navigate, query, guard: () => logoutGuard() };
  }

  it('edits a stable heading, flushes recovery, labels the acknowledgement honestly and guards exit', async () => {
    const { fixture, page, http, opened, rich, guard } = await create(); await opened();
    const id = model.blocks.find(block => block.kind === 'heading')!.section_id!;
    rich()!.focusSection(id); rich()!.block('h4');
    expect(page.recovery.content()).toContain(`<!-- ss:section:${id} -->`);
    expect(page.recovery.content()).toContain('#### '); page.backup();
    const request = http.expectOne(req => req.method === 'PUT' && req.url.endsWith('/draft'));
    expect(request.request.body.base_head_sha).toBe(sha);
    request.flush({ context, buffer: { ...buffer, content_utf8: null }, replayed: false });
    fixture.detectChanges(); expect(fixture.nativeElement.textContent).toContain('עותק התאוששות אושר בשרת');
    expect(fixture.nativeElement.textContent).toContain('אינו שמירה ב־Git');
    vi.spyOn(window, 'confirm').mockReturnValue(false); expect(page.canLeave()).toBe(false); expect(guard()).toBe(false);
    const event = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent;
    page.beforeUnload(event); expect(event.defaultPrevented).toBe(true);
    fixture.destroy(); http.verify();
  });

  it('keeps malformed source intact and renders a server preview as inert content', async () => {
    const { fixture, page, http, opened } = await create();
    await opened({ ...source, model: null, markdown: '---\nunfinished', parse_error: 'INVALID_FRONT_MATTER' });
    expect(page.mode()).toBe('source'); page.sourceChanged('---\nunfinished ** <script>alert(1)</script>'); page.validate();
    const request = http.expectOne(req => req.url.endsWith('/source-preview'));
    const result: SourcePreview = { can_edit: true, model: null, model_error: 'INVALID_FRONT_MATTER',
      parse_error: 'INVALID_FRONT_MATTER', base_parse_error: 'INVALID_FRONT_MATTER', preview: {
        proposal_id: proposal, proposal_version: 1, document_id: doc, base_snapshot_sha: sha, base_document_sha256: source.sha256,
        markdown: page.recovery.content(), sha256: 'b'.repeat(64), changed: true, removed_section_ids: null,
        findings: [], findings_total: 0, findings_truncated: false,
        render_events: [{ kind: 'text', text: '<script>alert(1)</script>', source_start_byte: 0, source_end_byte: 1 }], render_error: null,
      } };
    expect(request.request.body.markdown).toBe(page.recovery.content()); request.flush(result);
    fixture.detectChanges(); await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('script')).toBeNull();
    expect(fixture.nativeElement.textContent).toContain('<script>alert(1)</script>');
    page.richMode(); http.expectOne(req => req.url.endsWith('/source-preview')).flush(result);
    expect(page.mode()).toBe('source'); expect(page.previewError()).toBeTruthy();
    fixture.destroy(); http.verify();
  });

  it('compares an old-base recovery and adopts it only after a separate explicit choice', async () => {
    const { fixture, page, http, opened } = await create();
    await opened(source, [{ ...buffer, base_head_sha: 'b'.repeat(40), base_is_current: false }]);
    expect(fixture.nativeElement.textContent).toContain('private recovery'); expect(page.recovery.content()).toBe(markdown);
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false); page.restore(); expect(page.recovery.content()).toBe(markdown);
    confirm.mockReturnValue(true); page.restore(); expect(confirm).toHaveBeenLastCalledWith(expect.stringContaining('מגרסה אחרת'));
    expect(page.recovery.content()).toBe('private recovery'); expect(page.mode()).toBe('source'); page.backup();
    const request = http.expectOne(req => req.method === 'PUT' && req.url.endsWith('/draft'));
    expect(request.request.body).toMatchObject({ base_head_sha: sha, expected_draft_version: 7, content_utf8: 'private recovery' });
    request.flush({ context, buffer: { ...buffer, content_utf8: null, version: 8 }, replayed: false });
    fixture.destroy(); http.verify();
  });

  it('hides private content during session refresh and restores rich history and unfinished fields for the same actor', async () => {
    const { fixture, page, http, opened, rich, profile, load, recovery } = await create(); await opened();
    const id = model.blocks.find(block => block.kind === 'heading')!.section_id!;
    rich()!.focusSection(id); rich()!.block('h4'); rich()!.openLink(); rich()!.linkUrl.set('unfinished private link');
    const local = page.recovery.content(); profile.set(null); fixture.detectChanges(); await fixture.whenStable();
    expect(rich()).toBeUndefined(); expect(fixture.nativeElement.textContent).not.toContain('שינוי הרשאות');
    expect(page.recovery.content()).toBe(local); expect(page.richSnapshot()?.linkUrl).toBe('unfinished private link');
    profile.set({ user_id: 'actor-a' }); fixture.detectChanges();
    load().flush(source); recovery().flush({ context, buffers: [] }); fixture.detectChanges(); await fixture.whenStable();
    expect(rich()!.state!.doc.firstChild!.attrs['level']).toBe(4);
    expect(rich()!.linkOpen()).toBe(true); expect(rich()!.linkUrl()).toBe('unfinished private link');
    rich()!.closeLink(); rich()!.undo(); expect(page.recovery.content()).toBe(markdown);
    fixture.destroy(); http.verify();
  });

  it('purges local source, rich snapshots and buffers when the account changes or logout succeeds', async () => {
    const { fixture, page, http, opened, profile, logoutEpoch, load, recovery } = await create(); await opened();
    page.sourceChanged('private input by actor-a'); profile.set({ user_id: 'actor-b' }); fixture.detectChanges();
    expect(page.recovery.content()).toBe(''); expect(page.richSnapshot()).toBeNull();
    const switched = load(); expect(switched.request.headers.get('X-Expected-Actor')).toBe('actor-b'); switched.flush(source);
    recovery().flush({ context, buffers: [] }); fixture.detectChanges(); await fixture.whenStable();
    expect(page.recovery.content()).toBe(markdown); page.sourceChanged('private input by actor-b');
    logoutEpoch.set(1); profile.set(null); fixture.detectChanges(); await fixture.whenStable();
    expect(page.owner()).toBeNull(); expect(page.source()).toBeNull(); expect(page.recovery.content()).toBe('');
    expect(page.rich()).toBeNull(); expect(page.richSnapshot()).toBeNull(); expect(page.preview()).toBeNull();
    fixture.destroy(); http.verify();
  });

  it('freezes the rich editor after permission loss while preserving local text and pending fields', async () => {
    const { fixture, page, http, opened, rich, load, recovery } = await create(); await opened();
    rich()!.focusSection(model.blocks.find(block => block.kind === 'heading')!.section_id!); rich()!.block('h4');
    rich()!.openLink(); rich()!.linkUrl.set('unfinished'); const local = page.recovery.content(); page.refresh();
    load().flush({ ...source, can_edit: false, proposal_version: 2 });
    recovery().flush({ context: { ...context, can_edit: false, proposal_version: 2 }, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable();
    expect(page.editable()).toBe(false); expect(page.recovery.content()).toBe(local);
    expect(rich()!.linkUrl()).toBe('unfinished'); rich()!.undo(); expect(page.recovery.content()).toBe(local);
    expect(fixture.nativeElement.querySelector('.ProseMirror').getAttribute('contenteditable')).toBe('false');
    fixture.destroy(); http.verify();
  });

  it('cancels stale previews on editing and prevents export of stale Markdown after rich serialization fails', async () => {
    const { fixture, page, http, opened, rich } = await create(); await opened();
    page.validate(); const pending = http.expectOne(req => req.url.endsWith('/source-preview'));
    page.sourceChanged('newer input'); expect(pending.cancelled).toBe(true); expect(page.preview()).toBeNull();
    rich()!.error.set('unrepresentable'); page.invalidRich();
    const objectUrl = vi.fn().mockReturnValue('blob:private');
    const savedDescriptor = Object.getOwnPropertyDescriptor(URL, 'createObjectURL');
    Object.defineProperty(URL, 'createObjectURL', { configurable: true, value: objectUrl });
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (this: HTMLAnchorElement) {
      expect(this.download).toContain('uncommitted.editor.json');
    });
    try { page.download(); expect(objectUrl).toHaveBeenCalled(); }
    finally { if (savedDescriptor) Object.defineProperty(URL, 'createObjectURL', savedDescriptor); else Reflect.deleteProperty(URL, 'createObjectURL'); }
    // Disposing the fixture clears source and cancels queued recovery writes.
    fixture.destroy(); http.verify();
  });

  it('guards document and base changes even when Angular reuses the route, while section navigation retains the buffer', async () => {
    const profile = signal<{ user_id: string } | null>(null);
    await TestBed.configureTestingModule({
      imports: [EditorPage], providers: [provideHttpClient(), provideHttpClientTesting(),
        provideRouter([{ path: 'changes/:id/edit/:document', component: EditorPage, canDeactivate: [editorExit],
          runGuardsAndResolvers: 'paramsOrQueryParamsChange' }]),
        { provide: SessionState, useValue: { profile, logoutEpoch: signal(0), status: () => 'anonymous', refresh: vi.fn(),
          guardLogout: () => () => {} } }],
    }).compileComponents();
    const harness = await RouterTestingHarness.create();
    const path = `/changes/${proposal}/edit/${doc}?commit=${sha}`;
    const page = await harness.navigateByUrl(path, EditorPage), http = TestBed.inject(HttpTestingController);
    profile.set({ user_id: 'actor-a' }); harness.detectChanges();
    http.expectOne(request => request.url.endsWith('/editor/source')).flush(source);
    http.expectOne(request => request.url.endsWith('/draft')).flush({ context, buffers: [] });
    harness.detectChanges(); await harness.fixture.whenStable();
    page.sourceChanged('private unsaved route buffer');
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    const same = await harness.navigateByUrl(`${path}&section=SEC-EXAMPLE`, EditorPage);
    expect(same).toBe(page); expect(confirm).not.toHaveBeenCalled(); expect(page.recovery.content()).toBe('private unsaved route buffer');
    await harness.navigateByUrl(`/changes/${proposal}/edit/DOC-OTHER?commit=${sha}`);
    expect(confirm).toHaveBeenCalled(); expect(page.recovery.content()).toBe('private unsaved route buffer');
    await harness.navigateByUrl(`/changes/${proposal}/edit/${doc}?commit=${'b'.repeat(40)}`);
    expect(page.recovery.content()).toBe('private unsaved route buffer');
    http.expectNone(request => request.url.includes('DOC-OTHER')); http.verify(); harness.fixture.destroy();
  });
  it('saves exact active source, preserves later typing, advances URL/base and submits subsequent work against the verified version', async () => {
    const { fixture, page, http, opened, load, recovery, navigate } = await create(); await opened(); page.sourceMode();
    const first = markdown + '\r\nfirst change e\u0301'; page.sourceChanged(first); page.saveGit();
    const request = http.expectOne(req => req.url.endsWith('/commits'));
    expect(request.request.body).toMatchObject({ expected_head_sha: sha, expected_proposal_version: 1,
      documents: [{ document_id: doc, expected_document_sha256: source.sha256, markdown: first }] });
    const later = first + '\r\nlater local typing'; page.sourceChanged(later); page.saveGit(); http.expectNone(req => req.url.endsWith('/commits'));
    request.flush(saveTicket); http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`).flush(savedJob()); fixture.detectChanges();
    const fetch = load(); expect(fetch.request.params.get('commit')).toBe(savedHead); fetch.flush(savedSource(first));
    recovery().flush({ context: { ...context, current_head_sha: savedHead, proposal_version: 2 }, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable();
    expect(page.recovery.content()).toBe(later); expect(page.recovery.dirty()).toBe(true); expect(page.source()?.snapshot_sha).toBe(savedHead);
    expect(page.git.pending()).toBe(false); expect(navigate).toHaveBeenCalledWith([], expect.objectContaining({ queryParams: { commit: savedHead }, replaceUrl: true }));
    expect(fixture.nativeElement.textContent).toContain('שמירה אחרונה אומתה');
    expect(fixture.nativeElement.textContent).toContain('יש עריכות מקומיות נוספות');
    page.saveGit(); const next = http.expectOne(req => req.url.endsWith('/commits'));
    expect(next.request.body.expected_head_sha).toBe(savedHead); expect(next.request.body.expected_proposal_version).toBe(2);
    expect(next.request.body.documents[0].expected_document_sha256).toBe(savedHash); expect(next.request.body.documents[0].markdown).toBe(later);
    next.flush({}, { status: 422, statusText: 'Invalid' }); fixture.destroy(); http.verify();
  });

  it('keeps rich history and newly opened unfinished fields when the accepted save becomes the new baseline', async () => {
    const { fixture, page, http, opened, load, recovery, rich } = await create(); await opened();
    const id = model.blocks.find(block => block.kind === 'heading')!.section_id!;
    rich()!.focusSection(id); rich()!.block('h4'); const first = page.recovery.content(); page.saveGit();
    const request = http.expectOne(req => req.url.endsWith('/commits')); rich()!.block('h3'); rich()!.openLink(); rich()!.linkUrl.set('unfinished later link');
    const latest = page.recovery.content(); const instance = rich();
    request.flush(saveTicket); http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`).flush(savedJob()); fixture.detectChanges();
    load().flush(savedSource(first)); recovery().flush({ context: { ...context, current_head_sha: savedHead, proposal_version: 2 }, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable();
    expect(rich()).toBe(instance); expect(rich()!.linkOpen()).toBe(true); expect(rich()!.linkUrl()).toBe('unfinished later link');
    expect(page.recovery.content()).toBe(normalize(latest)); rich()!.closeLink(); rich()!.undo();
    expect(page.recovery.content()).not.toBe(normalize(latest)); expect(page.recovery.dirty()).toBe(true);
    page.saveGit(); const next = http.expectOne(req => req.url.endsWith('/commits'));
    expect(next.request.body.expected_head_sha).toBe(savedHead); next.flush({}, { status: 422, statusText: 'Invalid' });
    fixture.destroy(); http.verify();
  });

  it('flushes active rich fields before sending and blocks an invalid field without sending stale Markdown', async () => {
    const { fixture, page, http, opened, rich } = await create(); await opened();
    rich()!.openLink(); rich()!.linkUrl.set('javascript:alert(1)'); page.saveGit();
    http.expectNone(req => req.url.endsWith('/commits')); expect(rich()!.linkOpen()).toBe(true);
    rich()!.closeLink(); page.saveGit(); const request = http.expectOne(req => req.url.endsWith('/commits'));
    expect(request.request.body.documents[0].markdown).toBe(page.recovery.content());
    request.flush({}, { status: 422, statusText: 'Invalid' }); fixture.destroy(); http.verify();
  });

  it('withholds baseline promotion on mismatched source and retries only the pinned read', async () => {
    const { fixture, page, http, opened, load, recovery } = await create(); await opened(); page.sourceMode();
    const input = markdown + '\nchanged'; page.sourceChanged(input); page.saveGit();
    http.expectOne(req => req.url.endsWith('/commits')).flush(saveTicket);
    http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`).flush(savedJob()); fixture.detectChanges();
    load().flush({ ...savedSource(input), markdown: 'wrong content' }); fixture.detectChanges();
    expect(page.source()?.snapshot_sha).toBe(sha); expect(page.recovery.content()).toBe(input); expect(page.git.pending()).toBe(true);
    expect(page.saveApplyError()).toBeTruthy(); page.saveGit(); http.expectNone(req => req.url.endsWith('/commits'));
    page.installSaved(true); load().flush(savedSource(input));
    recovery().flush({ context: { ...context, current_head_sha: savedHead, proposal_version: 2 }, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable(); expect(page.git.pending()).toBe(false); expect(page.recovery.dirty()).toBe(false);
    vi.spyOn(window, 'confirm').mockReturnValue(false); expect(page.canLeave()).toBe(true); fixture.destroy(); http.verify();
  });

  it('reports a no-op without a commit time and guards an accepted operation even if local source is clean', async () => {
    const { fixture, page, http, opened, load, recovery, navigate } = await create(); await opened(); page.saveGit();
    http.expectOne(req => req.url.endsWith('/commits')).flush(saveTicket);
    const poll = http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`);
    vi.spyOn(window, 'confirm').mockReturnValue(false); expect(page.canLeave()).toBe(false);
    const event = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent; page.beforeUnload(event); expect(event.defaultPrevented).toBe(true);
    poll.flush(savedJob(true)); fixture.detectChanges(); load().flush(source); recovery().flush({ context, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable(); expect(page.git.result()?.committed_at).toBeNull(); expect(navigate).not.toHaveBeenCalled();
    expect(page.recovery.dirty()).toBe(false); expect(page.canLeave()).toBe(true); expect(fixture.nativeElement.textContent).toContain('לא נוצר Commit');
    fixture.destroy(); http.verify();
  });

  it('keeps an unrepresentable later rich edit and guards exit after installing the earlier verified save', async () => {
    const { fixture, page, http, opened, load, recovery, rich } = await create(); await opened();
    rich()!.focusSection(model.blocks.find(block => block.kind === 'heading')!.section_id!); rich()!.block('h4');
    const first = page.recovery.content(); page.saveGit(); const request = http.expectOne(req => req.url.endsWith('/commits'));
    rich()!.error.set('unrepresentable later state'); page.invalidRich();
    request.flush(saveTicket); http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`).flush(savedJob()); fixture.detectChanges();
    load().flush(savedSource(first)); recovery().flush({ context: { ...context, current_head_sha: savedHead, proposal_version: 2 }, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable(); expect(page.brokenRich()).toBe(true);
    expect(rich()!.error()).toBe('unrepresentable later state'); expect(page.git.pending()).toBe(false);
    page.saveGit(); http.expectNone(req => req.url.endsWith('/commits'));
    vi.spyOn(window, 'confirm').mockReturnValue(false); expect(page.canLeave()).toBe(false);
    rich()!.undo(); expect(page.brokenRich()).toBe(false); expect(page.recovery.dirty()).toBe(true);
    fixture.destroy(); http.verify();
  });

  it('freezes an interrupted submission through same-user login and purges it on a different login', async () => {
    const { fixture, page, http, opened, load, recovery, profile } = await create(); await opened(); page.sourceMode();
    page.sourceChanged(markdown + '\nprivate original intent'); page.saveGit(); const first = http.expectOne(req => req.url.endsWith('/commits'));
    const key = first.request.headers.get('Idempotency-Key'), body = first.request.body;
    profile.set(null); fixture.detectChanges(); await fixture.whenStable(); expect(first.cancelled).toBe(true);
    profile.set({ user_id: 'actor-a' }); fixture.detectChanges(); load().flush(source); recovery().flush({ context, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable(); expect(page.git.canRetry()).toBe(true); http.expectNone(req => req.url.endsWith('/commits'));
    page.git.retry(); const retry = http.expectOne(req => req.url.endsWith('/commits'));
    expect(retry.request.body).toEqual(body); expect(retry.request.headers.get('Idempotency-Key')).toBe(key);
    retry.flush(saveTicket); const pending = http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`);
    profile.set({ user_id: 'actor-b' }); fixture.detectChanges(); expect(pending.cancelled).toBe(true);
    expect(page.git.result()).toBeNull(); expect(page.git.submitted()).toBeNull(); expect(page.git.ticket()).toBeNull();
    load().flush(source); recovery().flush({ context, buffers: [] }); fixture.detectChanges(); await fixture.whenStable();
    expect(page.recovery.content()).toBe(markdown); fixture.destroy(); http.verify();
  });

  it('installs its own saved historical snapshot read-only when another operation already advanced recorded state', async () => {
    const { fixture, page, http, opened, load, recovery } = await create(); await opened(); page.sourceMode();
    const first = markdown + '\nfirst'; page.sourceChanged(first); page.saveGit(); const request = http.expectOne(req => req.url.endsWith('/commits'));
    page.sourceChanged(first + '\nlater local work'); request.flush(saveTicket);
    http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`).flush(savedJob()); fixture.detectChanges();
    load().flush({ ...savedSource(first), recorded_head_sha: 'd'.repeat(40), proposal_version: 3, can_edit: false });
    recovery().flush({ context: { ...context, current_head_sha: 'd'.repeat(40), proposal_version: 3 }, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable(); expect(page.editable()).toBe(false);
    expect(page.recovery.content()).toContain('later local work'); expect(page.source()?.snapshot_sha).toBe(savedHead);
    expect(page.git.pending()).toBe(false); page.saveGit(); http.expectNone(req => req.url.endsWith('/commits'));
    fixture.destroy(); http.verify();
  });

  it('shows incoming-reference blockers from untouched chapters without claiming a save or replacing source', async () => {
    const { fixture, page, http, opened } = await create(); await opened(); page.sourceMode();
    const local = markdown + '\nlocal edit'; page.sourceChanged(local); page.saveGit();
    http.expectOne(req => req.url.endsWith('/commits')).flush(saveTicket);
    const failed = savedJob(); failed.state = 'FAILED'; failed.phase = 'FAILED'; failed.save = null; failed.error_code = 'SAVE_VALIDATION_FAILED';
    failed.save_failure = { validation: { complete: true, findings_total: 1, blockers_total: 1, findings_truncated: false, findings: [
      { code: 'SECTION_MISSING', path: 'docs/untouched.md', path_truncated: false, severity: 'blocker', source_start_byte: 42, source_end_byte: 55 },
    ] } };
    http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`).flush(failed); fixture.detectChanges();
    expect(fixture.nativeElement.textContent).toContain('docs/untouched.md'); expect(fixture.nativeElement.textContent).toContain('SECTION_MISSING');
    expect(page.recovery.content()).toBe(local); expect(page.git.result()).toBeNull(); expect(page.source()?.snapshot_sha).toBe(sha);
    fixture.destroy(); http.verify();
  });

  it('promotes its verified base through the actual router without discarding later edits or bypassing another document guard', async () => {
    const profile = signal<{ user_id: string } | null>(null);
    await TestBed.configureTestingModule({ imports: [EditorPage], providers: [provideHttpClient(), provideHttpClientTesting(),
      provideRouter([{ path: 'changes/:id/edit/:document', component: EditorPage, canDeactivate: [editorExit], runGuardsAndResolvers: 'paramsOrQueryParamsChange' }]),
      { provide: SessionState, useValue: { profile, logoutEpoch: signal(0), status: () => 'authenticated', refresh: vi.fn(), guardLogout: () => () => {} } }],
    }).compileComponents();
    const harness = await RouterTestingHarness.create();
    const page = await harness.navigateByUrl(`/changes/${proposal}/edit/${doc}?commit=${sha}`, EditorPage);
    const http = TestBed.inject(HttpTestingController), router = TestBed.inject(Router);
    profile.set({ user_id: 'actor-a' }); harness.detectChanges();
    http.expectOne(req => req.url.endsWith('/editor/source')).flush(source);
    http.expectOne(req => req.url.endsWith('/draft')).flush({ context, buffers: [] }); harness.detectChanges(); await harness.fixture.whenStable();
    page.sourceMode(); const first = markdown + '\nfirst'; page.sourceChanged(first); page.saveGit();
    const request = http.expectOne(req => req.url.endsWith('/commits')); page.sourceChanged(first + '\nlater');
    request.flush(saveTicket); http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`).flush(savedJob()); harness.detectChanges();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    http.expectOne(req => req.url.endsWith('/editor/source')).flush(savedSource(first));
    http.expectOne(req => req.url.endsWith('/draft')).flush({ context: { ...context, current_head_sha: savedHead, proposal_version: 2 }, buffers: [] });
    harness.detectChanges(); await harness.fixture.whenStable();
    expect(router.url).toContain(`commit=${savedHead}`); expect(confirm).not.toHaveBeenCalled();
    expect(page.recovery.content()).toBe(first + '\nlater'); expect(page.source()?.snapshot_sha).toBe(savedHead);
    const same = await harness.navigateByUrl(`/changes/${proposal}/edit/${doc}?commit=${savedHead}&section=SEC-NEXT`, EditorPage);
    expect(same).toBe(page); expect(confirm).not.toHaveBeenCalled(); expect(page.recovery.content()).toContain('later');
    await harness.navigateByUrl(`/changes/${proposal}/edit/DOC-OTHER?commit=${savedHead}`);
    expect(confirm).toHaveBeenCalled(); expect(router.url).not.toContain('DOC-OTHER');
    harness.fixture.destroy(); http.verify();
  });

  it('cancels an older refresh before the verified source is installed', async () => {
    const { fixture, page, http, opened, load, recovery } = await create(); await opened(); page.sourceMode();
    const input = markdown + '\nchanged'; page.sourceChanged(input); page.saveGit();
    http.expectOne(req => req.url.endsWith('/commits')).flush(saveTicket);
    const poll = http.expectOne(`/api/v1/jobs/${saveTicket.job_id}`);
    page.refresh(); const old = load(); recovery().flush({ context, buffers: [] });
    poll.flush(savedJob()); fixture.detectChanges(); expect(old.cancelled).toBe(true);
    load().flush(savedSource(input)); recovery().flush({ context: { ...context, current_head_sha: savedHead, proposal_version: 2 }, buffers: [] });
    fixture.detectChanges(); await fixture.whenStable(); expect(page.source()?.snapshot_sha).toBe(savedHead);
    fixture.destroy(); http.verify();
  });

});
