import { inject, Injectable } from '@angular/core';
import { HttpClient } from '@angular/common/http';
import type { paths } from './generated/schema';

export type Profile = paths['/api/v1/me']['get']['responses'][200]['content']['application/json'];
export type AuthAvailability = paths['/api/v1/auth/status']['get']['responses'][200]['content']['application/json'];

@Injectable({ providedIn: 'root' })
export class SessionApi {
  private readonly http = inject(HttpClient);

  profile() { return this.http.get<Profile>('/api/v1/me'); }
  availability() { return this.http.get<AuthAvailability>('/api/v1/auth/status'); }
  logout() { return this.http.post<void>('/api/v1/auth/logout', null); }
}
