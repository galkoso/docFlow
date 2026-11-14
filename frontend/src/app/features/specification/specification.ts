import { ChangeDetectionStrategy, Component, computed, effect, inject, OnDestroy, untracked } from '@angular/core';
import { DatePipe } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { BookState } from './book-state';
import { DocumentRenderer } from './document-renderer';

@Component({
  selector: 'app-specification',
  imports: [DatePipe, RouterLink, DocumentRenderer],
  providers: [BookState],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './specification.html',
  styleUrl: './specification.css',
})
export class Specification implements OnDestroy {
  readonly session = inject(SessionState);
  readonly state = inject(BookState);
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  readonly params = toSignal(this.route.queryParamMap, { initialValue: this.route.snapshot.queryParamMap });
  private readonly selection = computed(() => JSON.stringify([this.params().get('repo'), this.params().get('commit')]));
  readonly links = computed(() => {
    const book = this.state.book();
    const links: Record<string, string> = {};
    if (!book) return links;
    for (const document of book.documents) {
      for (const section of ['', ...document.sections.map(item => item.section_id)]) {
        const params = new URLSearchParams({ repo: book.repository_id, commit: book.snapshot_sha, doc: document.document_id });
        if (section) params.set('section', section);
        const href = `/specification?${params}`;
        const suffix = section ? `#${section}` : '';
        links[`ss:${document.document_id}${suffix}`] = href;
        links[`${document.path}${suffix}`] = href;
      }
    }
    return links;
  });

  constructor() {
    effect(() => {
      const profile = this.session.profile();
      this.selection();
      untracked(() => profile ? this.retry() : this.state.clear());
    });
    effect(() => {
      const document = this.params().get('doc');
      const section = this.params().get('section') ?? '';
      const book = this.state.book();
      const loaded = this.state.documents();
      if (!document || !book?.documents.some(item => item.document_id === document)) return;
      if (!loaded[document]) untracked(() => this.state.loadDocument(document));
      else requestAnimationFrame(() => {
        const target = window.document.getElementById(this.sectionId(document, section));
        target?.scrollIntoView({ block: 'start' });
        target?.focus({ preventScroll: true });
      });
    });
  }

  ngOnDestroy() { this.state.clear(); }
  retry() { this.state.open(this.params().get('repo') ?? undefined, this.params().get('commit') ?? undefined); }
  latest() { void this.router.navigate([], { relativeTo: this.route, queryParams: { commit: null }, queryParamsHandling: 'merge' }); }
  selectRepository(value: string) { void this.router.navigate([], { relativeTo: this.route, queryParams: { repo: value } }); }
  sectionId(document: string, section = '') { return `chapter-${document}-${section}`; }
  jump(document: string, section = '') {
    const book = this.state.book();
    if (!book) return;
    void this.router.navigate([], { relativeTo: this.route,
      queryParams: { repo: book.repository_id, commit: this.params().get('commit'), doc: document, section: section || null } });
  }
}
