import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type Book = components['schemas']['Specification'];
export type BookDocument = components['schemas']['DocumentResponse'];
export type RepositoryList = components['schemas']['RepositoryList'];
export type SearchPage = components['schemas']['SearchResponse'];
export type MatchWindow = components['schemas']['MatchWindow'];
export type Highlight = components['schemas']['Highlight'];
export type HighlightResponse = components['schemas']['HighlightResponse'];

@Injectable({ providedIn: 'root' })
export class BookApi {
  private readonly http = inject(HttpClient);
  repositories() { return this.http.get<RepositoryList>('/api/v1/repositories'); }
  specification(repository: string, commit?: string) {
    return this.http.get<Book>(`${this.base(repository)}/specification`, { params: commit ? { commit } : {} });
  }
  document(repository: string, commit: string, document: string) {
    return this.http.get<BookDocument>(`${this.base(repository)}/documents/${encodeURIComponent(document)}`, { params: { commit } });
  }
  find(repository: string, commit: string, q: string, startAt = 0) {
    return this.http.get<SearchPage>(`${this.base(repository)}/find`, { params: { commit, q, limit: '100', start_at: String(startAt) } });
  }
  highlights(repository: string, commit: string, document: string, q: string, windows: MatchWindow[]) {
    const bytes = new TextEncoder().encode(JSON.stringify(windows));
    const encoded = btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
    return this.http.get<HighlightResponse>(`${this.base(repository)}/documents/${encodeURIComponent(document)}/highlights`,
      { params: { commit, q, windows: encoded } });
  }
  private base(repository: string) { return `/api/v1/repositories/${encodeURIComponent(repository)}`; }
}
