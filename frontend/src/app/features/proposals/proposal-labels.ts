export const proposalStates = [
  ['PROVISIONING', 'בהכנה'], ['DRAFT', 'טיוטה'], ['IN_REVIEW', 'בסקירה'], ['CHANGES_REQUESTED', 'נדרשים תיקונים'],
  ['APPROVED', 'מאושרת'], ['MERGING', 'במיזוג'], ['MERGED', 'מוזגה'], ['CLOSED', 'סגורה'],
] as const;
export function stateLabel(state: string): string { return proposalStates.find(([value]) => value === state)?.[1] ?? 'מצב לא מוכר'; }
export function preparationError(code?: string | null): string {
  switch (code) {
    case 'SOURCE_MOVED': return 'המקור השתנה מאז פתיחת ההצעה. נדרשת בדיקה של גרסת המקור.';
    case 'BRANCH_EXISTS': return 'שם הענף כבר קיים. יש לבחור שם אחר בהצעה חדשה.';
    case 'PERMISSION_REVOKED': return 'ההרשאות או הגדרות המאגר השתנו. יש לפנות למנהל המאגר.';
    case 'CREATE_RESULT_UNKNOWN': return 'תוצאת יצירת הענף דורשת בדיקה לפני המשך העבודה.';
    case 'PROVIDER_RATE_LIMIT': return 'ספק המאגר מגביל כרגע את הבקשות. ההכנה ממתינה להמשך בדיקה.';
    default: return 'הכנת הענף לא הושלמה. אפשר לבדוק שוב או לפנות למנהל המאגר.';
  }
}
