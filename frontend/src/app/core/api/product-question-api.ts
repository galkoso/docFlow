import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type ProductQuestions = components['schemas']['ProductQuestions'];
export type ProductQuestionHistory = components['schemas']['ProductQuestionHistory'];
export type ProductQuestionCreate = components['schemas']['CreateProductQuestion'];
export type ProductQuestionChange = components['schemas']['ChangeProductQuestion'];
export type ProductQuestionRecorded = components['schemas']['ProductQuestionRecorded'];

@Injectable({ providedIn: 'root' })
export class ProductQuestionApi {
  private readonly http = inject(HttpClient);
  list(actor: string, proposal: string, after?: string) {
    return this.http.get<ProductQuestions>(this.root(proposal), { headers: this.actor(actor), params: after ? { after } : {} });
  }
  history(actor: string, proposal: string, question: string, after?: number) {
    return this.http.get<ProductQuestionHistory>(`${this.root(proposal)}/${encodeURIComponent(question)}`, {
      headers: this.actor(actor), params: after == null ? {} : { after },
    });
  }
  create(actor: string, proposal: string, body: Readonly<ProductQuestionCreate>) {
    return this.http.post<ProductQuestionRecorded>(this.root(proposal), body, { headers: this.actor(actor) });
  }
  change(actor: string, proposal: string, question: string, body: Readonly<ProductQuestionChange>) {
    return this.http.post<ProductQuestionRecorded>(`${this.root(proposal)}/${encodeURIComponent(question)}/actions`, body, { headers: this.actor(actor) });
  }
  private root(proposal: string) { return `/api/v1/proposals/${encodeURIComponent(proposal)}/questions`; }
  private actor(actor: string) { return { 'X-Expected-Actor': actor }; }
}
