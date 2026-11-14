import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { App } from './app';

describe('Workspace navigation', () => {
  it('exposes workspaces and a keyboard skip link', async () => {
    await TestBed.configureTestingModule({ imports: [App], providers: [provideRouter([])] }).compileComponents();
    const fixture = TestBed.createComponent(App);
    await fixture.whenStable();
    const element = fixture.nativeElement as HTMLElement;
    const destinations = [...element.querySelectorAll('nav a')].map(link => link.getAttribute('href'));
    expect(destinations).toEqual(['/specification', '/changes', '/history', '/settings']);
    expect(element.querySelector('.skip-link')?.getAttribute('href')).toBe('#main-content');
    expect(element.querySelector('#main-content')?.getAttribute('tabindex')).toBe('-1');
  });
});
