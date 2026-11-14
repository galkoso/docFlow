import { inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { catchError, forkJoin, of, Subscription, throwError } from 'rxjs';
import { Profile, SessionApi } from '../api/session-api';

type SessionStatus = 'loading' | 'authenticated' | 'anonymous' | 'unconfigured' | 'error';

@Injectable({ providedIn: 'root' })
export class SessionState {
  private readonly api = inject(SessionApi);
  private readonly currentStatus = signal<SessionStatus>('loading');
  private readonly currentProfile = signal<Profile | null>(null);
  private readonly currentLogoutStatus = signal<'idle' | 'loading' | 'error'>('idle');
  private readonly currentLogoutError = signal<string | null>(null);
  private activeRefresh?: Subscription;
  private generation = 0;

  readonly status = this.currentStatus.asReadonly();
  readonly profile = this.currentProfile.asReadonly();
  readonly logoutStatus = this.currentLogoutStatus.asReadonly();
  readonly logoutError = this.currentLogoutError.asReadonly();

  refresh(): void {
    const generation = ++this.generation;
    this.activeRefresh?.unsubscribe();
    this.currentStatus.set('loading');
    this.currentProfile.set(null);
    this.activeRefresh = forkJoin({
      availability: this.api.availability(),
      profile: this.api.profile().pipe(catchError((error: HttpErrorResponse) => error.status === 401 ? of(null) : throwError(() => error))),
    }).subscribe({
      next: ({ availability, profile }) => {
        if (generation !== this.generation) return;
        this.currentProfile.set(profile);
        this.currentStatus.set(profile ? 'authenticated' : availability.login_available ? 'anonymous' : 'unconfigured');
      },
      error: () => {
        if (generation !== this.generation) return;
        this.currentProfile.set(null);
        this.currentStatus.set('error');
      },
    });
  }

  logout(): void {
    if (!this.profile() || this.logoutStatus() === 'loading') return;
    this.currentLogoutStatus.set('loading');
    this.currentLogoutError.set(null);
    this.api.logout().subscribe({
      next: () => {
        ++this.generation;
        this.activeRefresh?.unsubscribe();
        this.currentProfile.set(null);
        this.currentStatus.set('anonymous');
        this.currentLogoutStatus.set('idle');
      },
      error: (error: HttpErrorResponse) => {
        this.currentLogoutStatus.set('error');
        const message: unknown = error.error?.error?.message_he;
        this.currentLogoutError.set(typeof message === 'string' ? message : 'ההתנתקות לא הושלמה. אפשר לנסות שוב.');
      },
    });
  }
}
