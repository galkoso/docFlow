import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type ReviewRequest = components['schemas']['ReviewSubmissionRequest'];
export type ReviewAccepted = components['schemas']['ReviewSubmissionAccepted'];
export type ReviewPublication = components['schemas']['ReviewPublication'];
export type ReviewJob = components['schemas']['JobStatus'];
export type ReviewSnapshot = components['schemas']['ReviewSnapshot'];
export type ReviewHistory = components['schemas']['ReviewSnapshotHistory'];
export type ReviewFiles = components['schemas']['ReviewSnapshotFiles'];
export type ReviewMedia = components['schemas']['ReviewSnapshotMedia'];
export type ReviewDocuments = components['schemas']['ReviewSnapshotDocuments'];
export type ReviewBookDocument = components['schemas']['ReviewBookDocument'];
export type ReviewPrStatus = components['schemas']['ReviewPrStatus'];
export type PrRecoveryRequest = components['schemas']['PrRecoveryRequest'];
export type PrRecoveryAccepted = components['schemas']['PrRecoveryAccepted'];
export type ReviewDetail = components['schemas']['ProposalDetail'];
export type OfficialHistory = components['schemas']['OfficialReviewHistory'];
export type OfficialRequest = components['schemas']['OfficialReviewRequest'];
export type OfficialRecorded = components['schemas']['OfficialReviewRecorded'];

@Injectable({ providedIn: 'root' })
export class ReviewApi {
  private readonly http = inject(HttpClient);
  submit(actor: string, proposal: string, key: string, body: Readonly<ReviewRequest>) {
    return this.http.post<ReviewAccepted>(`${this.proposal(proposal)}/submit`, body,
      { headers: { 'X-Expected-Actor': actor, 'Idempotency-Key': key } });
  }
  detail(actor: string, proposal: string) {
    return this.http.get<ReviewDetail>(this.proposal(proposal), { headers: this.actor(actor) });
  }
  job(actor: string, job: string) { return this.http.get<ReviewJob>(`/api/v1/jobs/${encodeURIComponent(job)}`, { headers: this.actor(actor) }); }
  history(actor: string, proposal: string, after?: string) {
    return this.http.get<ReviewHistory>(this.snapshots(proposal), { headers: this.actor(actor), params: { limit: 20, ...(after ? { after } : {}) } });
  }
  snapshot(actor: string, proposal: string, snapshot: string) {
    return this.http.get<ReviewSnapshot>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}`, { headers: this.actor(actor) });
  }
  files(actor: string, proposal: string, snapshot: string, offset = 0) {
    return this.http.get<ReviewFiles>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}/files`, {
      headers: this.actor(actor), params: { offset, limit: 50 },
    });
  }
  documents(actor: string, proposal: string, snapshot: string, offset = 0) {
    return this.http.get<ReviewDocuments>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}/documents`, {
      headers: this.actor(actor), params: { offset, limit: 20 },
    });
  }
  prStatus(actor: string, proposal: string, snapshot: string) {
    return this.http.get<ReviewPrStatus>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}/pr-sync`, { headers: this.actor(actor) });
  }
  recoverPr(actor: string, proposal: string, snapshot: string, body: Readonly<PrRecoveryRequest>) {
    return this.http.post<PrRecoveryAccepted>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}/pr-sync/recover`, body,
      { headers: this.actor(actor) });
  }
  media(actor: string, proposal: string, snapshot: string, offset = 0) {
    return this.http.get<ReviewMedia>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}/media`, {
      headers: this.actor(actor), params: { offset, limit: 50 },
    });
  }
  decisions(actor: string, proposal: string, snapshot: string, after?: string) {
    return this.http.get<OfficialHistory>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}/decisions`, {
      headers: this.actor(actor), params: after ? { after } : {},
    });
  }
  recordDecision(actor: string, proposal: string, snapshot: string, body: Readonly<OfficialRequest>) {
    return this.http.post<OfficialRecorded>(`${this.snapshots(proposal)}/${encodeURIComponent(snapshot)}/decisions`, body, { headers: this.actor(actor) });
  }
  private actor(actor: string) { return { 'X-Expected-Actor': actor }; }
  private proposal(id: string) { return `/api/v1/proposals/${encodeURIComponent(id)}`; }
  private snapshots(id: string) { return `${this.proposal(id)}/review/snapshots`; }
}
