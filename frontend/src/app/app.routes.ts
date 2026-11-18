import { Routes } from '@angular/router';
import { creationExit } from './features/proposals/creation-exit';
import { editorExit } from './features/editor/editor-exit';
import { reviewExit } from './features/review/review-exit';
import { reviewSnapshotExit } from './features/review/review-snapshot-exit';

export const routes: Routes = [
  { path: '', pathMatch: 'full', redirectTo: 'specification' },
  { path: 'specification', title: 'האפיון המאושר | SpecSpace', loadComponent: () => import('./features/specification/specification').then(m => m.Specification) },
  { path: 'changes', title: 'הצעות שינוי | SpecSpace', loadComponent: () => import('./features/proposals/proposals').then(m => m.Proposals) },
  { path: 'changes/new', title: 'פתיחת הצעה | SpecSpace', canDeactivate: [creationExit], loadComponent: () => import('./features/proposals/proposal-create').then(m => m.ProposalCreate) },
  { path: 'changes/:id', title: 'פרטי הצעה | SpecSpace', loadComponent: () => import('./features/proposals/proposal-detail').then(m => m.ProposalDetailPage) },
  { path: 'changes/:id/diff', title: 'השוואת השינוי | SpecSpace', loadComponent: () => import('./features/comparison/comparison-page').then(m => m.ComparisonPage) },
  { path: 'changes/:id/review', title: 'הגדרת סקירה | SpecSpace', canDeactivate: [reviewExit],
    loadComponent: () => import('./features/review/review-configuration-page').then(m => m.ReviewConfigurationPage) },
  { path: 'changes/:id/review/snapshots', title: 'גרסאות סקירה | SpecSpace', canDeactivate: [reviewSnapshotExit],
    loadComponent: () => import('./features/review/review-snapshots-page').then(m => m.ReviewSnapshotsPage) },
  { path: 'changes/:id/book', title: 'תוכן ההצעה | SpecSpace', loadComponent: () => import('./features/specification/specification').then(m => m.Specification) },
  { path: 'changes/:id/edit/:document', title: 'עריכת הצעה | SpecSpace', canDeactivate: [editorExit], runGuardsAndResolvers: 'paramsOrQueryParamsChange',
    loadComponent: () => import('./features/editor/editor-page').then(m => m.EditorPage) },
  { path: 'history', title: 'היסטוריית האפיון | SpecSpace', loadComponent: () => import('./features/history/history').then(m => m.History) },
  { path: 'settings', title: 'ניהול סביבת העבודה | SpecSpace', loadComponent: () => import('./features/settings/settings').then(m => m.Settings) },
  { path: '**', redirectTo: 'specification' },
];
