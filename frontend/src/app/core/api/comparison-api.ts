import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type ImageEvidence = components['schemas']['ImageEvidence'];
export type ImageSide = components['schemas']['ImageSide'];
export type DiffResponse = components['schemas']['DiffResponse'];
export type DiffFiles = components['schemas']['DiffFiles'];
export type ComparisonSummary = components['schemas']['ComparisonSummary'];
export type ComparisonChange = components['schemas']['Change'];
export type ComparisonJob = components['schemas']['JobStatus'];
export type RawFileResponse = components['schemas']['RawFileResponse'];
export type RawCursor = components['schemas']['RawCursor'];
export type RawWindow = components['schemas']['RawWindow'];
export type RawSegment = components['schemas']['RawSegment'];
export type ReadableFileResponse = components['schemas']['ReadableFileResponse'];
export type ReadableUnitResponse = components['schemas']['ReadableUnitResponse'];
export type ReadablePage = components['schemas']['ReadablePage'];
export type ReadableRow = components['schemas']['ReadableRow'];
export type ReadableUnitSummary = components['schemas']['UnitSummary'];
export type ComparisonSide = components['schemas']['ReadableComparisonSide'];
export type EventCursor = components['schemas']['EventCursor'];
export type EventWindow = components['schemas']['EventWindow'];
export interface ComparisonRequest { readonly request_id: string; readonly head?: string; readonly target?: string; }

@Injectable({ providedIn: 'root' })
export class ComparisonApi {
  private readonly http = inject(HttpClient);
  prepare(actor: string, proposal: string, request: ComparisonRequest) {
    // Copy the captured selection now, including when subscription is delayed.
    return this.http.get<DiffResponse>(this.path(proposal), { params: { ...request }, headers: this.actor(actor) });
  }
  page(actor: string, proposal: string, snapshot: string, offset = 0, limit = 50) {
    return this.http.get<DiffResponse>(this.path(proposal), {
      params: { snapshot, offset, limit }, headers: this.actor(actor),
    });
  }
  filePage(actor: string, proposal: string, snapshot: string, file: string) {
    return this.http.get<DiffResponse>(this.path(proposal), { params: { snapshot, file, limit: 50 }, headers: this.actor(actor) });
  }
  readableRow(actor: string, proposal: string, snapshot: string, file: string, row: string) {
    return this.http.get<ReadableFileResponse>(`${this.path(proposal)}/files/${encodeURIComponent(file)}/readable`, {
      params: { snapshot, row, limit: 50 }, headers: this.actor(actor),
    });
  }
  raw(actor: string, proposal: string, snapshot: string, file: string, cursor: RawCursor = { chunk: 0, offset: 0 }, maxBytes = 32768) {
    return this.http.get<RawFileResponse>(`${this.path(proposal)}/files/${encodeURIComponent(file)}`, {
      params: { snapshot, chunk: cursor.chunk, offset: cursor.offset, max_bytes: maxBytes }, headers: this.actor(actor),
    });
  }
  readable(actor: string, proposal: string, snapshot: string, file: string, offset = 0, limit = 50) {
    return this.http.get<ReadableFileResponse>(`${this.path(proposal)}/files/${encodeURIComponent(file)}/readable`, {
      params: { snapshot, offset, limit }, headers: this.actor(actor),
    });
  }
  readableUnit(actor: string, proposal: string, snapshot: string, file: string, side: ComparisonSide, unit: number,
    cursor: EventCursor = { event: 0, offset: 0 }, maxBytes = 32768) {
    return this.http.get<ReadableUnitResponse>(`${this.path(proposal)}/files/${encodeURIComponent(file)}/readable/units/${unit}`, {
      params: { snapshot, side, event: cursor.event, offset: cursor.offset, max_bytes: maxBytes }, headers: this.actor(actor),
    });
  }
  image(actor: string, proposal: string, snapshot: string, file: string, side: ImageSide) {
    return this.http.get(`${this.path(proposal)}/files/${encodeURIComponent(file)}/image`, {
      params: { snapshot, side }, headers: this.actor(actor), responseType: 'blob', observe: 'response',
    });
  }
  job(actor: string, job: string) {
    return this.http.get<ComparisonJob>(`/api/v1/jobs/${encodeURIComponent(job)}`, { headers: this.actor(actor) });
  }
  private path(proposal: string) { return `/api/v1/proposals/${encodeURIComponent(proposal)}/diff`; }
  private actor(actor: string) { return { 'X-Expected-Actor': actor }; }
}
