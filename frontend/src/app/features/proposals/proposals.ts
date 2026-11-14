import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterLink } from '@angular/router';

@Component({
  selector: 'app-proposals',
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <h1>הצעות שינוי</h1>
    <p class="page-description">מגבשים שינוי בענף משלו, סוקרים יחד וממזגים לאחר אישור.</p>
    
    <section class="empty-state">
      <h2>ההצעות שלכם יופיעו כאן</h2>
      <p>לפני פתיחת הצעה ראשונה יש לחבר מאגר ולהגדיר הרשאות. כל הצעה תכלול ענף נפרד, בעלים וסבב סקירה.</p>
      <a class="button" routerLink="/settings">הגדרת סביבת העבודה</a>
    </section>
  `,
})
export class Proposals {}
