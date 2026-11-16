import { TestBed } from '@angular/core/testing';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { SessionState } from './session-state';
import type { Profile } from '../api/session-api';

const profile: Profile = {
  user_id: '00000000-0000-4000-8000-000000000001', display_name: 'Synthetic reader',
  memberships: [{ organization_id: '00000000-0000-4000-8000-000000000002', role: 'READER', review_domains: [] }],
  capabilities: { read_specification: true, create_proposal: false, review: false, merge: false, manage_policy: false },
};

describe('Session state', () => {
  let session: SessionState;
  let http: HttpTestingController;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideHttpClient(), provideHttpClientTesting()] });
    session = TestBed.inject(SessionState);
    http = TestBed.inject(HttpTestingController);
  });

  afterEach(() => http.verify());

  function signedIn(): void {
    session.refresh();
    http.expectOne('/api/v1/auth/status').flush({ login_available: true });
    http.expectOne('/api/v1/me').flush(profile);
    expect(session.status()).toBe('authenticated');
  }

  it('shows unconfigured login when no identity provider is configured', () => {
    session.refresh();
    http.expectOne('/api/v1/auth/status').flush({ login_available: false });
    http.expectOne('/api/v1/me').flush({}, { status: 401, statusText: 'Unauthorized' });
    expect(session.status()).toBe('unconfigured');
    expect(session.profile()).toBeNull();
  });

  it('distinguishes a service error from a signed-out user and supports retry', () => {
    session.refresh();
    http.expectOne('/api/v1/auth/status').flush({ login_available: true });
    http.expectOne('/api/v1/me').flush({}, { status: 503, statusText: 'Unavailable' });
    expect(session.status()).toBe('error');
    session.refresh();
    http.expectOne('/api/v1/auth/status').flush({ login_available: true });
    http.expectOne('/api/v1/me').flush({}, { status: 401, statusText: 'Unauthorized' });
    expect(session.status()).toBe('anonymous');
  });

  it('retains the current identity after a failed logout and prevents double submission', () => {
    signedIn();
    session.logout();
    session.logout();
    const request = http.expectOne('/api/v1/auth/logout');
    expect(request.request.method).toBe('POST');
    request.flush({ error: { message_he: 'השירות אינו זמין כרגע' } }, { status: 503, statusText: 'Unavailable' });
    expect(session.profile()?.user_id).toBe(profile.user_id);
    expect(session.status()).toBe('authenticated');
    expect(session.logoutStatus()).toBe('error');
    expect(session.logoutError()).toBe('השירות אינו זמין כרגע');
    expect(session.logoutEpoch()).toBe(0);
  });

  it('honors active editor logout guards and emits a purge epoch only after successful logout', () => {
    signedIn();
    const release = session.guardLogout(() => false);
    session.logout(); http.expectNone('/api/v1/auth/logout');
    expect(session.profile()?.user_id).toBe(profile.user_id); expect(session.logoutEpoch()).toBe(0);
    release(); session.logout(); http.expectOne('/api/v1/auth/logout').flush(null);
    expect(session.profile()).toBeNull(); expect(session.logoutEpoch()).toBe(1);
  });

  it('cancels stale identity requests when logout completes', () => {
    signedIn();
    session.logout();
    const logout = http.expectOne('/api/v1/auth/logout');
    session.refresh();
    const availability = http.expectOne('/api/v1/auth/status');
    const oldProfile = http.expectOne('/api/v1/me');
    logout.flush(null);
    expect(availability.cancelled).toBe(true);
    expect(oldProfile.cancelled).toBe(true);
    expect(session.status()).toBe('anonymous');
    expect(session.profile()).toBeNull();
  });
});
