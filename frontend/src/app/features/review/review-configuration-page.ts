import { ChangeDetectionStrategy, Component, computed, effect, HostListener, inject, OnDestroy, signal, untracked } from '@angular/core';
import { FormsModule } from '@angular/forms';
import { ActivatedRoute, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { stateLabel } from '../proposals/proposal-labels';
import type { ReviewArea } from '../../core/api/review-configuration-api';
import { areas } from './review-evidence';
import { ReviewConfigurationState } from './review-configuration-state';

@Component({
  selector: 'app-review-configuration', imports: [FormsModule, RouterLink], providers: [ReviewConfigurationState],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './review-configuration-page.html', styleUrl: './review-configuration-page.css',
})
export class ReviewConfigurationPage implements OnDestroy {
  readonly session = inject(SessionState);
  readonly state = inject(ReviewConfigurationState);
  private readonly route = inject(ActivatedRoute);
  private readonly params = toSignal(this.route.paramMap, { initialValue: this.route.snapshot.paramMap });
  private lastActor: string | null = null;
  private seenLogout = this.session.logoutEpoch();
  private routeKey = '';
  readonly blocked = signal(false);
  readonly areas = areas;
  readonly stateLabel = stateLabel;
  readonly picked = signal<Record<ReviewArea, string>>({ PRODUCT: '', DEVELOPMENT: '', DESIGN: '' });
  readonly proposal = computed(() => this.params().get('id'));
  readonly visible = computed(() => !this.blocked() && !!this.session.profile() && this.session.profile()?.user_id === this.state.owner());
  private readonly releaseLogout = this.session.guardLogout(() => this.canLeave());
  constructor() {
    effect(() => {
      const actor = this.session.profile()?.user_id ?? null, proposal = this.params().get('id'), logout = this.session.logoutEpoch();
      untracked(() => {
        if (logout !== this.seenLogout) { this.seenLogout = logout; this.state.clear(); this.purgePicker(); this.blocked.set(true); }
        if (!actor) { this.state.suspend(); return; }
        if (this.lastActor && this.lastActor !== actor) { this.state.clear(); this.purgePicker(); this.blocked.set(true); }
        this.lastActor = actor;
        if (this.blocked() || !proposal) return;
        const key = JSON.stringify([actor, proposal]);
        if (key !== this.routeKey || this.state.phase() === 'idle') {
          this.routeKey = key; this.purgePicker(); this.state.open(actor, proposal);
        }
      });
    });
  }
  private purgePicker(): void { this.picked.set({ PRODUCT: '', DEVELOPMENT: '', DESIGN: '' }); }
  areaLabel(area: ReviewArea): string { return { PRODUCT: 'מוצר', DEVELOPMENT: 'פיתוח', DESIGN: 'עיצוב' }[area]; }
  choose(area: ReviewArea, id: string): void { this.picked.update(picked => ({ ...picked, [area]: id })); }
  add(area: ReviewArea): void { this.state.add(area, this.picked()[area]); this.choose(area, ''); }
  candidates(area: ReviewArea, direction: 'retry' | 'next' | 'previous'): void { this.choose(area, ''); this.state.candidates(area, direction); }
  reload(): void {
    const actor = this.session.profile()?.user_id, proposal = this.proposal();
    if (!actor || !proposal || this.state.pending() || !this.canLeave()) return;
    this.lastActor = actor; this.blocked.set(false); this.routeKey = JSON.stringify([actor, proposal]);
    this.purgePicker(); this.state.open(actor, proposal);
  }
  canLeave(): boolean {
    if (this.state.pending()) return window.confirm('תוצאת השמירה עדיין אינה ודאית. לעזוב ולבדוק את ההגדרה השמורה בכניסה הבאה?');
    return !this.state.dirty() || window.confirm('יש שינויים בהגדרת הסקירה שטרם נשמרו. לעזוב ולמחוק את העותק המקומי?');
  }
  @HostListener('window:beforeunload', ['$event'])
  beforeUnload(event: BeforeUnloadEvent): void {
    if (this.state.dirty() || this.state.pending()) { event.preventDefault(); event.returnValue = ''; }
  }
  ngOnDestroy(): void { this.releaseLogout(); this.state.clear(); this.purgePicker(); }
}
