import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type CheckOperation = components['schemas']['ReviewCheckOperation'];
export type CheckOperationPage = components['schemas']['ReviewCheckOperationPage'];
export type CheckRecoveryRequest = components['schemas']['ReviewCheckRecoveryRequest'];
export type CheckRecoveryAccepted = components['schemas']['ReviewCheckRecoveryAccepted'];
export const CHECK_PAGE_SIZE = 20;

@Injectable({ providedIn: 'root' })
export class CheckOperationsApi {
  private readonly http = inject(HttpClient);
  page(actor: string, proposal: string, after?: string) {
    return this.http.get<CheckOperationPage>(this.collection(proposal), {
      headers: { 'X-Expected-Actor': actor }, params: { limit: CHECK_PAGE_SIZE, ...(after ? { after } : {}) },
    });
  }
  read(actor: string, proposal: string, operation: string) {
    return this.http.get<CheckOperation>(`${this.collection(proposal)}/${encodeURIComponent(operation)}`, { headers: { 'X-Expected-Actor': actor } });
  }
  recover(actor: string, proposal: string, operation: string, body: Readonly<CheckRecoveryRequest>) {
    return this.http.post<CheckRecoveryAccepted>(`${this.collection(proposal)}/${encodeURIComponent(operation)}/recover`, body,
      { headers: { 'X-Expected-Actor': actor } });
  }
  private collection(proposal: string) { return `/api/v1/proposals/${encodeURIComponent(proposal)}/review/checks`; }
}
