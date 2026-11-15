import { inject, Injectable } from '@angular/core';
import { HttpClient, HttpParams } from '@angular/common/http';
import type { components } from './generated/schema';

export type ProposalSummary = components['schemas']['ProposalSummary'];
export type ProposalPage = components['schemas']['ProposalPage'];
export type ProposalDetail = components['schemas']['ProposalDetail'];
export type ProposalChoices = components['schemas']['ProposalChoices'];
export type JobStatus = components['schemas']['JobStatus'];
export type PreparedCreation = components['schemas']['PreparedCreation'];
export type CreateProposal = components['schemas']['CreateProposal'];
export type CreationAccepted = components['schemas']['CreationAccepted'];
export interface ProposalFilters { repository_id?: string; state?: string; owner_id?: string; team_id?: string; q?: string; cursor?: string; }

@Injectable({ providedIn: 'root' })
export class ProposalApi {
  private readonly http = inject(HttpClient);
  list(filters: ProposalFilters) {
    let params = new HttpParams().set('limit', '50');
    for (const [name, value] of Object.entries(filters)) if (value) params = params.set(name, value);
    return this.http.get<ProposalPage>('/api/v1/proposals', { params });
  }
  choices() { return this.http.get<ProposalChoices>('/api/v1/proposals/options'); }
  prepare(repository: string, key: string) {
    return this.http.post<PreparedCreation>('/api/v1/proposals/context', { repository_id: repository }, { headers: { 'Idempotency-Key': key } });
  }
  create(body: CreateProposal, key: string) {
    return this.http.post<CreationAccepted>('/api/v1/proposals', body, { headers: { 'Idempotency-Key': key } });
  }
  detail(id: string) { return this.http.get<ProposalDetail>(`/api/v1/proposals/${encodeURIComponent(id)}`); }
  job(id: string) { return this.http.get<JobStatus>(`/api/v1/jobs/${encodeURIComponent(id)}`); }
}
