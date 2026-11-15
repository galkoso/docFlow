import { Routes } from '@angular/router';
import { creationExit } from './features/proposals/creation-exit';

export const routes: Routes = [
  { path: '', pathMatch: 'full', redirectTo: 'specification' },
  { path: 'specification', title: 'האפיון המאושר | SpecSpace', loadComponent: () => import('./features/specification/specification').then(m => m.Specification) },
  { path: 'changes', title: 'הצעות שינוי | SpecSpace', loadComponent: () => import('./features/proposals/proposals').then(m => m.Proposals) },
  { path: 'changes/new', title: 'פתיחת הצעה | SpecSpace', canDeactivate: [creationExit], loadComponent: () => import('./features/proposals/proposal-create').then(m => m.ProposalCreate) },
  { path: 'changes/:id', title: 'פרטי הצעה | SpecSpace', loadComponent: () => import('./features/proposals/proposal-detail').then(m => m.ProposalDetailPage) },
  { path: 'changes/:id/book', title: 'תוכן ההצעה | SpecSpace', loadComponent: () => import('./features/specification/specification').then(m => m.Specification) },
  { path: 'history', title: 'היסטוריית האפיון | SpecSpace', loadComponent: () => import('./features/history/history').then(m => m.History) },
  { path: 'settings', title: 'ניהול סביבת העבודה | SpecSpace', loadComponent: () => import('./features/settings/settings').then(m => m.Settings) },
  { path: '**', redirectTo: 'specification' },
];
