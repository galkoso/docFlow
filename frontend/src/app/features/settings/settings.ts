import { ChangeDetectionStrategy, Component, inject } from '@angular/core';
import { RouterLink } from '@angular/router';
import { SessionState } from '../../core/auth/session-state';

@Component({
  selector: 'app-settings',
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <h1>ניהול סביבת העבודה</h1>
    <p class="page-description">חיבור מקור הידע, זהויות ומדיניות עבודה.</p>
    @if (session.status() === 'unconfigured') {
      <div class="notice" role="status">להפעלת ההתחברות הארגונית נדרשת הגדרה ראשונית של סביבת העבודה.</div>
    }
    @if (session.status() === 'authenticated') {
      <div class="notice" role="status">ההתחברות פעילה. חיבור מאגר האפיון עדיין לא הוגדר.</div>
    }
    <section class="empty-state">
      <h2>חיבור מאגר האפיון</h2>
      <p>מנהל סביבת העבודה יוכל לחבר מאגר פרטי, להקצות צוותים וסוקרים ולהגדיר מדיניות ייצוא.</p>
      <a class="button" routerLink="/settings">הגדרת סביבת העבודה</a>
    </section>
  `,
})
export class Settings { protected readonly session = inject(SessionState); }
