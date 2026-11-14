import { inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription, switchMap, throwError } from 'rxjs';
import { Book, BookApi, BookDocument, RepositoryList } from '../../core/api/book-api';

export function bookError(error: unknown): string {
  const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
  return typeof message === 'string' ? message : 'הטעינה לא הושלמה. אפשר לנסות שוב.';
}

/** Component-scoped, disposable content. Every chapter request uses the selected SHA. */
@Injectable()
export class BookState {
  private readonly api = inject(BookApi);
  private requests = new Subscription();
  private generation = 0;
  private activeDocuments = new Set<string>();
  readonly book = signal<Book | null>(null);
  readonly repositories = signal<RepositoryList['repositories']>([]);
  readonly documents = signal<Record<string, BookDocument>>({});
  readonly documentErrors = signal<Record<string, string>>({});
  readonly loadingDocuments = signal<ReadonlySet<string>>(new Set());
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

  clear(): void {
    ++this.generation;
    this.requests.unsubscribe();
    this.requests = new Subscription();
    this.activeDocuments.clear();
    this.book.set(null);
    this.repositories.set([]);
    this.documents.set({});
    this.documentErrors.set({});
    this.loadingDocuments.set(new Set());
    this.loading.set(false);
    this.error.set(null);
  }

  open(repository?: string, commit?: string): void {
    this.clear();
    const generation = this.generation;
    this.loading.set(true);
    this.requests.add(this.api.repositories().pipe(switchMap(list => {
      this.repositories.set(list.repositories);
      const selected = repository ?? list.repositories[0]?.repository_id;
      if (!selected) return throwError(() => new Error('NO_REPOSITORIES'));
      // An explicit inaccessible repository must never fall back to a different one.
      return this.api.specification(selected, commit);
    })).subscribe({
      next: book => {
        if (generation !== this.generation) return;
        this.book.set(book);
        this.loading.set(false);
        const first = book.documents[0];
        if (first) this.loadDocument(first.document_id);
      },
      error: error => {
        if (generation !== this.generation) return;
        this.loading.set(false);
        this.error.set(error instanceof Error && error.message === 'NO_REPOSITORIES'
          ? 'אין מאגרי אפיון זמינים לחשבון זה.' : bookError(error));
      },
    }));
  }

  loadDocument(id: string): void {
    const book = this.book();
    if (!book || this.documents()[id] || this.activeDocuments.has(id) || !book.documents.some(doc => doc.document_id === id)) return;
    const generation = this.generation;
    this.activeDocuments.add(id);
    this.loadingDocuments.set(new Set(this.activeDocuments));
    this.documentErrors.update(errors => { const next = { ...errors }; delete next[id]; return next; });
    this.requests.add(this.api.document(book.repository_id, book.snapshot_sha, id).subscribe({
      next: document => {
        if (generation !== this.generation) return;
        if (document.snapshot_sha !== book.snapshot_sha || document.document_id !== id) {
          this.documentErrors.update(errors => ({ ...errors, [id]: 'התקבלה גרסה שונה. יש לרענן את הספר.' }));
        } else this.documents.update(documents => ({ ...documents, [id]: document }));
        this.finishDocument(id);
      },
      error: error => {
        if (generation !== this.generation) return;
        if (error instanceof HttpErrorResponse && [401, 403, 404].includes(error.status)) {
          this.clear();
          this.error.set(error.status === 401 ? 'החיבור פג. יש להתחבר מחדש.' : 'הגישה לספר אינה זמינה. יש לטעון אותו מחדש.');
          return;
        }
        this.documentErrors.update(errors => ({ ...errors, [id]: bookError(error) }));
        this.finishDocument(id);
      },
    }));
  }

  private finishDocument(id: string): void {
    this.activeDocuments.delete(id);
    this.loadingDocuments.set(new Set(this.activeDocuments));
  }
}
