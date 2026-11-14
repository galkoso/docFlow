import { ChangeDetectionStrategy, Component } from '@angular/core';
import { RouterLink } from '@angular/router';

@Component({
  selector: 'app-specification',
  imports: [RouterLink],
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <h1>האפיון המאושר</h1>
    <p class="page-description">מקור הידע המשותף של המערכת, בגרסה המאושרת האחרונה.</p>
    
    <section class="empty-state">
      <h2>מתחילים ממקור ידע אחד</h2>
      <p>חיבור מאגר האפיון עדיין לא הוגדר. לאחר החיבור, כל הפרקים יוצגו כאן כספר רציף עם חיפוש בכל התוכן.</p>
      <a class="button" routerLink="/settings">הגדרת סביבת העבודה</a>
    </section>
  `,
})
export class Specification {}
