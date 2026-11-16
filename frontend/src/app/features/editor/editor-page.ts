import {
  afterRenderEffect, ChangeDetectionStrategy, Component, computed, effect, HostListener, inject, OnDestroy, signal, untracked, viewChild,
} from '@angular/core';
import { DatePipe } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { EditorApi, EditorSource, SourcePreview } from '../../core/api/editor-api';
import { SessionState } from '../../core/auth/session-state';
import { DocumentRenderer } from '../specification/document-renderer';
import { DraftRecovery, RecoveryScope } from './draft-recovery';
import { GitSave } from './git-save';
import { RichDocument } from './rich-document';
import { RichChange, RichEditor, RichEditorSnapshot } from './rich-editor';

@Component({
  selector: 'app-editor-page', imports: [DatePipe, RouterLink, RichEditor, DocumentRenderer], providers: [DraftRecovery, GitSave],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './editor-page.html', styleUrl: './editor-page.css',
})
export class EditorPage implements OnDestroy {
  readonly session = inject(SessionState);
  readonly recovery = inject(DraftRecovery);
  readonly git = inject(GitSave);
  private readonly router = inject(Router);
  private readonly api = inject(EditorApi);
  private readonly route = inject(ActivatedRoute);
  private readonly params = toSignal(this.route.paramMap, { initialValue: this.route.snapshot.paramMap });
  private readonly query = toSignal(this.route.queryParamMap, { initialValue: this.route.snapshot.queryParamMap });
  private readonly widget = viewChild(RichEditor);
  private scope: RecoveryScope | null = null;
  private key = '';
  private active = false;
  private generation = 0;
  private previewGeneration = 0;
  private loadingRequest?: Subscription;
  private previewRequest?: Subscription;
  private seenLogout = this.session.logoutEpoch();
  private readonly removeLogoutGuard: () => void;
  private focused = '';
  private savedSourceRequest?: Subscription;
  private savedGeneration = 0;
  private attemptedJob = '';
  private canonicalRich = false;
  private richBase: string | null = null;
  private baseTransition: { from: string; to: string } | null = null;
  readonly commitMessage = signal('עדכון מסמך האפיון');
  readonly applyingSave = signal(false);
  readonly saveApplyError = signal('');
  readonly owner = signal<string | null>(null);
  readonly source = signal<EditorSource | null>(null);
  readonly rich = signal<RichDocument | null>(null);
  readonly richSnapshot = signal<RichEditorSnapshot | null>(null);
  readonly mode = signal<'rich' | 'source'>('source');
  readonly loading = signal(false);
  private readonly accessReady = signal(false);
  readonly error = signal('');
  readonly previewError = signal('');
  readonly previewing = signal(false);
  readonly preview = signal<SourcePreview | null>(null);
  readonly brokenRich = signal(false);
  readonly visible = computed(() => !!this.owner() && this.owner() === this.session.profile()?.user_id);
  readonly editable = computed(() => {
    const source = this.source(), context = this.recovery.context();
    return this.visible() && this.accessReady() && !this.loading() && !!source?.can_edit && (!context || (context.can_edit
      && context.current_head_sha === source.snapshot_sha && context.proposal_version === source.proposal_version));
  });
  readonly assets = computed(() => {
    const source = this.source();
    return source ? { repository: source.repository_id, proposal: source.proposal_id, commit: source.snapshot_sha } : null;
  });

