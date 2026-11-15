import { signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, convertToParamMap, provideRouter, Router } from '@angular/router';
import { BehaviorSubject } from 'rxjs';
import { SessionState } from '../../core/auth/session-state';
import { Proposals } from './proposals';

describe('Proposal list navigation scope', () => {
  it('restores filters from the URL and cancels pending search navigation when identity changes', () => {
    vi.useFakeTimers();
    const profile = signal<{ user_id: string } | null>({ user_id: 'actor' });
    const params = new BehaviorSubject(convertToParamMap({ q: 'Alpha', state: 'DRAFT', repository_id: 'repo' }));
    TestBed.configureTestingModule({ imports: [Proposals], providers: [provideHttpClient(), provideHttpClientTesting(), provideRouter([]),
      { provide: SessionState, useValue: { profile, status: signal('authenticated') } },
      { provide: ActivatedRoute, useValue: { queryParamMap: params.asObservable(), snapshot: { queryParamMap: params.value } } },
    ] });
    const fixture = TestBed.createComponent(Proposals);
    const http = TestBed.inject(HttpTestingController);
    const navigate = vi.spyOn(TestBed.inject(Router), 'navigate').mockResolvedValue(true);
    try {
      fixture.detectChanges();
      http.expectOne('/api/v1/proposals/options').flush({ repositories: [], owners: [], teams: [] });
      const request = http.expectOne(req => req.url === '/api/v1/proposals');
      expect(request.request.params.get('q')).toBe('Alpha'); expect(request.request.params.get('state')).toBe('DRAFT');
      expect(request.request.params.get('repository_id')).toBe('repo'); request.flush({ proposals: [] }); fixture.detectChanges();
      fixture.componentInstance.search('private draft query'); profile.set(null); fixture.detectChanges();
      vi.advanceTimersByTime(1000); expect(navigate).not.toHaveBeenCalled();
      expect(fixture.componentInstance.state.choices()).toBeNull(); expect(fixture.componentInstance.state.rows()).toEqual([]);
      http.verify();
    } finally { fixture.destroy(); vi.useRealTimers(); vi.restoreAllMocks(); }
  });
});
