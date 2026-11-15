import { ChangeDetectionStrategy, Component, computed, effect, inject, OnDestroy, signal, untracked } from '@angular/core';
import { DatePipe } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { ProposalFilters } from '../../core/api/proposal-api';
import { ProposalListState } from './proposal-state';
import { preparationError, proposalStates, stateLabel } from './proposal-labels';

@Component({
  selector: 'app-proposals', imports: [DatePipe, RouterLink], providers: [ProposalListState],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './proposals.html', styleUrl: './proposals.css',
})
export class Proposals implements OnDestroy {
  readonly session = inject(SessionState);
  readonly state = inject(ProposalListState);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  readonly params = toSignal(this.route.queryParamMap, { initialValue: this.route.snapshot.queryParamMap });
  readonly searchDraft = signal('');
  readonly states = proposalStates;
  readonly stateLabel = stateLabel;
  readonly preparationError = preparationError;
  private lastProfile: unknown;
  private timer?: ReturnType<typeof setTimeout>;
  readonly filters = computed<ProposalFilters>(() => {
    const params = this.params();
    return Object.fromEntries(['repository_id', 'state', 'owner_id', 'team_id', 'q', 'cursor'].map(key => [key, params.get(key) ?? undefined]));
  });
  readonly hasFilters = computed(() => Object.entries(this.filters()).some(([key, value]) => key !== 'cursor' && !!value));
  readonly canCreate = computed(() => this.state.choices()?.repositories.some(item => item.can_create
    && (!this.filters().repository_id || this.filters().repository_id === item.repository_id)) ?? false);
  readonly teams = computed(() => this.state.choices()?.teams.filter(item => !this.filters().repository_id
    || item.repository_ids.includes(this.filters().repository_id!)) ?? []);
  readonly owners = computed(() => this.state.choices()?.owners.filter(item => !this.filters().repository_id
    || item.repository_ids.includes(this.filters().repository_id!)) ?? []);

  constructor() {
    effect(() => {
      const profile = this.session.profile();
      const filters = this.filters();
      untracked(() => {
        clearTimeout(this.timer);
        if (profile !== this.lastProfile) {
          this.lastProfile = profile; this.state.clear();
          if (profile) this.state.loadChoices();
        }
        this.searchDraft.set(filters.q ?? '');
        if (profile) this.state.load(filters); else this.state.clear();
      });
    });
  }
  search(value: string): void {
    this.searchDraft.set(value); clearTimeout(this.timer);
    this.timer = setTimeout(() => { void this.router.navigate([], { relativeTo: this.route,
      queryParams: { q: value || null, cursor: null }, queryParamsHandling: 'merge' }); }, 200);
  }
  filter(key: 'repository_id' | 'state' | 'owner_id' | 'team_id', value: string): void {
    clearTimeout(this.timer);
    void this.router.navigate([], { relativeTo: this.route, queryParamsHandling: 'merge', queryParams: {
      [key]: value || null, q: this.searchDraft() || null, cursor: null, ...(key === 'repository_id' ? { owner_id: null, team_id: null } : {}),
    } });
  }
  reset(): void { clearTimeout(this.timer); void this.router.navigate([], { relativeTo: this.route, queryParams: {} }); }
  next(): void { void this.router.navigate([], { relativeTo: this.route, queryParams: { cursor: this.state.nextCursor() }, queryParamsHandling: 'merge' }); }
  first(): void { void this.router.navigate([], { relativeTo: this.route, queryParams: { cursor: null }, queryParamsHandling: 'merge' }); }
  refresh(): void { this.state.loadChoices(); this.state.load(this.filters()); }
  ngOnDestroy(): void { clearTimeout(this.timer); this.state.clear(); }
}