  constructor() {
    this.removeLogoutGuard = this.session.guardLogout(() => this.canLeave(true));
    effect(() => {
      const actor = this.session.profile()?.user_id ?? null;
      const logout = this.session.logoutEpoch();
      const proposal = this.params().get('id'), document = this.params().get('document'), base = this.query().get('commit');
      untracked(() => {
        if (logout !== this.seenLogout) { this.seenLogout = logout; this.clear(); }
        if (!actor) { this.pause(); return; }
        const key = JSON.stringify([proposal, document, base]);
        if (this.owner() && this.owner() !== actor) this.clear();
        if (!proposal || !document || !base) {
          this.clear(); this.error.set('יש לבחור מסמך וגרסת הצעה מתוך ספר ההצעה.'); return;
        }
        const ownTransition = this.baseTransition && this.scope?.proposal === proposal && this.scope?.document === document
          && (base === this.baseTransition.from || base === this.baseTransition.to);
        if (ownTransition && base === this.baseTransition!.to) { this.key = key; this.baseTransition = null; }
        if (!ownTransition && (this.key !== key || !this.scope)) {
          this.clear(); this.key = key; this.owner.set(actor);
          this.scope = { actor, proposal, document, base }; this.active = true; this.load(false);
        } else if (!this.active) {
          this.active = true; this.load(true);
        }
      });
    });
    effect(() => {
      if (this.recovery.authRequired()) untracked(() => { this.recovery.authRequired.set(false); this.session.refresh(); });
    });
    effect(() => {
      if (this.git.authRequired()) untracked(() => { this.git.authRequired.set(false); this.session.refresh(); });
      if (this.git.contextChanged()) untracked(() => { this.git.contextChanged.set(false); this.accessReady.set(false); });
    });
    effect(() => {
      const result = this.git.result(), pending = this.git.pending(), status = this.git.status(), visible = this.visible();
      if (result && pending && status === 'completed' && visible) untracked(() => this.installSaved());
    });
    afterRenderEffect(() => {
      const widget = this.widget(), section = this.query().get('section');
      const rich = this.rich();
      if (widget && section && rich && this.focused !== `${this.key}/${section}`) {
        this.focused = `${this.key}/${section}`; untracked(() => widget.focusSection(section));
      }
    });
  }

