import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';
import { App } from './app';
import { signal } from '@angular/core';
import { SessionState } from './core/auth/session-state';

describe('Workspace navigation', () => {
  it('exposes workspaces and a keyboard skip link', async () => {
    const session = { status: signal('unconfigured'), profile: signal(null), logoutStatus: signal('idle'), logoutError: signal(null), refresh: vi.fn() };
    await TestBed.configureTestingModule({ imports: [App], providers: [provideRouter([]), { provide: SessionState, useValue: session }] }).compileComponents();
    const fixture = TestBed.createComponent(App);
    await fixture.whenStable();
    const element = fixture.nativeElement as HTMLElement;
    const destinations = [...element.querySelectorAll('nav a')].map(link => link.getAttribute('href'));
    expect(destinations).toEqual(['/specification', '/changes', '/history', '/settings']);
    expect(element.querySelector('.skip-link')?.getAttribute('href')).toBe('#main-content');
    expect(element.querySelector('#main-content')?.getAttribute('tabindex')).toBe('-1');
    expect(element.textContent).toContain('נדרשת הגדרת התחברות');
  });
});
