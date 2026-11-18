import { inject, Injectable, signal } from '@angular/core';
import { HttpClient, HttpErrorResponse } from '@angular/common/http';
import { Observable, tap } from 'rxjs';
import type { ComparisonBodyApi } from './comparison-body-api';
import type { ComparisonSide, EventCursor, ImageSide, RawCursor, RawFileResponse, ReadableFileResponse, ReadableUnitResponse } from './comparison-api';

@Injectable({ providedIn: 'root' })
export class SharedReviewBodyApi implements ComparisonBodyApi {
  private readonly http = inject(HttpClient);
  readonly denied = signal(false);
  raw(actor: string, proposal: string, snapshot: string, file: string, cursor: RawCursor = { chunk: 0, offset: 0 }, maxBytes = 32768) {
    return this.observe(this.http.get<RawFileResponse>(`${this.path(proposal, snapshot, file)}/raw`, {
      params: { chunk: cursor.chunk, offset: cursor.offset, max_bytes: maxBytes }, headers: this.actor(actor),
    }));
  }
  readable(actor: string, proposal: string, snapshot: string, file: string, offset = 0, limit = 50) {
    return this.observe(this.http.get<ReadableFileResponse>(`${this.path(proposal, snapshot, file)}/readable`, {
      params: { offset, limit }, headers: this.actor(actor),
    }));
  }
  readableRow(actor: string, proposal: string, snapshot: string, file: string, row: string) {
    return this.observe(this.http.get<ReadableFileResponse>(`${this.path(proposal, snapshot, file)}/readable`, {
      params: { row, limit: 50 }, headers: this.actor(actor),
    }));
  }
  readableUnit(actor: string, proposal: string, snapshot: string, file: string, side: ComparisonSide, unit: number,
    cursor: EventCursor = { event: 0, offset: 0 }, maxBytes = 32768) {
    return this.observe(this.http.get<ReadableUnitResponse>(`${this.path(proposal, snapshot, file)}/readable/units/${unit}`, {
      params: { side, event: cursor.event, offset: cursor.offset, max_bytes: maxBytes }, headers: this.actor(actor),
    }));
  }
  image(actor: string, proposal: string, snapshot: string, file: string, side: ImageSide) {
    return this.observe(this.http.get(`${this.path(proposal, snapshot, file)}/image`, {
      params: { side }, headers: this.actor(actor), responseType: 'blob', observe: 'response',
    }));
  }
  mediaImage(actor: string, proposal: string, snapshot: string, filename: string) {
    return this.observe(this.http.get(`/api/v1/proposals/${encodeURIComponent(proposal)}/review/snapshots/${encodeURIComponent(snapshot)}/media/${encodeURIComponent(filename)}/image`, {
      headers: this.actor(actor), responseType: 'blob', observe: 'response',
    }));
  }
  beforeMediaImage(actor: string, proposal: string, snapshot: string, file: string, filename: string) {
    return this.observe(this.http.get(`${this.path(proposal, snapshot, file)}/before-media/${encodeURIComponent(filename)}/image`, {
      headers: this.actor(actor), responseType: 'blob', observe: 'response',
    }));
  }
  private observe<T>(request: Observable<T>): Observable<T> {
    return request.pipe(tap({ error: (error: unknown) => {
      if (!(error instanceof HttpErrorResponse)) return;
      if ([401, 403, 404].includes(error.status) || error.error?.error?.code === 'SESSION_IDENTITY_CHANGED') this.denied.set(true);
      if (error.status === 409 && error.error instanceof Blob && error.error.size <= 16384) {
        void error.error.text().then(text => {
          try { if (JSON.parse(text)?.error?.code === 'SESSION_IDENTITY_CHANGED') this.denied.set(true); } catch { /* Invalid error evidence is never displayed. */ }
        });
      }
    } }));
  }
  private path(proposal: string, snapshot: string, file: string) {
    return `/api/v1/proposals/${encodeURIComponent(proposal)}/review/snapshots/${encodeURIComponent(snapshot)}/files/${encodeURIComponent(file)}`;
  }
  private actor(actor: string) { return { 'X-Expected-Actor': actor }; }
}
