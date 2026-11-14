import { Routes } from '@angular/router';

export const routes: Routes = [
  { path: '', pathMatch: 'full', redirectTo: 'specification' },
  { path: 'specification', title: 'האפיון המאושר | SpecSpace', loadComponent: () => import('./features/specification/specification').then(m => m.Specification) },
  { path: 'changes', title: 'הצעות שינוי | SpecSpace', loadComponent: () => import('./features/proposals/proposals').then(m => m.Proposals) },
  { path: 'history', title: 'היסטוריית האפיון | SpecSpace', loadComponent: () => import('./features/history/history').then(m => m.History) },
  { path: 'settings', title: 'ניהול סביבת העבודה | SpecSpace', loadComponent: () => import('./features/settings/settings').then(m => m.Settings) },
  { path: '**', redirectTo: 'specification' },
];
