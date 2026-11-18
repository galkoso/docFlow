import { inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse, HttpResponse } from '@angular/common/http';
import { Subscription, timeout } from 'rxjs';
import { SessionState } from '../../core/auth/session-state';
import { SharedReviewBodyApi } from '../../core/api/shared-review-body-api';
import { IMAGE_DIGEST } from './image-state';
import { reviewMediaEvidence, reviewMediaKey, type ReviewInlineMediaEvidence, type ReviewMediaScope } from './review-media-evidence';

/** One lazy shared before/after image. Source is never persisted, URL publication waits
 * for path/domain identity and PNG body hashes, and every async boundary is fenced. */
@Injectable()
export class ReviewMediaState implements OnDestroy {
  private readonly api = inject(SharedReviewBodyApi);
  private readonly session = inject(SessionState);
  private readonly hash = inject(IMAGE_DIGEST);
  readonly denied = this.api.denied;
  private scope: ReviewMediaScope | null = null;
  private key: string | null = null;
  private generation = 0;
  private request?: Subscription;
  readonly data = signal<{ url: string; evidence: ReviewInlineMediaEvidence } | null>(null);
  readonly busy = signal(false);
  readonly loaded = signal(false);
  readonly error = signal('');
  readonly started = signal(false);
  open(scope: ReviewMediaScope | null): void {
    const key = reviewMediaKey(scope);
    if (key && key === this.key) return;
    this.clear();
    if (!key || !scope || scope.actor !== this.session.profile()?.user_id || this.api.denied()) return;
    this.scope = structuredClone(scope); this.key = key;
  }
  owns(scope: ReviewMediaScope): boolean {
    return reviewMediaKey(scope) === this.key && this.session.profile()?.user_id === scope.actor && !this.api.denied();
  }
  load(): void {
    const scope = this.scope;
    if (!scope || this.busy() || !this.owns(scope) || this.data()) return;
    this.drop(); this.started.set(true); this.error.set(''); this.busy.set(true); const generation = ++this.generation;
    const request = scope.beforeDocument
      ? this.api.beforeMediaImage(scope.actor, scope.proposal, scope.snapshot, scope.beforeDocument.file, scope.path.slice(7))
      : this.api.mediaImage(scope.actor, scope.proposal, scope.snapshot, scope.path.slice(7));
    this.request = request.pipe(timeout(30_000)).subscribe({
      next: response => { void this.install(response, scope, generation); }, error: error => { void this.failed(error, generation); },
    });
  }
  decoded(url: string, width: number, height: number): void {
    const data = this.data(); if (!data || data.url !== url) return;
    if (!this.scope || !this.owns(this.scope) || data.evidence.image.width !== width || data.evidence.image.height !== height) { this.failedImage(url); return; }
    this.loaded.set(true);
  }
  failedImage(url: string): void {
    if (url !== this.data()?.url) return;
    this.drop(); this.error.set('לא ניתן להציג את התמונה. אפשר לנסות שוב את אותה גרסה.');
  }
  clear(): void {
    ++this.generation; this.request?.unsubscribe(); this.request = undefined; this.scope = null; this.key = null;
    this.drop(); this.busy.set(false); this.error.set(''); this.started.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
  private drop(): void { const data = this.data(); if (data) URL.revokeObjectURL(data.url); this.data.set(null); this.loaded.set(false); }
  private async install(response: HttpResponse<Blob>, scope: ReviewMediaScope, generation: number): Promise<void> {
    try {
      const evidence = reviewMediaEvidence(response, scope);
      if (!evidence) throw new Error('MEDIA_EVIDENCE_MISMATCH');
      if (!scope.beforeDocument) {
        const identity = await this.hash(new Blob([`specspace-review-media-v1\0${scope.path}`]));
        if (generation !== this.generation || this.scope !== scope || !this.owns(scope)) return;
        if (identity !== evidence.image.file_id) throw new Error('MEDIA_IDENTITY_MISMATCH');
      }
      if (generation !== this.generation || this.scope !== scope || !this.owns(scope)) return;
      const display = await this.hash(response.body!);
      if (generation !== this.generation || this.scope !== scope || !this.owns(scope)) return;
      if (display !== evidence.image.display_sha256) throw new Error('MEDIA_DISPLAY_MISMATCH');
      const url = URL.createObjectURL(response.body!); this.data.set({ url, evidence }); this.busy.set(false);
    } catch {
      if (generation !== this.generation) return;
      this.busy.set(false); this.error.set('נתוני התמונה אינם תואמים לגרסת הסקירה שנבחרה. אפשר לבדוק שוב את אותה תמונה.');
    }
  }
  private async failed(error: unknown, generation: number): Promise<void> {
    let body = error instanceof HttpErrorResponse ? error.error : null;
    if (body instanceof Blob && body.size <= 16384) { try { body = JSON.parse(await body.text()); } catch { body = null; } }
    if (generation !== this.generation) return;
    this.busy.set(false); const message: unknown = body?.error?.message_he;
    this.error.set(typeof message === 'string' && message.length <= 2000 ? message : 'לא ניתן לטעון כרגע את התמונה. אפשר לנסות שוב את אותה גרסה.');
  }
}
