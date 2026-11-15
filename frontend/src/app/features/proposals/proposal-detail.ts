import { ChangeDetectionStrategy, Component, effect, inject, OnDestroy, untracked } from '@angular/core';
import { DatePipe } from '@angular/common';
import { ActivatedRoute, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { ProposalDetailState } from './proposal-state';
import { preparationError, stateLabel } from './proposal-labels';

@Component({
  selector: 'app-proposal-detail', imports: [DatePipe, RouterLink], providers: [ProposalDetailState],
  changeDetection: ChangeDetectionStrategy.OnPush, templateUrl: './proposal-detail.html', styleUrl: './proposal-detail.css',
})
export class ProposalDetailPage implements OnDestroy {
  readonly session = inject(SessionState);
  readonly state = inject(ProposalDetailState);
  private readonly route = inject(ActivatedRoute);
  private readonly params = toSignal(this.route.paramMap, { initialValue: this.route.snapshot.paramMap });
  readonly stateLabel = stateLabel;
  readonly preparationError = preparationError;
  constructor() {
    effect(() => {
      const profile = this.session.profile(); const id = this.params().get('id');
      untracked(() => { if (profile && id) this.state.load(id); else this.state.clear(); });
    });
  }
  refresh(): void { const id = this.params().get('id'); if (id && this.session.profile()) this.state.load(id); }
  ngOnDestroy(): void { this.state.clear(); }
}
