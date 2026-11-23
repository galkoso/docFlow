import { ChangeDetectionStrategy, Component, computed, effect, HostListener, inject, OnDestroy, signal, untracked, viewChild } from '@angular/core';
import { DatePipe } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { stateLabel } from '../proposals/proposal-labels';
import { ReviewSubmission } from './review-submission';
import { SharedReviewBody } from './shared-review-body';
import { ReviewUnitsView } from './review-units-view';
import { ReviewPrStatusView } from './review-pr-status';
import { OfficialReviewsView } from './official-reviews-view';
import { DiscussionsView, type DiscussionScope } from './discussions-view';
import type { RawScope } from '../comparison/raw-state';
import { SharedReviewState } from './shared-review-state';
import { reviewLocation, type ReviewPageLocation } from './review-location';

@Component({
  selector: 'app-review-snapshots', imports: [RouterLink, DatePipe, SharedReviewBody, ReviewPrStatusView, OfficialReviewsView, DiscussionsView, ReviewUnitsView],
  providers: [ReviewSubmission, SharedReviewState],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './review-snapshots-page.html', styleUrl: './review-snapshots-page.css',
})
export class ReviewSnapshotsPage implements OnDestroy {
  private readonly official = viewChild(OfficialReviewsView);
  private readonly discussions = viewChild(DiscussionsView);
  private readonly requirements = viewChild(ReviewUnitsView);
  readonly sourceDomains = computed(() => this.official()?.view()?.domains ?? []);
  readonly session = inject(SessionState);
  readonly submission = inject(ReviewSubmission);
  readonly state = inject(SharedReviewState);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly params = toSignal(this.route.paramMap, { initialValue: this.route.snapshot.paramMap });
  private readonly query = toSignal(this.route.queryParamMap, { initialValue: this.route.snapshot.queryParamMap });
  private lastActor: string | null = null;
  private seenLogout = this.session.logoutEpoch();
  private scopeKey = '';
  private routeKey = '';
  private applied = '';
  readonly blocked = signal(false);
  readonly routeError = signal('');
  readonly proposal = computed(() => this.params().get('id'));
  readonly location = computed(() => reviewLocation(this.query()));
  readonly visible = computed(() => !this.blocked() && !!this.session.profile() && this.session.profile()?.user_id === this.state.owner()
    && !this.submission.authRequired() && !this.submission.accessDenied() && !this.state.authRequired());
  readonly current = computed(() => !!this.state.snapshot()?.current_metadata && this.state.snapshot()?.snapshot_id === this.state.history()?.active_snapshot_id);
  readonly mismatched = computed(() => {
    const result = this.submission.result(), snapshot = this.state.snapshot();
    return !!result && !!snapshot && result.snapshot_id === snapshot.snapshot_id
      && (result.fingerprint !== snapshot.fingerprint || result.evidence_digest !== snapshot.evidence_digest);
  });
  readonly selectedFile = computed(() => this.state.files()?.files.find(file => file.id === this.location()?.file) ?? null);
  readonly fileScope = computed<RawScope | null>(() => {
    const actor = this.session.profile()?.user_id, snapshot = this.state.snapshot(), file = this.selectedFile(), proposal = this.proposal();
    if (!this.visible() || this.mismatched() || this.state.phase() !== 'ready' || !actor || !proposal || !snapshot || !file) return null;
    const summary = snapshot.metadata.summary;
    return { actor, proposal, snapshot: snapshot.snapshot_id, content: summary.comparison_content_id, head: summary.head_sha, base: summary.merge_base_sha, file };
  });
  readonly stateLabel = stateLabel;
  readonly discussionScope = computed<DiscussionScope | null>(() => {
    const actor = this.session.profile()?.user_id, proposal = this.proposal();
    return this.visible() && this.state.phase() === 'ready' && actor && proposal
      ? { actor, proposal, snapshot: this.state.snapshot(), file: this.selectedFile() } : null;
  });
  private readonly releaseLogout = this.session.guardLogout(() => this.canLeave());
  constructor() {
    effect(() => {
      const actor = this.session.profile()?.user_id ?? null, proposal = this.params().get('id'), query = this.query(), logout = this.session.logoutEpoch();
      untracked(() => {
        if (logout !== this.seenLogout) { this.seenLogout = logout; this.blocked.set(true); this.submission.clear(); this.state.clear(); }
        if (actor && this.lastActor && actor !== this.lastActor) { this.blocked.set(true); this.submission.clear(); this.state.clear(); }
        if (!actor) { this.state.clear(); this.submission.identity(null); this.routeKey = ''; return; }
        this.lastActor = actor;
        if (this.blocked() || !proposal) return;
        this.open(actor, proposal, reviewLocation(query));
      });
    });
    effect(() => {
      const result = this.submission.result(), job = this.submission.jobId(), actor = this.session.profile()?.user_id;
      untracked(() => {
        if (!actor || actor !== this.submission.owner() || this.blocked()) return;
        const key = result ? `${result.job_id}:${result.snapshot_id}` : '';
        if (result && this.applied !== key) {
          this.applied = key; this.navigate({ job: result.job_id, snapshot: result.snapshot_id, files: null, media: null, requirements: null, file: null, mode: null, unit: null }, true);
        } else if (job && this.query().get('job') !== job) this.navigate({ job }, true);
      });
    });
    effect(() => {
      const snapshot = this.state.snapshot(), phase = this.state.phase(), actor = this.session.profile()?.user_id;
      untracked(() => {
        const location = this.location(), proposal = this.proposal();
        if (!snapshot || phase !== 'ready' || !actor || actor !== this.state.owner() || this.blocked() || !location || !proposal || location.snapshot) return;
        this.routeKey = this.key(actor, proposal, { ...location, snapshot: snapshot.snapshot_id });
        this.navigate({ snapshot: snapshot.snapshot_id }, true);
      });
    });
    effect(() => {
      const required = this.submission.authRequired() || this.submission.accessDenied() || this.state.authRequired();
      untracked(() => { if (required) { this.blocked.set(true); this.state.clear(); this.submission.identity(null); this.routeKey = ''; } });
    });
  }
  private key(actor: string, proposal: string, location: ReviewPageLocation): string {
    return JSON.stringify([actor, proposal, location.snapshot ?? null, location.files, location.media, location.after ?? null]);
  }
  private open(actor: string, proposal: string, location: ReviewPageLocation | null): void {
    const scope = JSON.stringify([actor, proposal]);
    if (scope !== this.scopeKey) { this.submission.clear(); this.state.clear(); this.scopeKey = scope; this.routeKey = ''; this.applied = ''; }
    this.submission.identity(actor);
    if (!location) { this.routeError.set('כתובת גרסת הסקירה אינה תקינה.'); this.state.clear(); this.routeKey = ''; return; }
    this.routeError.set('');
    const key = this.key(actor, proposal, location);
    if (key !== this.routeKey) { this.routeKey = key; this.state.open(actor, proposal, location); }
    if (location.job && location.job !== this.submission.jobId() && !this.submission.pending() && !this.submission.busy()) {
      this.submission.restore(actor, proposal, location.job);
    }
  }
  reload(): void {
    if ((this.official()?.hasDraft() || this.discussions()?.hasDraft() || this.requirements()?.hasPending()) && !this.canLeave()) return;
    const actor = this.session.profile()?.user_id, proposal = this.proposal();
    if (!actor || !proposal) return;
    let location = this.location();
    if (this.submission.accessDenied()) {
      this.submission.clear(); this.applied = ''; this.navigate({ job: null }, true);
      if (location) location = { ...location, job: undefined };
    }
    this.lastActor = actor; this.blocked.set(false); this.routeKey = ''; this.open(actor, proposal, location);
  }
  prepare(): void {
    const actor = this.session.profile()?.user_id, proposal = this.proposal();
    if (!actor || !proposal || this.blocked() || this.submission.pending() || this.submission.busy()) return;
    this.applied = ''; this.navigate({ job: null }, true); this.submission.prepare(actor, proposal);
  }
  send(): void { if (this.ownsSubmission()) this.submission.start(); }
  retry(): void { if (this.ownsSubmission()) this.submission.retry(); }
  check(): void { if (this.ownsSubmission()) this.submission.check(); }
  private ownsSubmission(): boolean {
    return !this.blocked() && !!this.session.profile() && this.session.profile()?.user_id === this.submission.owner();
  }
  select(snapshot: string): void { this.navigate({ snapshot, files: null, media: null, requirements: null, file: null, mode: null, unit: null }); }
  filePage(offset: number): void { this.navigate({ snapshot: this.state.selected(), files: offset, file: null, mode: null, unit: null }); }
  selectFile(file: string): void {
    if (this.visible() && this.state.files()?.files.some(item => item.id === file)) this.navigate({ snapshot: this.state.selected(), file, mode: null, unit: null });
  }
  setMode(mode: 'raw' | 'readable'): void { if (this.fileScope()) this.navigate({ mode, unit: null }); }
  selectUnit(unit: string | null): void { if (this.fileScope()) this.navigate({ unit }); }
  bodyDenied(): void { this.blocked.set(true); this.state.clear(); this.routeKey = ''; }
  decisionRecorded(): void {
    const actor = this.session.profile()?.user_id, proposal = this.proposal();
    if (actor && proposal && this.visible()) this.state.refreshHistory(actor, proposal);
  }
  individualApprovalRecorded(): void { if (this.visible()) this.official()?.refreshAfterUnitApproval(); }
  requirementPage(offset: number): void { this.navigate({ snapshot: this.state.selected(), requirements: offset }); }
  mediaPage(offset: number): void { this.navigate({ snapshot: this.state.selected(), media: offset }); }
  historyPage(after: string | null): void { this.navigate({ after }); }
  previous(offset: number): number { return Math.max(0, offset - 50); }
  private navigate(params: Record<string, string | number | null>, replaceUrl = false): void {
    void this.router.navigate([], { relativeTo: this.route, queryParams: params, queryParamsHandling: 'merge', replaceUrl });
  }
  canNavigate(url?: string): boolean {
    if (!url || url.split('?')[0] !== this.router.url.split('?')[0]) return this.canLeave();
    const next = reviewLocation(this.router.parseUrl(url).queryParamMap), current = this.location();
    const selected = next?.snapshot ?? this.state.history()?.active_snapshot_id ?? this.state.history()?.snapshots[0]?.snapshot_id;
    if (next && current && selected === this.state.snapshot()?.snapshot_id && next.files === current.files && next.media === current.media
      && (next.after ?? '') === (current.after ?? '')) {
      if (next.file !== current.file && this.discussions()?.hasDraft()) return this.canLeave();
      return true;
    }
    return this.canLeave();
  }
  phaseLabel(phase: string): string {
    return ({ checking: 'בודקים את הגרסה השמורה', prepared: 'מוכנה לשליחה', sending: 'שולחים את הבקשה', observing: 'בודקים את העבודה הקיימת', uncertain: 'תוצאת השליחה טרם אומתה',
      queued: 'הבקשה בתור', preparing: 'בודקים תוכן ותמונות ומכינים גרסת סקירה', 'retry-wait': 'ממתינים לניסיון נוסף',
      reconciling: 'נדרש בירור תוצאת העבודה', completed: 'גרסה משותפת זמינה לסקירה', failed: 'הגרסה לא הוכנה',
      suspended: 'הבדיקה מושהית עד לאימות התחברות', error: 'נדרשת בדיקה נוספת' } as Record<string, string>)[phase] ?? phase;
  }
  kind(value: string): string { return ({ document: 'מסמך', catalog: 'סדר הספר', asset: 'תמונה', other: 'קובץ' } as Record<string, string>)[value] ?? value; }
  change(value: string): string { return ({ added: 'נוסף', deleted: 'נמחק', modified: 'השתנה', renamed: 'שינוי שם או מיקום' } as Record<string, string>)[value] ?? value; }
  findingLabel(value: string): string {
    return ({ IMAGE_ALT_REQUIRED: 'חסר תיאור חלופי לתמונה', UNSUPPORTED_HTML: 'בלוק HTML דורש בדיקה במקור',
      UNSUPPORTED_HEADING_LEVEL: 'רמת הכותרת אינה נתמכת בעורך' } as Record<string, string>)[value] ?? value;
  }
  canLeave(): boolean {
    return !this.submission.pending() && !this.official()?.hasDraft() && !this.discussions()?.hasDraft() && !this.requirements()?.hasPending()
      || window.confirm('הודעה, טיוטה או בקשת סקירה עדיין ממתינה או שתוצאתה אינה ודאית. לעזוב ולבדוק את הגרסאות וההיסטוריה בכניסה הבאה?');
  }
  @HostListener('window:beforeunload', ['$event'])
  beforeUnload(event: BeforeUnloadEvent): void {
    if (this.submission.pending() || this.official()?.hasDraft() || this.discussions()?.hasDraft() || this.requirements()?.hasPending()) { event.preventDefault(); event.returnValue = ''; }
  }
  ngOnDestroy(): void { this.releaseLogout(); this.state.clear(); this.submission.clear(); }
}
