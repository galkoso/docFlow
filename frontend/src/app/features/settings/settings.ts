import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterLink } from '@angular/router';

@Component({
  selector: 'app-settings',
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <h1>ניהול סביבת העבודה</h1>
    <p class="page-description">חיבור מקור הידע, זהויות ומדיניות עבודה.</p>
    <div class="notice" role="status">המערכת נמצאת בפיתוח. חיבור מאגר והתחברות ארגונית עדיין אינם זמינים.</div>
    <section class="empty-state">
      <h2>חיבור מאגר האפיון</h2>
      <p>מנהל סביבת העבודה יוכל לחבר מאגר פרטי, להקצות צוותים וסוקרים ולהגדיר מדיניות ייצוא.</p>
      <a class="button" routerLink="/settings">הגדרת סביבת העבודה</a>
    </section>
  `,
})
export class Settings {}
