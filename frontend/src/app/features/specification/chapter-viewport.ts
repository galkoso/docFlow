import { Directive, effect, ElementRef, inject, input, OnDestroy, OnInit, output } from '@angular/core';

@Directive({
  selector: '[appChapterViewport]',
  host: { '[style.min-height.px]': 'loaded() ? null : placeholderHeight()' },
})
export class ChapterViewport implements OnInit, OnDestroy {
  readonly loaded = input(false);
  readonly placeholderHeight = input(320);
  readonly near = output<boolean>();
  readonly measured = output<number>();
  private readonly element = inject<ElementRef<HTMLElement>>(ElementRef).nativeElement;
  private intersection?: IntersectionObserver;
  private resize?: ResizeObserver;
  private frame = 0;

  constructor() {
    effect(() => {
      if (this.loaded()) {
        this.frame = requestAnimationFrame(() => this.measure());
      }
    });
  }

  ngOnInit() {
    if (typeof IntersectionObserver !== 'undefined') {
      this.intersection = new IntersectionObserver(entries => {
        for (const entry of entries) this.near.emit(entry.isIntersecting);
      }, { rootMargin: '800px 0px' });
      this.intersection.observe(this.element);
    }
    if (typeof ResizeObserver !== 'undefined') {
      this.resize = new ResizeObserver(() => this.measure());
      this.resize.observe(this.element);
    }
  }

  ngOnDestroy() {
    this.intersection?.disconnect();
    this.resize?.disconnect();
    cancelAnimationFrame(this.frame);
  }

  private measure() {
    if (this.loaded()) this.measured.emit(this.element.getBoundingClientRect().height);
  }
}
