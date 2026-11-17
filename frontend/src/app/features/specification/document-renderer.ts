import { afterRenderEffect, ChangeDetectionStrategy, Component, computed, effect, inject, input, output, signal, untracked, viewChildren } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { assetSource, AssetContext, externalLink, renderTree, textParts, RenderEvent, SearchHit, TextChange, TextHighlight, TextPart }
  from './render-model';
import type { MatchWindow } from '../../core/api/book-api';
import { TextWindow } from './text-window';
import { HighlightContext, VisibleHighlights } from './visible-highlights';

@Component({
  selector: 'app-document-renderer',
  imports: [NgTemplateOutlet, TextWindow],
  providers: [VisibleHighlights],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './document-renderer.html',
  styles: `
    :host { display: block; overflow-wrap: anywhere; line-height: 1.8; }
    pre { overflow: auto; padding: 1rem; background: #f0f3f6; direction: ltr; text-align: left; }
    blockquote { border-inline-start: 3px solid #b3c7cc; margin-inline: 0; padding-inline-start: 1rem; }
    .table-scroll { overflow-x: auto; } table { border-collapse: collapse; width: 100%; }
    th, td { border: 1px solid #cdd7df; padding: .5rem .75rem; text-align: start; }
    th { background: #f0f3f6; } mark { background: #fff0a6; color: #142333; }
    ins.diff-added { background: #c9f1d3; color: #173d22; text-decoration: underline; }
    del.diff-removed { background: #fbd1ce; color: #66231f; text-decoration: line-through; }
    mark.active { background: #ffd15c; outline: 2px solid #8d6413; }
    mark.line-break { display: inline-block; min-width: .4em; }
    .unsupported { color: #765217; border: 1px solid #dcc78f; padding: .5rem; }
    img { display: block; max-width: 100%; height: auto; margin-block: 1rem; }
    h1, h2, h3, h4, h5, h6 { scroll-margin-top: 7rem; }
  `,
})
export class DocumentRenderer {
  readonly events = input.required<RenderEvent[]>();
  readonly hits = input<SearchHit[]>([]);
  readonly changes = input<TextChange[]>([]);
  readonly activeMatch = input<string | null>(null);
  readonly sectionPrefix = input('');
  readonly internalLinks = input<Record<string, string>>({});
  readonly assetContext = input<AssetContext | null>(null);
  readonly highlightContext = input<HighlightContext | null>(null);
  readonly query = input('');
  readonly accessDenied = output<void>();
  readonly visible = inject(VisibleHighlights);
  private readonly windows = viewChildren(TextWindow);
  readonly failedImages = signal(new Set<string>());
  readonly tree = computed(() => {
    try { return { nodes: renderTree(this.events()), error: false }; }
    catch { return { nodes: [], error: true }; }
  });
  readonly parts = computed(() => {
    const unique = new Map<string, TextHighlight>();
    for (const hit of [...this.visible.hits(), ...this.hits()]) unique.set(hit.match_id, hit);
    const byBlock = new Map<string, TextHighlight[]>();
    for (const hit of unique.values()) {
      const hits = byBlock.get(hit.block_id) ?? [];
      hits.push(hit); byBlock.set(hit.block_id, hits);
    }
    const parts = new Map<RenderEvent, TextPart[]>();
    const pending = [...this.tree().nodes];
    while (pending.length) {
      const node = pending.pop()!;
      for (const child of node.children) pending.push(child);
      for (const chunk of node.chunks ?? []) parts.set(chunk.event, textParts(chunk.event, byBlock.get(chunk.event.block_id ?? '') ?? [], this.changes()));
    }
    return parts;
  });
  readonly external = externalLink;

  constructor() {
    effect(() => {
      const context = this.highlightContext();
      const query = this.query();
      untracked(() => this.visible.configure(context, query));
    });
    effect(() => { if (this.visible.denied()) this.accessDenied.emit(); });
    afterRenderEffect(onCleanup => {
      const chunks = this.windows().map(chunk => ({ element: chunk.element, window: chunk.window() }));
      if (typeof IntersectionObserver === 'undefined') {
        if (this.query() && this.highlightContext()) this.visible.error.set('הדפדפן אינו תומך בסימון טקסט לפי אזור התצוגה.');
        return;
      }
      const windows = new Map<Element, MatchWindow>();
      for (const chunk of chunks) if (chunk.window) windows.set(chunk.element, chunk.window);
      const observer = new IntersectionObserver(entries => {
        this.visible.viewports(entries.flatMap(entry => {
          const window = windows.get(entry.target);
          return window ? [{ window, near: entry.isIntersecting }] : [];
        }));
      }, { rootMargin: '300px 0px' });
      for (const element of windows.keys()) observer.observe(element);
      onCleanup(() => { observer.disconnect(); this.visible.unobserve(); });
    });
  }
  imageSource(destination?: string | null) { return assetSource(this.assetContext(), destination); }
  imageFailed(source: string) { this.failedImages.update(failed => new Set(failed).add(source)); }
  retryImage(source: string) {
    this.failedImages.update(failed => { const next = new Set(failed); next.delete(source); return next; });
  }
  internal(destination?: string | null): string | null {
    const link = destination ? this.internalLinks()[destination] : undefined;
    return link && (link.startsWith('/specification?') || /^\/changes\/[a-f0-9-]{36}\/book\?/.test(link)) ? link : null;
  }
}
