import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type ReviewConfiguration = components['schemas']['ReviewConfiguration'];
export type ReviewConfigurationRequest = components['schemas']['ReviewConfigurationRequest'];
export type ConfigurationUpdated = components['schemas']['ConfigurationUpdated'];
export type ReviewSettings = components['schemas']['ReviewSettings'];
export type ReviewerSelection = components['schemas']['ReviewerSelection'];
export type ReviewerStatus = components['schemas']['ReviewerStatus'];
export type ReviewArea = components['schemas']['ReviewArea'];
export type ReviewerCandidates = components['schemas']['ReviewerCandidates'];

@Injectable({ providedIn: 'root' })
export class ReviewConfigurationApi {
  private readonly http = inject(HttpClient);
  read(actor: string, proposal: string) {
    return this.http.get<ReviewConfiguration>(this.path(proposal), { headers: { 'X-Expected-Actor': actor } });
  }
  update(actor: string, proposal: string, body: ReviewConfigurationRequest) {
    return this.http.put<ConfigurationUpdated>(this.path(proposal), body, { headers: { 'X-Expected-Actor': actor } });
  }
  candidates(actor: string, proposal: string, domain: ReviewArea, after?: string) {
    return this.http.get<ReviewerCandidates>(`/api/v1/proposals/${encodeURIComponent(proposal)}/review/reviewers`, {
      params: after ? { domain, after } : { domain }, headers: { 'X-Expected-Actor': actor },
    });
  }
  private path(proposal: string) { return `/api/v1/proposals/${encodeURIComponent(proposal)}/review/configuration`; }
}
