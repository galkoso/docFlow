import { inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { map, Observable, Subscription, switchMap, throwError } from 'rxjs';
import { Book, BookApi, BookDocument, RepositoryList } from '../../core/api/book-api';
import { ProposalApi, ProposalDetail } from '../../core/api/proposal-api';

export function bookError(error: unknown): string {
  const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
  return typeof message === 'string' ? message : 'הטעינה לא הושלמה. אפשר לנסות שוב.';
}

/** Component-scoped, disposable content. Every chapter request uses the selected SHA. */
@Injectable()
export class BookState {
  private readonly api = inject(BookApi);
  private readonly proposals = inject(ProposalApi);
  private requests = new Subscription();
  private generation = 0;
  private activeDocuments = new Set<string>();
  private nearDocuments = new Set<string>();
  private pendingDocuments: string[] = [];
  private pinnedDocument: string | null = null;
  readonly book = signal<Book | null>(null);
  readonly proposal = signal<ProposalDetail | null>(null);
  readonly proposalId = signal<string | undefined>(undefined);
  readonly repositories = signal<RepositoryList['repositories']>([]);
  readonly documents = signal<Record<string, BookDocument>>({});
  readonly documentErrors = signal<Record<string, string>>({});
  readonly loadingDocuments = signal<ReadonlySet<string>>(new Set());
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly heights = signal<Record<string, number>>({});

  clear(): void {
    ++this.generation;
    this.requests.unsubscribe();
    this.requests = new Subscription();
    this.activeDocuments.clear();
    this.nearDocuments.clear();
    this.pendingDocuments = [];
    this.pinnedDocument = null;
    this.book.set(null);
    this.proposal.set(null);
    this.proposalId.set(undefined);
    this.repositories.set([]);
    this.documents.set({});
    this.documentErrors.set({});
    this.loadingDocuments.set(new Set());
    this.loading.set(false);
    this.error.set(null);
    this.heights.set({});
  }

  open(repository?: string, commit?: string): void {
    this.clear();
    this.watch(this.api.repositories().pipe(switchMap(list => {
      this.repositories.set(list.repositories);
      const selected = repository ?? list.repositories[0]?.repository_id;
      if (!selected) return throwError(() => new Error('NO_REPOSITORIES'));
      // An explicit inaccessible repository must never fall back to a different one.
      return this.api.specification(selected, commit);
    })));
  }

  openProposal(id: string, commit?: string): void {
    this.clear();
    this.proposalId.set(id);
    this.watch(this.proposals.detail(id).pipe(switchMap(proposal => {
      if (proposal.proposal_id !== id) return throwError(() => new Error('READER_SCOPE_MISMATCH'));
      if (!proposal.head_sha) return throwError(() => new Error('PROPOSAL_NOT_READY'));
      return this.api.specification(proposal.repository_id, commit, id).pipe(map(book => {
        if (book.repository_id !== proposal.repository_id || book.branch !== proposal.branch
          || book.snapshot_sha !== (commit ?? proposal.head_sha)) throw new Error('READER_SCOPE_MISMATCH');
        this.proposal.set(proposal);
        return book;
      }));
    })));
  }

  private watch(request: Observable<Book>): void {
    const generation = this.generation;
    this.loading.set(true);
    this.requests.add(request.subscribe({
      next: book => {
        if (generation !== this.generation) return;
        this.book.set(book);
        this.loading.set(false);
        const first = book.documents[0];
        if (first) this.loadDocument(first.document_id);
      },
      error: error => {
        if (generation !== this.generation) return;
        if (error instanceof HttpErrorResponse && [401, 403, 404].includes(error.status)) this.clear();
        this.loading.set(false);
        const messages: Record<string, string> = { NO_REPOSITORIES: 'אין מאגרי אפיון זמינים לחשבון זה.',
          PROPOSAL_NOT_READY: 'ענף ההצעה עדיין אינו מאומת. אפשר לבדוק את ההכנה בפרטי ההצעה.',
          READER_SCOPE_MISMATCH: 'זהות ההצעה או הגרסה השתנתה בזמן הטעינה. יש לנסות שוב.' };
        this.error.set(error instanceof Error && messages[error.message] ? messages[error.message] : bookError(error));
      },
    }));
  }

  loadDocument(id: string): void {
    this.pinnedDocument = id;
    this.queueDocument(id, true);
  }

  viewport(id: string, near: boolean): void {
    if (near) {
      this.nearDocuments.add(id);
      this.queueDocument(id, false);
    } else {
      this.nearDocuments.delete(id);
      if (id !== this.pinnedDocument) this.pendingDocuments = this.pendingDocuments.filter(item => item !== id);
      this.drain();
    }
  }

  measure(id: string, height: number): void {
    if (Number.isFinite(height) && height > 0 && this.book()?.documents.some(document => document.document_id === id)) {
      this.heights.update(heights => ({ ...heights, [id]: Math.ceil(height) }));
    }
  }

  private queueDocument(id: string, priority: boolean): void {
    const book = this.book();
    if (!book || this.documents()[id] || this.activeDocuments.has(id) || !book.documents.some(doc => doc.document_id === id && !doc.error_code)) return;
    if (!priority && this.documentErrors()[id]) return;
    this.pendingDocuments = this.pendingDocuments.filter(item => item !== id);
    if (priority) this.pendingDocuments.unshift(id);
    else this.pendingDocuments.push(id);
    this.drain();
  }

  private drain(): void {
    while (this.activeDocuments.size < 2 && this.pendingDocuments.length) {
      const id = this.pendingDocuments[0];
      if (Object.keys(this.documents()).length + this.activeDocuments.size >= 8) {
        const cached = Object.keys(this.documents());
        const victim = cached.find(key => !this.nearDocuments.has(key) && key !== this.pinnedDocument)
          ?? (id === this.pinnedDocument ? cached.find(key => key !== id) : undefined);
        if (!victim) break;
        this.documents.update(documents => { const next = { ...documents }; delete next[victim]; return next; });
      }
      this.pendingDocuments.shift();
      this.fetchDocument(id);
    }
    this.loadingDocuments.set(new Set([...this.activeDocuments, ...this.pendingDocuments]));
  }

  private fetchDocument(id: string): void {
    const book = this.book();
    if (!book) return;
    const generation = this.generation;
    this.activeDocuments.add(id);
    this.loadingDocuments.set(new Set(this.activeDocuments));
    this.documentErrors.update(errors => { const next = { ...errors }; delete next[id]; return next; });
    this.requests.add(this.api.document(book.repository_id, book.snapshot_sha, id, this.proposalId()).subscribe({
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
    this.drain();
  }
}
