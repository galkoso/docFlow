import { inject, Injectable, OnDestroy, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { forkJoin, Subscription, timeout } from 'rxjs';
import { ReviewApi, type ReviewFiles, type ReviewHistory, type ReviewMedia, type ReviewSnapshot } from '../../core/api/review-api';
import { integer, uuid } from '../comparison/comparison-evidence';
import { validFiles, validHistory, validMedia, validSnapshot } from './snapshot-evidence';

export interface ReviewLocation { snapshot?: string; after?: string; files: number; media: number; }
@Injectable()
export class SharedReviewState implements OnDestroy {
  private readonly api = inject(ReviewApi);
  private requests = new Subscription();
  private generation = 0;
  private historyAfter?: string;
  readonly owner = signal<string | null>(null);
  readonly history = signal<ReviewHistory | null>(null);
  readonly snapshot = signal<ReviewSnapshot | null>(null);
  readonly files = signal<ReviewFiles | null>(null);
  readonly media = signal<ReviewMedia | null>(null);
  readonly selected = signal<string | null>(null);
  readonly phase = signal<'idle' | 'loading' | 'ready' | 'error'>('idle');
  readonly error = signal('');
  readonly authRequired = signal(false);
  open(actor: string, proposal: string, location: ReviewLocation): void {
    this.clear(); this.owner.set(actor);
    this.historyAfter = location.after;
    if (!uuid(actor) || !uuid(proposal) || location.snapshot != null && !uuid(location.snapshot) || location.after != null && !uuid(location.after)
      || !integer(location.files, 40000) || !integer(location.media, 20000)
      || !location.snapshot && (location.files !== 0 || location.media !== 0)) {
      this.phase.set('error'); this.error.set('כתובת גרסת הסקירה אינה תקינה.'); return;
    }
    const generation = this.generation; this.phase.set('loading');
    this.requests.add(this.api.history(actor, proposal, location.after).pipe(timeout(20_000)).subscribe({
      next: history => {
        if (generation !== this.generation) return;
        if (!validHistory(history, proposal, location.after)) { this.invalid(); return; }
        this.history.set(history);
        const selected = location.snapshot ?? history.active_snapshot_id ?? history.snapshots[0]?.snapshot_id;
        if (!selected) { this.phase.set('ready'); return; }
        this.selected.set(selected);
        this.requests.add(forkJoin({ snapshot: this.api.snapshot(actor, proposal, selected),
          files: this.api.files(actor, proposal, selected, location.files), media: this.api.media(actor, proposal, selected, location.media) })
          .pipe(timeout(20_000)).subscribe({
            next: ({ snapshot, files, media }) => {
              if (generation !== this.generation) return;
              if (!validSnapshot(snapshot, proposal, selected) || !validFiles(files, snapshot, location.files) || !validMedia(media, snapshot, location.media)) {
                this.invalid(); return;
              }
              const entry = history.snapshots.find(item => item.snapshot_id === selected), summary = snapshot.metadata.summary;
              if (summary.expected_proposal_version > history.current_proposal_version) { this.invalid(); return; }
              if (entry && (entry.fingerprint !== snapshot.fingerprint || entry.head_sha !== summary.head_sha || entry.target_sha !== summary.target_sha
                || entry.file_count !== snapshot.file_count || entry.media_count !== snapshot.media_count || entry.title !== snapshot.title
                || entry.branch !== summary.branch || entry.prepared_version !== summary.expected_proposal_version
                || entry.configuration_version !== summary.configuration_version || entry.policy_version !== summary.policy_version
                || entry.created_at !== snapshot.created_at || entry.created_by !== snapshot.created_by)) { this.invalid(); return; }
              this.snapshot.set(snapshot); this.files.set(files); this.media.set(media); this.phase.set('ready');
            }, error: error => { if (generation === this.generation) this.failure(error); },
          }));
      }, error: error => { if (generation === this.generation) this.failure(error); },
    }));
  }
  refreshHistory(actor: string, proposal: string): void {
    if (this.owner() !== actor || !this.snapshot()) return;
    const generation = this.generation, after = this.historyAfter;
    this.requests.add(this.api.history(actor, proposal, after).pipe(timeout(20000)).subscribe({
      next: value => {
        if (generation !== this.generation || this.owner() !== actor) return;
        if (!validHistory(value, proposal, after)) { this.invalid(); return; }
        this.history.set(value);
      },
      error: error => {
        if (generation !== this.generation) return;
        if ([401, 403, 404].includes(error?.status) || error?.error?.error?.code === 'SESSION_IDENTITY_CHANGED') {
          this.clear(); this.authRequired.set(true);
        } else this.error.set('ההחלטה נשמרה, אך פרטי מצב ההצעה לא עודכנו בתצוגה. יש לטעון אותם מחדש.');
      },
    }));
  }
  private invalid(): void { this.snapshot.set(null); this.files.set(null); this.media.set(null); this.phase.set('error'); this.error.set('נתוני גרסת הסקירה אינם תואמים לבקשה. יש לטעון את הגרסה מחדש.'); }
  private failure(error: unknown): void {
    this.snapshot.set(null); this.files.set(null); this.media.set(null); this.phase.set('error');
    const status = error instanceof HttpErrorResponse ? error.status : 0;
    if ([401, 403, 404].includes(status) || error instanceof HttpErrorResponse && error.error?.error?.code === 'SESSION_IDENTITY_CHANGED') {
      this.history.set(null); this.selected.set(null); this.error.set('יש לבדוק את הגרסה, ההתחברות וההרשאות לפני טעינה נוספת.');
      this.authRequired.set(status === 401 || error instanceof HttpErrorResponse && error.error?.error?.code === 'SESSION_IDENTITY_CHANGED');
    } else this.error.set('לא ניתן לטעון כרגע את גרסת הסקירה. אפשר לנסות שוב.');
  }
  clear(): void {
    ++this.generation; this.requests.unsubscribe(); this.requests = new Subscription(); this.owner.set(null);
    this.history.set(null); this.snapshot.set(null); this.files.set(null); this.media.set(null); this.selected.set(null);
    this.phase.set('idle'); this.error.set(''); this.authRequired.set(false);
  }
  ngOnDestroy(): void { this.clear(); }
}
