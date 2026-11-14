import {
  ChangeDetectionStrategy, Component, computed, effect, ElementRef, HostListener, inject, OnDestroy, signal, untracked, viewChild,
} from '@angular/core';
import { DatePipe } from '@angular/common';
import { ActivatedRoute, Router, RouterLink } from '@angular/router';
import { toSignal } from '@angular/core/rxjs-interop';
import { SessionState } from '../../core/auth/session-state';
import { BookState } from './book-state';
import { DocumentRenderer } from './document-renderer';
import { ChapterViewport } from './chapter-viewport';
import { FindState } from './find-state';
import type { SearchHit } from './render-model';

@Component({
  selector: 'app-specification',
  imports: [DatePipe, RouterLink, DocumentRenderer, ChapterViewport],
  providers: [BookState, FindState],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './specification.html',
  styleUrl: './specification.css',
})
export class Specification implements OnDestroy {
  readonly session = inject(SessionState);
  readonly state = inject(BookState);
  readonly find = inject(FindState);
  readonly findOpen = signal(false);
  private readonly findInput = viewChild<ElementRef<HTMLInputElement>>('findInput');
  private readonly route = inject(ActivatedRoute);
  private readonly router = inject(Router);
  private lastJump = '';
  private lastMatch = '';
  readonly chapterHits = computed(() => {
    const result: Record<string, SearchHit[]> = {};
    for (const hit of this.find.hits()) (result[hit.document_id] ??= []).push(hit);
    return result;
  });
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
      untracked(() => {
        if (profile) this.retry();
        else { this.state.clear(); this.find.clear(); this.findOpen.set(false); }
      });
    });
    effect(() => {
      const book = this.state.book();
      untracked(() => this.find.scope(book?.repository_id, book?.snapshot_sha));
    });
    effect(() => {
      const query = this.params().get('q') ?? '';
      untracked(() => { this.find.search(query); if (query) this.findOpen.set(true); });
    });
    effect(() => {
      const match = this.find.current();
      const documents = this.state.documents();
      if (!match) { this.lastMatch = ''; return; }
      if (this.lastMatch === match.match_id) return;
      if (!documents[match.document_id]) untracked(() => this.state.loadDocument(match.document_id));
      else if (this.lastMatch !== match.match_id) {
        this.lastMatch = match.match_id;
        requestAnimationFrame(() => window.document.querySelector(`[data-match-id="${CSS.escape(match.match_id)}"]`)?.scrollIntoView({ block: 'center' }));
      }
    });
    effect(() => {
      const document = this.params().get('doc');
      const section = this.params().get('section') ?? '';
      const book = this.state.book();
      const loaded = this.state.documents();
      if (!document) { this.lastJump = ''; return; }
      if (!book?.documents.some(item => item.document_id === document)) return;
      if (this.lastJump === `${book.snapshot_sha}/${document}/${section}`) return;
      if (!loaded[document]) untracked(() => this.state.loadDocument(document));
      else if (this.lastJump !== `${book.snapshot_sha}/${document}/${section}`) {
        this.lastJump = `${book.snapshot_sha}/${document}/${section}`;
        requestAnimationFrame(() => {
        const target = window.document.getElementById(this.sectionId(document, section));
        target?.scrollIntoView({ block: 'start' });
        target?.focus({ preventScroll: true });
        });
      }
    });
  }

  ngOnDestroy() { this.state.clear(); this.find.clear(); }
  retry() { this.state.open(this.params().get('repo') ?? undefined, this.params().get('commit') ?? undefined); }
  latest() { void this.router.navigate([], { relativeTo: this.route, queryParams: { commit: null }, queryParamsHandling: 'merge' }); }
  selectRepository(value: string) { void this.router.navigate([], { relativeTo: this.route, queryParams: { repo: value } }); }
  openFind() {
    this.findOpen.set(true);
    requestAnimationFrame(() => { this.findInput()?.nativeElement.focus(); this.findInput()?.nativeElement.select(); });
  }
  closeFind() {
    this.findOpen.set(false);
    this.updateQuery('');
  }
  updateQuery(query: string) {
    this.find.search(query);
    void this.router.navigate([], { relativeTo: this.route, queryParams: { q: query || null }, queryParamsHandling: 'merge', replaceUrl: true });
  }
  findKey(event: KeyboardEvent) {
    if (event.key === 'Enter' || event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      this.find.move(event.key === 'ArrowUp' || (event.key === 'Enter' && event.shiftKey) ? -1 : 1);
    }
  }
  @HostListener('document:keydown', ['$event'])
  keyboard(event: KeyboardEvent) {
    if (event.defaultPrevented) return;
    if (event.key === 'Escape' && this.findOpen()) { event.preventDefault(); this.closeFind(); return; }
    const target = event.target instanceof HTMLElement ? event.target : null;
    if (target?.closest('input, textarea, [contenteditable="true"]') && target !== this.findInput()?.nativeElement) return;
    if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === 'f' && this.state.book()) {
      event.preventDefault(); this.openFind();
    }
  }
  sectionId(document: string, section = '') { return `chapter-${document}-${section}`; }
  jump(document: string, section = '') {
    const book = this.state.book();
    if (!book) return;
    void this.router.navigate([], { relativeTo: this.route,
      queryParams: { repo: book.repository_id, commit: this.params().get('commit'), doc: document, section: section || null } });
  }
}
