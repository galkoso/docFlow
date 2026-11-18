import { COMPARISON_BODY_API } from '../../core/api/comparison-body-api';
import { inject, Injectable, InjectionToken, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse, HttpResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { ImageEvidence, ImageSide } from '../../core/api/comparison-api';
import { changeIdentity, digest, integer, sha, uuid, validChange } from './comparison-evidence';
import type { RawScope } from './raw-state';

export interface ImageScope { file: RawScope; side: ImageSide; }
export const IMAGE_DIGEST = new InjectionToken<(blob: Blob) => Promise<string>>('image display SHA-256', {
  providedIn: 'root', factory: () => async blob => {
    const hash = await crypto.subtle.digest('SHA-256', await blob.arrayBuffer());
    return Array.from(new Uint8Array(hash), byte => byte.toString(16).padStart(2, '0')).join('');
  },
});
function scopeKey(scope: ImageScope): string | null {
  const file = scope?.file;
  if (!file || ![file.actor, file.proposal, file.snapshot].every(uuid) || !digest(file.content) || !sha(file.head) || !sha(file.base)
    || !validChange(file.file) || file.file.kind !== 'asset' || !['before', 'after'].includes(scope.side)) return null;
  return JSON.stringify([file.actor, file.proposal, file.snapshot, file.content, file.head, file.base, changeIdentity(file.file), scope.side]);
}
function evidence(response: HttpResponse<Blob>, scope: ImageScope): ImageEvidence | null {
  const source = scope.file.file[scope.side], header = response.headers.get('X-Comparison-Image');
  if (!source || !header || header.length > 4096 || !/^[A-Za-z0-9_-]+$/.test(header) || response.headers.get('Content-Type') !== 'image/png'
    || !(response.body instanceof Blob) || response.body.type !== 'image/png' || response.body.size > 16 * 1024 * 1024) return null;
  try {
    const value: ImageEvidence = JSON.parse(atob(header.replaceAll('-', '+').replaceAll('_', '/')));
    if (!value || value.actor_id !== scope.file.actor || value.proposal_id !== scope.file.proposal || value.snapshot_id !== scope.file.snapshot
      || value.content_id !== scope.file.content || value.file_id !== scope.file.file.id || value.side !== scope.side
      || value.head_sha !== scope.file.head || value.merge_base_sha !== scope.file.base || value.source_blob_oid !== source.blob_oid
      || value.source_sha256 !== source.path.slice(7, 71) || source.sha256 != null && value.source_sha256 !== source.sha256
      || value.source_bytes !== source.bytes || !digest(value.display_sha256) || value.display_bytes !== response.body.size
      || !integer(value.display_bytes, 16 * 1024 * 1024, 1) || !integer(value.width, 2048, 1) || !integer(value.height, 2048, 1)) return null;
    return value;
  } catch { return null; }
}
/** One bounded PNG derivative. URLs are created only after proof/body validation
 * and are revoked on replacement, retry, failure, scope loss and destruction. */
@Injectable()
export class ImageState implements OnDestroy {
  private readonly api = inject(COMPARISON_BODY_API);
  private readonly hash = inject(IMAGE_DIGEST);
  private scope: ImageScope | null = null;
  private readonly key = signal('');
  private generation = 0;
  private request?: Subscription;
  readonly data = signal<{ url: string; evidence: ImageEvidence } | null>(null);
  readonly absent = signal(false);
  readonly canRetry = signal(false);
  readonly busy = signal(false);
  readonly loaded = signal(false);
  readonly error = signal('');
  readonly authRequired = signal(false);
  owns(scope: ImageScope): boolean { return scopeKey(scope) === this.key(); }
  open(scope: ImageScope | null): void {
    if (!scope) { this.clear(); return; }
    const key = scopeKey(scope); if (key && key === this.key()) return;
    this.clear();
    if (!key) { this.error.set('פרטי התמונה אינם תואמים להשוואה שנבחרה.'); return; }
    this.scope = structuredClone(scope); this.key.set(key);
    const source = scope.file.file[scope.side];
    if (!source) { this.absent.set(true); return; }
    if (!/^assets\/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$/.test(source.path) || source.executable || source.bytes > 5 * 1024 * 1024) {
      this.error.set('סוג התמונה, נתיבה או גודלה אינם מאפשרים תצוגה.'); return;
    }
    this.canRetry.set(true); this.fetch();
  }
  retry(): void { if (this.scope && this.canRetry() && !this.absent() && !this.busy() && !this.authRequired()) this.fetch(); }
  decoded(url: string, width: number, height: number): void {
    const data = this.data(); if (!data || data.url !== url) return;
    if (data.evidence.width !== width || data.evidence.height !== height) { this.imageFailed(url); return; }
    this.loaded.set(true);
  }
  imageFailed(url: string): void {
    if (this.data()?.url !== url) return;
    this.drop(); this.error.set('לא ניתן להציג את התמונה. אפשר לנסות שוב את אותה גרסה.');
  }
  clear(): void {
    ++this.generation; this.request?.unsubscribe(); this.request = undefined; this.scope = null; this.key.set('');
    this.drop(); this.absent.set(false); this.canRetry.set(false); this.busy.set(false); this.error.set(''); this.authRequired.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
  private drop(): void {
    const data = this.data(); if (data) URL.revokeObjectURL(data.url);
    this.data.set(null); this.loaded.set(false);
  }
  private fetch(): void {
    const scope = this.scope; if (!scope) return;
    const source = scope.file.file[scope.side];
    if (!source || source.executable || source.bytes > 5 * 1024 * 1024 || !/^assets\/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$/.test(source.path)) return;
    const generation = ++this.generation;
    this.drop(); this.busy.set(true); this.error.set('');
    this.request = this.api.image(scope.file.actor, scope.file.proposal, scope.file.snapshot, scope.file.file.id, scope.side).pipe(timeout(30_000)).subscribe({
      next: response => { void this.install(response, scope, generation); },
      error: error => { void this.failed(error, generation); },
    });
  }
  private async install(response: HttpResponse<Blob>, scope: ImageScope, generation: number): Promise<void> {
    try {
      const proof = evidence(response, scope);
      if (!proof || await this.hash(response.body!) !== proof.display_sha256) throw new Error('IMAGE_EVIDENCE_MISMATCH');
      if (generation !== this.generation || this.scope !== scope) return;
      const url = URL.createObjectURL(response.body!); this.data.set({ url, evidence: proof }); this.busy.set(false);
    } catch {
      if (generation !== this.generation) return;
      this.busy.set(false); this.error.set('נתוני התמונה אינם תואמים לגרסה שנבחרה. אפשר לבדוק שוב את אותה תמונה.');
    }
  }
  private async failed(error: unknown, generation: number): Promise<void> {
    let body = error instanceof HttpErrorResponse ? error.error : null;
    if (body instanceof Blob && body.size <= 16384) { try { body = JSON.parse(await body.text()); } catch { body = null; } }
    if (generation !== this.generation) return;
    this.busy.set(false);
    this.authRequired.set(error instanceof HttpErrorResponse && (error.status === 401 || body?.error?.code === 'SESSION_IDENTITY_CHANGED'));
    const message: unknown = body?.error?.message_he;
    this.error.set(typeof message === 'string' && message.length <= 2000 ? message : 'לא ניתן לטעון כרגע את התמונה. אפשר לנסות שוב את אותה גרסה.');
  }
}
