import { Directive, ElementRef, inject, input } from '@angular/core';
import type { MatchWindow } from '../../core/api/book-api';

@Directive({ selector: '[appTextWindow]' })
export class TextWindow {
  readonly window = input<MatchWindow | null>(null);
  readonly element = inject<ElementRef<HTMLElement>>(ElementRef).nativeElement;
}
