import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type DiscussionThreads = components['schemas']['DiscussionThreads'];
export type DiscussionComments = components['schemas']['DiscussionComments'];
export type DiscussionThread = components['schemas']['DiscussionThread'];
export type DiscussionComment = components['schemas']['DiscussionComment'];
export type OpenDiscussion = components['schemas']['OpenThreadRequest'];
export type DiscussionAction = components['schemas']['ThreadMutationRequest'];
export type DiscussionRecorded = components['schemas']['DiscussionRecorded'];
export type DiscussionIntent = { kind: 'open'; body: Readonly<OpenDiscussion> }
  | { kind: 'action'; thread: string; body: Readonly<DiscussionAction> };

@Injectable({ providedIn: 'root' })
export class DiscussionApi {
  private readonly http = inject(HttpClient);
  threads(actor: string, proposal: string, after?: string) {
    return this.http.get<DiscussionThreads>(this.root(proposal), { headers: this.actor(actor), params: after ? { after } : {} });
  }
  comments(actor: string, proposal: string, thread: string, version?: number, after?: string) {
    return this.http.get<DiscussionComments>(`${this.root(proposal)}/${encodeURIComponent(thread)}`, {
      headers: this.actor(actor), params: { ...(version != null ? { expected_thread_version: version } : {}), ...(after ? { after } : {}) },
    });
  }
  write(actor: string, proposal: string, intent: DiscussionIntent) {
    const url = intent.kind === 'open' ? this.root(proposal) : `${this.root(proposal)}/${encodeURIComponent(intent.thread)}/actions`;
    return this.http.post<DiscussionRecorded>(url, intent.body, { headers: this.actor(actor) });
  }
  private actor(actor: string) { return { 'X-Expected-Actor': actor }; }
  private root(proposal: string) { return `/api/v1/proposals/${encodeURIComponent(proposal)}/threads`; }
}