  private load(resuming: boolean): void {
    const scope = this.scope;
    if (!scope) return;
    const generation = ++this.generation;
    this.loadingRequest?.unsubscribe(); this.loading.set(true); this.accessReady.set(false); this.error.set('');
    this.loadingRequest = this.api.source(scope.actor, scope.proposal, scope.document, scope.base).pipe(timeout(20_000)).subscribe({
      next: source => {
        if (generation !== this.generation || !this.active) return;
        try {
          if (source.proposal_id !== scope.proposal || source.document_id !== scope.document || source.snapshot_sha !== scope.base
            || !Number.isSafeInteger(source.proposal_version) || source.proposal_version < 1
            || !/^[a-f0-9]{64}$/.test(source.sha256) || typeof source.markdown !== 'string') throw new Error('EDITOR_SCOPE_MISMATCH');
          if (source.model && source.model.document_id !== scope.document) throw new Error('EDITOR_DOCUMENT_MISMATCH');
          if (resuming && this.source() && (source.repository_id !== this.source()!.repository_id
            || source.sha256 !== this.source()!.sha256 || source.markdown !== this.source()!.markdown)) {
            throw new Error('EDITOR_BASE_CHANGED');
          }
          if (!resuming || !this.source()) {
            const rich = source.model ? new RichDocument(source.model,
              { repository: source.repository_id, proposal: source.proposal_id, commit: source.snapshot_sha }) : null;
            if (rich && (rich.originalSource !== source.markdown || source.model!.sha256 !== source.sha256)) throw new Error('EDITOR_SOURCE_MISMATCH');
            this.rich.set(rich); this.richBase = rich ? scope.base : null; this.mode.set(rich ? 'rich' : 'source');
            this.recovery.open(scope, source.markdown);
          } else this.recovery.identity(scope.actor);
          this.source.set(source); this.loading.set(false); this.accessReady.set(true); this.git.identity(scope.actor);
          if (this.git.status() === 'completed' && this.git.pending()) this.installSaved();
        } catch {
          this.loading.set(false); this.error.set('זהות המקור או הגרסה אינה תואמת למסמך שנבחר. העריכה המקומית לא הוחלפה.');
        }
      },
      error: error => {
        if (generation !== this.generation) return;
        this.loading.set(false); this.error.set(this.message(error, 'לא ניתן לטעון את מקור ההצעה. אפשר לנסות שוב.'));
        if (error instanceof HttpErrorResponse && (error.status === 401 || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED')) {
          this.session.refresh();
        }
      },
    });
  }

  refresh(): void {
    if (!this.visible()) return;
    if (this.git.status() === 'completed' && this.git.pending()) { this.installSaved(true); return; }
    this.capture(); this.load(!!this.source());
    // An explicit refresh also rechecks recovery after a failed read or conflict.
    if (this.recovery.owner()) this.recovery.inspect();
  }
  private pause(): void {
    if (!this.active) return;
    this.capture(); this.active = false; this.accessReady.set(false); ++this.generation;
    this.loadingRequest?.unsubscribe(); this.cancelPreview(); this.recovery.identity(null); this.git.identity(null);
    ++this.savedGeneration; this.savedSourceRequest?.unsubscribe(); this.applyingSave.set(false); this.attemptedJob = '';
  }
  private capture(): void { const snapshot = this.widget()?.snapshot(); if (snapshot) this.richSnapshot.set(snapshot); }
  richChanged(change: RichChange): void {
    this.brokenRich.set(false); this.recovery.update(this.richSource(change.markdown)); this.cancelPreview();
  }
  sourceChanged(value: string): void { this.recovery.update(value); this.cancelPreview(); }
  invalidRich(): void { this.brokenRich.set(true); this.recovery.holdInput(); this.cancelPreview(); }
  private flushFields(): boolean {
    if (this.mode() === 'rich' && this.widget() && !this.widget()!.flushFields()) {
      this.error.set('יש לתקן או לבטל את השדה הפעיל בעורך לפני המשך הפעולה.'); return false;
    }
    return !this.brokenRich();
  }
  sourceMode(): void {
    if (!this.flushFields()) return;
    this.capture(); this.mode.set('source'); this.error.set('');
  }
  richMode(): void {
    if (this.mode() === 'rich') return;
    this.validate(true);
  }

  validate(toRich = false): void {
    const scope = this.scope, source = this.source();
    if (!scope || !source || !this.visible() || this.loading() || !this.flushFields()) return;
    this.cancelPreview(); const generation = this.previewGeneration;
    const markdown = this.recovery.content();
    if (new TextEncoder().encode(markdown).length > 2 * 1024 * 1024 || markdown.includes('\0')) {
      this.previewError.set('המקור חורג ממגבלת 2MiB או כולל תו אפס.'); return;
    }
    this.previewing.set(true);
    this.previewRequest = this.api.sourcePreview(scope.actor, scope.proposal, scope.document, scope.base,
      source.proposal_version, source.sha256, markdown).pipe(timeout(20_000)).subscribe({
      next: result => {
        if (generation !== this.previewGeneration || !this.active) return;
        this.previewing.set(false);
        const preview = result.preview;
        if (preview.proposal_id !== scope.proposal || preview.document_id !== scope.document || preview.base_snapshot_sha !== scope.base
          || preview.base_document_sha256 !== source.sha256 || preview.proposal_version !== source.proposal_version || preview.markdown !== markdown) {
          this.previewError.set('תוצאת הבדיקה אינה תואמת לגרסה או לעריכה המקומית.'); return;
        }
        this.preview.set(result);
        if (!result.can_edit) this.source.set({ ...source, can_edit: false });
        if (toRich) {
          try {
            if (!result.model || result.model.document_id !== scope.document || result.model.sha256 !== preview.sha256) {
              throw new Error('SOURCE_NOT_REPRESENTABLE');
            }
            const rich = new RichDocument(result.model, this.assets()!);
            if (rich.originalSource !== markdown) throw new Error('SOURCE_NOT_REPRESENTABLE');
            // Returning without changing source preserves rich history and active
            // field state; changed raw source gets a fresh validated rich baseline.
            if (!this.rich() || this.richSource(this.rich()!.changes(this.richSnapshot()?.state.doc ?? this.rich()!.doc).markdown) !== markdown) {
              this.rich.set(rich); this.richBase = scope.base; this.richSnapshot.set(null); this.canonicalRich = false;
            }
            this.mode.set('rich'); this.focused = '';
          } catch { this.previewError.set('המקור אינו זמין לעריכה עשירה. הוא נשאר בשלמותו במצב מקור.'); }
        }
      },
      error: error => {
        if (generation !== this.previewGeneration) return;
        this.previewing.set(false); this.previewError.set(this.message(error, 'הבדיקה לא הושלמה. הטקסט המקומי נשמר בדפדפן.'));
        if (error instanceof HttpErrorResponse && [403, 404, 409].includes(error.status)) this.accessReady.set(false);
        if (error instanceof HttpErrorResponse && (error.status === 401 || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED')) this.session.refresh();
      },
    });
  }

  saveGit(): void {
    const scope = this.scope, source = this.source();
    if (!scope || !source || !this.editable() || this.git.pending() || this.recovery.choiceRequired() || !this.flushFields()) return;
    this.capture(); this.cancelPreview(); this.saveApplyError.set('');
    this.git.start(scope.actor, scope.proposal, { expected_head_sha: source.snapshot_sha,
      expected_proposal_version: source.proposal_version, message: this.commitMessage(), documents: [
        { document_id: scope.document, expected_document_sha256: source.sha256, markdown: this.recovery.content() },
      ] });
  }
  private normalized(value: string): string { return value.replace(/\r\n?/g, '\n').normalize('NFC'); }
  private richSource(value: string): string { return this.canonicalRich ? this.normalized(value) : value; }
  canFollowSavedBase(proposal: string, document: string, commit: string | null): boolean {
    return !!this.baseTransition && this.baseTransition.to === commit && this.scope?.proposal === proposal
      && this.scope.document === document && this.scope.base === commit && this.git.result()?.head_sha === commit;
  }
  installSaved(retry = false): void {
    const result = this.git.result(), ticket = this.git.ticket(), submitted = this.git.submitted(), scope = this.scope, original = this.source();
    if (!result || !ticket || !submitted || !scope || !original || !this.active || !this.visible() || !this.git.pending()
      || this.applyingSave() || !retry && this.attemptedJob === ticket.job_id) return;
    this.attemptedJob = ticket.job_id; this.capture(); this.cancelPreview();
    ++this.generation; this.loadingRequest?.unsubscribe();
    const generation = ++this.savedGeneration;
    this.applyingSave.set(true); this.loading.set(true); this.saveApplyError.set('');
    this.savedSourceRequest = this.api.source(scope.actor, scope.proposal, scope.document, result.head_sha).pipe(timeout(20_000)).subscribe({
      next: source => {
        if (generation !== this.savedGeneration || !this.active) return;
        try {
          const document = result.documents.find(document => document.document_id === scope.document);
          const input = submitted.documents.find(document => document.document_id === scope.document)!;
          const expected = document ? this.normalized(input.markdown) : original.markdown;
          if (source.proposal_id !== scope.proposal || source.repository_id !== original.repository_id || source.document_id !== scope.document
            || source.snapshot_sha !== result.head_sha || source.sha256 !== (document?.sha256 ?? original.sha256) || source.markdown !== expected
            || !Number.isSafeInteger(source.proposal_version) || source.proposal_version < result.proposal_version
            || source.can_edit && (source.proposal_version !== result.proposal_version || source.recorded_head_sha !== result.head_sha)) {
            throw new Error('SAVE_BASE_MISMATCH');
          }
          const nextScope = { ...scope, base: result.head_sha };
          // Keep the live ProseMirror state/history and unfinished fields. Its
          // lossless old source fragments are canonicalized only after this
          // file's canonical bytes have been verified in the saved snapshot.
          const canonical = expected === this.normalized(expected);
          const local = this.recovery.content();
          const content = !this.brokenRich() && local === input.markdown ? expected
            : this.mode() === 'rich' && canonical ? this.normalized(local) : local;
          if (!this.recovery.rebase(nextScope, expected, content, !this.brokenRich())) throw new Error('RECOVERY_SCOPE_MISMATCH');
          this.canonicalRich = canonical; this.source.set(source); this.scope = nextScope;
          this.loading.set(false); this.applyingSave.set(false); this.accessReady.set(true);
          this.git.applied(ticket.job_id);
          if (scope.base !== result.head_sha) {
            this.baseTransition = { from: scope.base, to: result.head_sha };
            void this.router.navigate([], { relativeTo: this.route, queryParams: { commit: result.head_sha }, queryParamsHandling: 'merge', replaceUrl: true })
              .then(ok => { if (!ok && generation === this.savedGeneration) this.saveApplyError.set('השמירה אומתה. כתובת הגרסה לא עודכנה; יש להישאר בעורך ולשמור עותק לפני רענון.'); })
              .catch(() => { if (generation === this.savedGeneration) this.saveApplyError.set('השמירה אומתה, אך עדכון כתובת הגרסה נכשל. העריכה המקומית נשמרה.'); });
          }
        } catch {
          this.loading.set(false); this.applyingSave.set(false);
          this.saveApplyError.set('המקור שהתקבל אינו תואם לשמירה המאומתת. הטקסט המקומי לא הוחלף; יש לבדוק שוב את הגרסה.');
        }
      },
      error: error => {
        if (generation !== this.savedGeneration) return;
        this.loading.set(false); this.applyingSave.set(false);
        this.saveApplyError.set(this.message(error, 'השמירה אומתה אך המקור החדש טרם נטען. העריכה המקומית לא הוחלפה.'));
        if (error instanceof HttpErrorResponse && [403, 404, 409].includes(error.status)) this.accessReady.set(false);
        if (error instanceof HttpErrorResponse && (error.status === 401 || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED')) this.session.refresh();
      },
    });
  }

  restore(): void {
    const remote = this.recovery.remote(), source = this.source();
    if (!remote || !source || !this.flushFields()) return;
    const moved = remote.base_head_sha !== source.snapshot_sha;
    if (!window.confirm(moved
      ? 'עותק ההתאוששות נוצר מגרסה אחרת. לאחר השוואת המקורות, להעביר את הטקסט לגרסה שנבחרה? נדרשת בדיקה לפני שמירה בענף.'
      : 'להחליף את הטקסט המקומי בעותק ההתאוששות המוצג?')) return;
    if (this.recovery.restore(moved)) { this.mode.set('source'); this.richSnapshot.set(null); this.cancelPreview(); }
  }
  keepLocal(): void {
    if (!this.flushFields()) return;
    if (window.confirm(this.recovery.uncertain()
      ? 'ייתכן שהשמירה הקודמת התקבלה. להחליף במפורש את העותק האחרון שנבדק בשרת בטקסט המקומי? שינוי מאוחר יותר יחסום את ההחלפה.'
      : 'להמשיך עם הטקסט המקומי ולהחליף את עותק ההתאוששות המוצג?')) this.recovery.keepLocal();
  }
  backup(): void { if (this.flushFields()) this.recovery.flush(); }
  download(): void {
    if (!this.visible()) return;
    // A failed rich serialization must export the actual editor state, never a
    // stale last-good Markdown buffer under a misleading filename.
    const widget = this.widget();
    if (widget && !widget.flushFields()) {
      const snapshot = widget.snapshot();
      if (!snapshot) return;
      this.saveFile(JSON.stringify({ format: 'specspace-uncommitted-editor-v1', document_id: this.scope?.document,
        base_head_sha: this.scope?.base, base_document_sha256: this.source()?.sha256,
        source_model_base_head_sha: this.richBase, normalize_rich_source: this.canonicalRich, source_model: this.rich()?.model, last_serialized_markdown: this.recovery.content(),
        document: snapshot.state.doc.toJSON(), selection: snapshot.state.selection.toJSON(), pending_link: snapshot.linkOpen ? snapshot.linkUrl : null,
        pending_image_alt: snapshot.imageOpen ? snapshot.imageAlt : null }), 'uncommitted.editor.json', 'application/json');
    } else this.saveFile(this.recovery.content(), 'uncommitted.md', 'text/markdown;charset=utf-8');
  }
  private saveFile(content: string, suffix: string, type: string): void {
    const url = URL.createObjectURL(new Blob([content], { type }));
    const link = document.createElement('a'); link.href = url; link.download = `${this.scope?.document ?? 'document'}.${suffix}`;
    link.click(); setTimeout(() => URL.revokeObjectURL(url), 0);
  }
  canLeave(logout = false): boolean {
    if (!this.owner()) return true;
    if (!this.visible()) return !this.recovery.dirty() && !this.recovery.pending() && !this.git.pending() && !this.brokenRich() && !this.hasPendingFields()
      || window.confirm('יש עריכה מקומית בחשבון שפתח את העורך. לעזוב ולמחוק את העותק המקומי?');
    const flushed = this.flushFields();
    if (!flushed) return window.confirm('יש בעורך שינוי שאינו מיוצג עדיין ב־Markdown. יציאה תמחק אותו. מומלץ לבטל ולהוריד עותק התאוששות. לצאת בכל זאת?');
    if (!this.recovery.dirty() && !this.recovery.pending() && !this.git.pending() && !this.recovery.choiceRequired()) return true;
    return window.confirm(logout ? 'השינויים טרם נשמרו ב־Git. להתנתק ולמחוק את העותק המקומי?'
      : 'השינויים טרם נשמרו ב־Git. לעזוב את העורך? רק עותק התאוששות שאושר בשרת יהיה זמין בהמשך.');
  }
  @HostListener('window:beforeunload', ['$event'])
  beforeUnload(event: BeforeUnloadEvent): void {
    if (this.owner() && (this.recovery.dirty() || this.recovery.pending() || this.git.pending() || this.brokenRich() || this.hasPendingFields())) {
      event.preventDefault(); event.returnValue = '';
    }
  }
  private hasPendingFields(): boolean {
    return this.widget()?.hasPendingFields() ?? !!(this.richSnapshot()?.linkOpen || this.richSnapshot()?.imageOpen);
  }
  private cancelPreview(): void {
    ++this.previewGeneration; this.previewRequest?.unsubscribe(); this.preview.set(null); this.previewing.set(false); this.previewError.set('');
  }
  private message(error: unknown, fallback: string): string {
    const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
    return typeof message === 'string' ? message : fallback;
  }
  private clear(): void {
    ++this.generation; this.loadingRequest?.unsubscribe(); this.cancelPreview(); this.recovery.clear(); this.git.clear();
    ++this.savedGeneration; this.savedSourceRequest?.unsubscribe(); this.applyingSave.set(false); this.saveApplyError.set('');
    this.attemptedJob = ''; this.canonicalRich = false; this.richBase = null; this.baseTransition = null; this.commitMessage.set('עדכון מסמך האפיון');
    this.scope = null; this.key = ''; this.active = false; this.owner.set(null); this.source.set(null); this.rich.set(null);
    this.richSnapshot.set(null); this.loading.set(false); this.accessReady.set(false); this.error.set('');
    this.mode.set('source'); this.brokenRich.set(false); this.focused = '';
  }
  ngOnDestroy(): void { this.removeLogoutGuard(); this.clear(); }
}
