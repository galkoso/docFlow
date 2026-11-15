import { inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription } from 'rxjs';
import { JobStatus, ProposalApi, ProposalChoices, ProposalDetail, ProposalFilters, ProposalSummary } from '../../core/api/proposal-api';

function message(error: unknown): string {
  const text: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
  return typeof text === 'string' ? text : 'הטעינה לא הושלמה. אפשר לנסות שוב.';
}
function denied(error: unknown): boolean { return error instanceof HttpErrorResponse && [401, 403, 404].includes(error.status); }

@Injectable()
export class ProposalListState {
  private readonly api = inject(ProposalApi);
  private request?: Subscription;
  private optionsRequest?: Subscription;
  private generation = 0;
  private optionsGeneration = 0;
  readonly rows = signal<ProposalSummary[]>([]);
  readonly nextCursor = signal<string | null>(null);
  readonly choices = signal<ProposalChoices | null>(null);
  readonly loading = signal(false);
  readonly loadingChoices = signal(false);
  readonly error = signal<string | null>(null);
  readonly choicesError = signal<string | null>(null);

  clear(): void {
    ++this.generation; ++this.optionsGeneration;
    this.request?.unsubscribe(); this.optionsRequest?.unsubscribe();
    this.rows.set([]); this.nextCursor.set(null); this.choices.set(null);
    this.loading.set(false); this.loadingChoices.set(false); this.error.set(null); this.choicesError.set(null);
  }
  load(filters: ProposalFilters): void {
    this.request?.unsubscribe();
    const generation = ++this.generation;
    this.rows.set([]); this.nextCursor.set(null); this.error.set(null); this.loading.set(false);
    if (Array.from(filters.q ?? '').length > 200) { this.error.set('אפשר לחפש עד 200 תווים.'); return; }
    this.loading.set(true);
    this.request = this.api.list(filters).subscribe({
      next: page => {
        if (generation !== this.generation) return;
        this.rows.set(page.proposals); this.nextCursor.set(page.next_cursor ?? null); this.loading.set(false);
      },
      error: error => {
        if (generation !== this.generation) return;
        if (denied(error)) this.clear();
        this.loading.set(false); this.error.set(message(error));
      },
    });
  }
  loadChoices(): void {
    this.optionsRequest?.unsubscribe();
    const generation = ++this.optionsGeneration;
    this.loadingChoices.set(true); this.choicesError.set(null);
    this.optionsRequest = this.api.choices().subscribe({
      next: choices => {
        if (generation !== this.optionsGeneration) return;
        this.choices.set(choices); this.loadingChoices.set(false);
      },
      error: error => {
        if (generation !== this.optionsGeneration) return;
        if (denied(error)) this.clear();
        this.loadingChoices.set(false); this.choicesError.set(message(error));
      },
    });
  }
}

@Injectable()
export class ProposalDetailState {
  private readonly api = inject(ProposalApi);
  private requests = new Subscription();
  private timer?: ReturnType<typeof setTimeout>;
  private generation = 0;
  readonly detail = signal<ProposalDetail | null>(null);
  readonly job = signal<JobStatus | null>(null);
  readonly loading = signal(false);
  readonly polling = signal(false);
  readonly error = signal<string | null>(null);
  readonly jobError = signal<string | null>(null);

  clear(): void {
    ++this.generation; this.requests.unsubscribe(); this.requests = new Subscription(); clearTimeout(this.timer);
    this.detail.set(null); this.job.set(null); this.loading.set(false); this.polling.set(false); this.error.set(null); this.jobError.set(null);
  }
  load(id: string): void {
    this.open(id, 0);
  }
  private open(id: string, readyReloads: number): void {
    this.clear();
    const generation = this.generation;
    this.loading.set(true);
    this.requests.add(this.api.detail(id).subscribe({
      next: detail => {
        if (generation !== this.generation) return;
        if (detail.proposal_id !== id) { this.loading.set(false); this.error.set('פרטי ההצעה אינם תואמים לבקשה.'); return; }
        this.detail.set(detail); this.loading.set(false);
        if (detail.state === 'PROVISIONING' && detail.creation_job_id) this.poll(id, detail.creation_job_id, generation, 0, readyReloads);
      },
      error: error => {
        if (generation !== this.generation) return;
        this.loading.set(false); this.error.set(message(error));
      },
    }));
  }
  private poll(proposal: string, job: string, generation: number, checks: number, readyReloads: number): void {
    this.polling.set(true);
    this.requests.add(this.api.job(job).subscribe({
      next: status => {
        if (generation !== this.generation) return;
        if (status.job_id !== job || status.proposal_id !== proposal) {
          this.polling.set(false); this.jobError.set('מצב פעולת ההכנה אינו תואם להצעה.'); return;
        }
        this.job.set(status);
        if (status.state === 'SUCCEEDED') {
          if (readyReloads) { this.polling.set(false); this.jobError.set('פרטי ההצעה עדיין אינם תואמים לתוצאת ההכנה. יש לבדוק שוב.'); }
          else this.open(proposal, 1);
          return;
        }
        if (['FAILED', 'CANCELLED'].includes(status.state)) { this.polling.set(false); return; }
        if (checks >= 90) { this.polling.set(false); this.jobError.set('ההכנה נמשכת זמן רב. אפשר לבדוק שוב בעוד רגע.'); return; }
        this.timer = setTimeout(() => {
          if (generation === this.generation) this.poll(proposal, job, generation, checks + 1, readyReloads);
        }, globalThis.document?.visibilityState === 'hidden' ? 10000 : 2000);
      },
      error: error => {
        if (generation !== this.generation) return;
        if (denied(error)) { this.clear(); this.error.set(message(error)); }
        else { this.polling.set(false); this.jobError.set(message(error)); }
      },
    }));
  }
}
