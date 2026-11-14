import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterLink } from '@angular/router';

@Component({
  selector: 'app-history',
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <h1>היסטוריית האפיון</h1>
    <p class="page-description">גרסאות שפורסמו וההחלטות שהובילו אליהן.</p>
    
    <section class="empty-state">
      <h2>עדיין אין גרסאות להצגה</h2>
      <p>לאחר חיבור המאגר יוצגו כאן גרסאות האפיון. שחזור יפתח הצעת שינוי חדשה לסקירה.</p>
      <a class="button" routerLink="/settings">הגדרת סביבת העבודה</a>
    </section>
  `,
})
export class History {}
