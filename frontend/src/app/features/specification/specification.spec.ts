import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, convertToParamMap, provideRouter, Router } from '@angular/router';
import { BehaviorSubject } from 'rxjs';
import { SessionState } from '../../core/auth/session-state';
import { Specification } from './specification';

describe('Authorized reader page', () => {
  it('keeps the selected snapshot when only the section changes, then clears the page on logout', async () => {
    const profile = signal<object | null>({ user_id: 'reader' });
    const params = new BehaviorSubject(convertToParamMap({ repo: 'repo' }));
    await TestBed.configureTestingModule({
      imports: [Specification],
      providers: [provideHttpClient(), provideHttpClientTesting(), provideRouter([]),
        { provide: SessionState, useValue: { profile } },
        { provide: ActivatedRoute, useValue: { queryParamMap: params, snapshot: { queryParamMap: params.value } } }],
    }).compileComponents();
    const fixture = TestBed.createComponent(Specification);
    vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    const http = TestBed.inject(HttpTestingController);
    fixture.detectChanges();
    http.expectOne('/api/v1/repositories').flush({ repositories: [{ repository_id: 'repo', default_branch: 'main' }] });
    http.expectOne('/api/v1/repositories/repo/specification').flush({ repository_id: 'repo', branch: 'main',
      snapshot_sha: 'a'.repeat(40), version_selected: false, updated_at: '2026-11-14T00:00:00Z', findings: [],
      documents: [{ document_id: 'D1', title: 'הפרק הראשון', path: 'docs/one.md', sections: [] },
        { document_id: 'D2', title: 'הפרק השני', path: 'docs/two.md', sections: [] }] });
    http.expectOne(`/api/v1/repositories/repo/documents/D1?commit=${'a'.repeat(40)}`).flush({
      document_id: 'D1', snapshot_sha: 'a'.repeat(40), title: 'הפרק הראשון', blocks: [], render_events: [
        { kind: 'text', text: 'תוכן מורשה פרטי', source_start_byte: 0, source_end_byte: 1 },
      ],
    });
    await fixture.whenStable();
    expect(fixture.nativeElement.textContent).toContain('תוכן מורשה פרטי');
    const shortcut = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, cancelable: true });
    document.dispatchEvent(shortcut);
    fixture.detectChanges();
    expect(shortcut.defaultPrevented).toBe(true);
    expect(fixture.nativeElement.querySelector('#book-find')).not.toBeNull();
    const externalInput = document.createElement('input');
    document.body.append(externalInput);
    const externalShortcut = new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, cancelable: true, bubbles: true });
    externalInput.dispatchEvent(externalShortcut);
    expect(externalShortcut.defaultPrevented).toBe(false);
    externalInput.remove();
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', cancelable: true }));
    fixture.detectChanges();
    expect(fixture.nativeElement.querySelector('#book-find')).toBeNull();
    expect(fixture.nativeElement.textContent).toContain('תוכן מורשה פרטי');
    params.next(convertToParamMap({ repo: 'repo', doc: 'D2', section: 'S2' }));
    fixture.detectChanges();
    http.expectNone('/api/v1/repositories');
    const pending = http.expectOne(`/api/v1/repositories/repo/documents/D2?commit=${'a'.repeat(40)}`);
    profile.set(null);
    fixture.detectChanges();
    await fixture.whenStable();
    expect(pending.cancelled).toBe(true);
    expect(fixture.nativeElement.textContent).not.toContain('תוכן מורשה פרטי');
    expect(fixture.nativeElement.querySelector('.book-pages')).toBeNull();
    expect(fixture.nativeElement.textContent).toContain('יש להתחבר');
    http.verify();
  });
});
