import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { components } from './generated/schema';

export type EditorModel = components['schemas']['EditorDocument'];
export type EditorBlock = components['schemas']['EditorBlock'];
export type EditorEdit = components['schemas']['EditorEdit'];
export type EditorResponse = components['schemas']['EditorResponse'];
export type EditorPreview = components['schemas']['EditorPreview'];
export type DraftSnapshot = components['schemas']['DraftSnapshot'];
export type DraftSaved = components['schemas']['DraftSaved'];
export type SaveDraft = components['schemas']['SaveDraft'];
export type DraftBuffer = components['schemas']['DraftBuffer'];

@Injectable({ providedIn: 'root' })
export class EditorApi {
  private readonly http = inject(HttpClient);
  model(proposal: string, document: string, commit: string) {
    return this.http.get<EditorResponse>(this.document(proposal, document), { params: { commit } });
  }
  preview(proposal: string, document: string, commit: string, version: number, edit: EditorEdit) {
    return this.http.post<EditorPreview>(`${this.document(proposal, document)}/preview`, { commit, expected_proposal_version: version, edit });
  }
  recovery(proposal: string, document?: string) {
    return this.http.get<DraftSnapshot>(this.draft(proposal), { params: document ? { document_id: document } : {} });
  }
  saveRecovery(proposal: string, body: SaveDraft) { return this.http.put<DraftSaved>(this.draft(proposal), body); }
  discardRecovery(proposal: string, document: string, version: number) {
    return this.http.delete<void>(this.draft(proposal), { params: { document_id: document, expected_draft_version: String(version) } });
  }
  private document(proposal: string, document: string) {
    return `/api/v1/proposals/${encodeURIComponent(proposal)}/documents/${encodeURIComponent(document)}/editor`;
  }
  private draft(proposal: string) { return `/api/v1/proposals/${encodeURIComponent(proposal)}/draft`; }
}
