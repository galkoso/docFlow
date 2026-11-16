import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { By } from '@angular/platform-browser';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, convertToParamMap, provideRouter } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { BehaviorSubject } from 'rxjs';
import { EditorModel, EditorSource, SourcePreview } from '../../core/api/editor-api';
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
    return { fixture, page, http, load, recovery, opened, rich, profile, logoutEpoch, session, guard: () => logoutGuard() };
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
});
