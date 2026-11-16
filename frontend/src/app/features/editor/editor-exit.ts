import type { CanDeactivateFn } from '@angular/router';
import type { EditorPage } from './editor-page';

export const editorExit: CanDeactivateFn<EditorPage> = (component, current, _state, next) => {
  const url = new URL(next.url, 'https://specspace.invalid');
  const path = `/changes/${encodeURIComponent(current.params['id'])}/edit/${encodeURIComponent(current.params['document'])}`;
  // Section/focus changes reuse the exact document and local buffer. A document
  // or base change runs the loss guard, except the page's own verified save
  // promotion, which retains this exact document and its later local edits.
  return url.pathname === path && (url.searchParams.get('commit') === current.queryParamMap.get('commit')
    || component.canFollowSavedBase(current.params['id'], current.params['document'], url.searchParams.get('commit'))) || component.canLeave();
};
