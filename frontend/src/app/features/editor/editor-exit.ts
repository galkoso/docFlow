import type { CanDeactivateFn } from '@angular/router';
import type { EditorPage } from './editor-page';

export const editorExit: CanDeactivateFn<EditorPage> = (component, current, _state, next) => {
  const url = new URL(next.url, 'https://specspace.invalid');
  const path = `/changes/${encodeURIComponent(current.params['id'])}/edit/${encodeURIComponent(current.params['document'])}`;
  // Section/focus changes reuse the exact document and local buffer. A document
  // or base change must run the same loss guard as leaving the editor entirely.
  return url.pathname === path && url.searchParams.get('commit') === current.queryParamMap.get('commit') || component.canLeave();
};
