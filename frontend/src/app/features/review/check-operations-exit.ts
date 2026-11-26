import type { CanDeactivateFn } from '@angular/router';
import type { CheckOperationsPage } from './check-operations-page';
export const checkOperationsExit: CanDeactivateFn<CheckOperationsPage> = component => component.canLeave();
