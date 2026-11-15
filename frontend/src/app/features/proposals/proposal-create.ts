import { ChangeDetectionStrategy, Component, computed, effect, HostListener, inject, OnDestroy, untracked } from '@angular/core';
import { DatePipe } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { ProposalCreateState } from './proposal-create-state';

@Component({
  selector: 'app-proposal-create', imports: [DatePipe, RouterLink], providers: [ProposalCreateState],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './proposal-create.html', styleUrl: './proposal-create.css',
})
export class ProposalCreate implements OnDestroy {
  readonly session = inject(SessionState);
  readonly state = inject(ProposalCreateState);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private readonly params = toSignal(this.route.queryParamMap, { initialValue: this.route.snapshot.queryParamMap });
  private readonly repository = computed(() => this.params().get('repo') ?? undefined);
  readonly linkFields = [['figma', 'Figma'], ['ticket', 'טיקט'], ['development_pr', 'PR פיתוח']] as const;
  constructor() {
    effect(() => {
      const profile = this.session.profile(), repository = this.repository();
      untracked(() => this.state.scope(profile?.user_id, repository));
    });
    effect(() => {
      const ticket = this.state.ticket(), profile = this.session.profile();
      if (ticket && profile && this.state.ticketOwner() === profile.user_id) untracked(() => {
        void this.router.navigate(['/changes', ticket.proposal_id], { queryParams: { mode: this.state.form().initial_mode } });
      });
    });
    effect(() => {
      if (this.state.authRequired()) untracked(() => { this.state.authRequired.set(false); this.session.refresh(); });
    });
  }
  selectRepository(value: string): void {
    if (this.state.locked()) return;
    this.state.update('repository_id', value);
    void this.router.navigate([], { relativeTo: this.route, queryParams: { repo: value || null }, queryParamsHandling: 'merge' });
  }
  refreshChoices(): void { if (!this.state.locked()) this.state.scope(this.session.profile()?.user_id, this.repository()); }
  startFresh(): void {
    if (window.confirm('ייתכן שבקשת היצירה הקודמת כבר התקבלה. מומלץ לבדוק את רשימת ההצעות. להכין טופס חדש עם מספר אחר?')) this.state.abandonUncertain();
  }
  canLeave(): boolean {
    if (this.state.ticket() || (!this.state.dirty() && !this.state.uncertain() && !this.state.submitting())) return true;
    return window.confirm(this.state.uncertain() || this.state.submitting()
      ? 'ייתכן שפעולת היצירה ממשיכה בשרת. לעזוב את הטופס ולוותר על נתוני ההתאוששות המקומיים?'
      : 'פרטי הטופס טרם נשמרו. לעזוב ולוותר על השינויים?');
  }
  @HostListener('window:beforeunload', ['$event'])
  beforeUnload(event: BeforeUnloadEvent): void {
    if (!this.state.ticket() && (this.state.dirty() || this.state.uncertain() || this.state.submitting())) { event.preventDefault(); event.returnValue = ''; }
  }
  ngOnDestroy(): void { this.state.dispose(); }
}
