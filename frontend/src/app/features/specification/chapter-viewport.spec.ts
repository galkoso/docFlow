import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { ChapterViewport } from './chapter-viewport';

@Component({
  imports: [ChapterViewport],
  template: `<article appChapterViewport [loaded]="loaded" [placeholderHeight]="900" (near)="near = $event" (measured)="height = $event"></article>`,
})
class Host { loaded = false; near = false; height = 0; }

describe('Chapter viewport lifecycle', () => {
  it('loads near the viewport, measures rendered chapters, and disconnects observers', async () => {
    let intersect!: IntersectionObserverCallback;
    let resize!: ResizeObserverCallback;
    const intersectionDisconnect = vi.fn();
    const resizeDisconnect = vi.fn();
    vi.stubGlobal('IntersectionObserver', class {
      constructor(callback: IntersectionObserverCallback, options: IntersectionObserverInit) {
        intersect = callback;
        expect(options.rootMargin).toBe('800px 0px');
      }
      observe = vi.fn();
      disconnect = intersectionDisconnect;
    });
    vi.stubGlobal('ResizeObserver', class {
      constructor(callback: ResizeObserverCallback) { resize = callback; }
      observe = vi.fn();
      disconnect = resizeDisconnect;
    });
    try {
      await TestBed.configureTestingModule({ imports: [Host] }).compileComponents();
      const fixture = TestBed.createComponent(Host);
      fixture.detectChanges();
      const article: HTMLElement = fixture.nativeElement.querySelector('article');
      vi.spyOn(article, 'getBoundingClientRect').mockReturnValue({ height: 1500 } as DOMRect);
      expect(article.style.minHeight).toBe('900px');
      intersect([{ isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver);
      expect(fixture.componentInstance.near).toBe(true);
      resize([], {} as ResizeObserver);
      expect(fixture.componentInstance.height).toBe(0);
      fixture.componentInstance.loaded = true;
      fixture.detectChanges();
      resize([], {} as ResizeObserver);
      expect(fixture.componentInstance.height).toBe(1500);
      expect(article.style.minHeight).toBe('');
      fixture.destroy();
      expect(intersectionDisconnect).toHaveBeenCalledOnce();
      expect(resizeDisconnect).toHaveBeenCalledOnce();
    } finally { vi.unstubAllGlobals(); }
  });
});
