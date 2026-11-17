import { ChangeDetectionStrategy, Component, computed, effect, inject, OnDestroy, signal, untracked } from '@angular/core';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { ComparisonState } from './comparison-state';
import { digest, uuid } from './comparison-evidence';
import { comparisonLocation } from './comparison-location';
import { ComparisonImage } from './image-view';
import { ReadableView } from './readable-view';
import { RawScope, RawState } from './raw-state';

@Component({
  selector: 'app-comparison-page', imports: [RouterLink, ReadableView, ComparisonImage], providers: [ComparisonState, RawState],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './comparison-page.html', styleUrl: './comparison-page.css',
})
export class ComparisonPage implements OnDestroy {
  readonly session = inject(SessionState);
  readonly state = inject(ComparisonState);
  readonly raw = inject(RawState);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly params = toSignal(this.route.paramMap, { initialValue: this.route.snapshot.paramMap });
  private readonly query = toSignal(this.route.queryParamMap, { initialValue: this.route.snapshot.queryParamMap });
  private lastActor: string | null = null;
  private seenLogout = this.session.logoutEpoch();
  private blocked = false;
  private routeKey = '';
  private freshIntent = false;
  readonly location = computed(() => comparisonLocation(this.query()));
  readonly unit = computed(() => this.location()?.file === this.state.selected()?.id ? this.location()?.unit ?? null : null);
  readonly invalidUnitKind = computed(() => !!this.unit() && !['document', 'catalog'].includes(this.state.selected()?.kind ?? ''));
  readonly routeError = signal('');
  readonly showEqual = signal(true);
  readonly mode = signal<'readable' | 'raw'>('readable');
  readonly fileScope = computed<RawScope | null>(() => {
    const response = this.state.data(), selected = this.state.selected(), actor = this.session.profile()?.user_id;
    return actor && actor === this.state.owner() && response?.summary && selected && !this.invalidUnitKind() ? {
      actor, proposal: response.proposal_id, snapshot: response.snapshot_id, content: response.summary.content_id,
      head: response.summary.head_sha, base: response.summary.merge_base_sha, file: selected,
    } : null;
  });
  readonly proposal = computed(() => this.params().get('id'));
  readonly visible = computed(() => !!this.session.profile() && this.session.profile()?.user_id === this.state.owner());
  readonly segments = computed(() => this.raw.data()?.raw.segments.filter(segment => this.showEqual() || segment.kind !== 'equal') ?? []);
  constructor() {
    effect(() => {
      const actor = this.session.profile()?.user_id ?? null, logout = this.session.logoutEpoch();
      const proposal = this.params().get('id'), query = this.query();
      untracked(() => {
        if (logout !== this.seenLogout) { this.seenLogout = logout; this.blocked = true; this.state.clear(); this.raw.clear(); }
        if (actor && this.lastActor && actor !== this.lastActor) this.blocked = true;
        this.state.identity(actor);
        if (!actor) { this.raw.clear(); return; }
        this.lastActor = actor;
        if (this.blocked) return;
        const location = comparisonLocation(query);
        if (!proposal || !uuid(proposal) || !location) {
          this.routeError.set('כתובת ההשוואה אינה תקינה.'); this.routeKey = ''; this.state.clear(); return;
        }
        this.mode.set(location.view); this.routeError.set('');
        const { snapshot, request, head, target, offset, file } = location;
        const key = JSON.stringify([proposal, snapshot, request, head, target, offset, file]);
        if (key === this.routeKey) return;
        this.routeKey = key; this.freshIntent = false;
        if (snapshot && file) this.state.openFile(actor, proposal, snapshot, file);
        else if (snapshot) { this.state.selected.set(null); this.state.openSnapshot(actor, proposal, snapshot, offset); }
        else this.state.start(actor, proposal, { ...(request ? { request_id: request } : {}), ...(head ? { head } : {}), ...(target ? { target } : {}) });
      });
    });
    effect(() => {
      const snapshot = this.state.snapshot(), request = this.state.requestId(), profile = this.session.profile();
      untracked(() => {
        if (!profile || profile.user_id !== this.state.owner() || this.blocked) return;
        if (snapshot && this.query().get('snapshot') !== snapshot) void this.router.navigate([], { relativeTo: this.route,
          queryParams: { snapshot, request_id: null, head: null, target: null, file: null, unit: null, offset: this.freshIntent ? null : this.query().get('offset') },
          queryParamsHandling: 'merge', replaceUrl: true });
        else if (!snapshot && request && this.query().get('request_id') !== request) void this.router.navigate([], { relativeTo: this.route,
          queryParams: { request_id: request, snapshot: null, head: this.freshIntent ? null : this.query().get('head'),
            target: this.freshIntent ? null : this.query().get('target'), offset: null, file: null, unit: null }, queryParamsHandling: 'merge', replaceUrl: true });
      });
    });
    effect(() => {
      const scope = this.fileScope(), mode = this.mode();
      untracked(() => this.raw.open(mode === 'raw' && scope && ['document', 'catalog'].includes(scope.file.kind) ? scope : null));
    });
  }
  fresh(): void {
    const actor = this.session.profile()?.user_id, proposal = this.proposal();
    if (!actor || !proposal) return;
    this.lastActor = actor; this.freshIntent = true; this.blocked = false; this.routeError.set(''); this.state.clear(); this.raw.clear(); this.routeKey = '';
    this.state.start(actor, proposal);
  }
  previousFiles(offset: number): void { this.files(Math.max(0, offset - 50)); }
  label(value: string): string {
    return ({ QUEUED: 'בתור', LOADING: 'קריאת גרסאות', PUBLISHING: 'שמירת תוצאה', RETRY_WAIT: 'ממתינים לניסיון נוסף',
      added: 'נוסף', deleted: 'נמחק', modified: 'השתנה', renamed: 'שינוי שם או מיקום',
      document: 'מסמך', catalog: 'סדר הספר', asset: 'מדיה', other: 'קובץ' } as Record<string, string>)[value] ?? value;
  }
  select(id: string): void {
    if (!digest(id) || !this.state.select(id)) return;
    void this.router.navigate([], { relativeTo: this.route, queryParams: { snapshot: this.state.snapshot(), file: id, unit: null,
      offset: null, request_id: null, head: null, target: null, view: this.mode() }, queryParamsHandling: 'merge', replaceUrl: false });
  }
  selectUnit(id: string | null): void {
    if (id != null && !digest(id) || !this.state.selected()) return;
    void this.router.navigate([], { relativeTo: this.route, queryParams: { unit: id }, queryParamsHandling: 'merge', replaceUrl: false });
  }
  setMode(view: 'readable' | 'raw'): void {
    this.mode.set(view);
    void this.router.navigate([], { relativeTo: this.route, queryParams: { view, ...(view === 'raw' ? { unit: null } : {}) }, queryParamsHandling: 'merge', replaceUrl: false });
  }
  clearFile(): void {
    void this.router.navigate([], { relativeTo: this.route, queryParams: { file: null, unit: null, offset: 0 },
      queryParamsHandling: 'merge', replaceUrl: false });
  }
  readonly selectionHref = computed(() => {
    const file = this.fileScope(); if (!file) return null;
    const query = new URLSearchParams({ snapshot: file.snapshot, file: file.file.id, view: this.mode() });
    if (this.mode() === 'readable' && this.unit()) query.set('unit', this.unit()!);
    return `/changes/${file.proposal}/diff?${query}`;
  });
  files(offset: number): void {
    this.state.files(offset);
    if (!this.state.selected()) void this.router.navigate([], { relativeTo: this.route, queryParams: { offset },
      queryParamsHandling: 'merge', replaceUrl: true });
  }
  ngOnDestroy(): void { this.state.clear(); this.raw.clear(); }
}
