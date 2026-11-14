import { ChangeDetectionStrategy, Component, computed, input } from '@angular/core';
import { NgTemplateOutlet } from '@angular/common';
import { externalLink, renderTree, textParts, RenderEvent, SearchHit } from './render-model';

@Component({
  selector: 'app-document-renderer',
  imports: [NgTemplateOutlet],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: './document-renderer.html',
  styles: `
    :host { display: block; overflow-wrap: anywhere; line-height: 1.8; }
    pre { overflow: auto; padding: 1rem; background: #f0f3f6; direction: ltr; text-align: left; }
    blockquote { border-inline-start: 3px solid #b3c7cc; margin-inline: 0; padding-inline-start: 1rem; }
    .table-scroll { overflow-x: auto; } table { border-collapse: collapse; width: 100%; }
    th, td { border: 1px solid #cdd7df; padding: .5rem .75rem; text-align: start; }
    th { background: #f0f3f6; } mark { background: #fff0a6; color: #142333; }
    mark.active { background: #ffd15c; outline: 2px solid #8d6413; }
    .unsupported { color: #765217; border: 1px solid #dcc78f; padding: .5rem; }
    h1, h2, h3, h4, h5, h6 { scroll-margin-top: 7rem; }
  `,
})
export class DocumentRenderer {
  readonly events = input.required<RenderEvent[]>();
  readonly hits = input<SearchHit[]>([]);
  readonly activeMatch = input<string | null>(null);
  readonly sectionPrefix = input('');
  readonly internalLinks = input<Record<string, string>>({});
  readonly tree = computed(() => {
    try { return { nodes: renderTree(this.events()), error: false }; }
    catch { return { nodes: [], error: true }; }
  });
  readonly parts = textParts;
  readonly external = externalLink;
  internal(destination?: string | null): string | null {
    const link = destination ? this.internalLinks()[destination] : undefined;
    return link?.startsWith('/specification?') ? link : null;
  }
}
