import { CanDeactivateFn } from '@angular/router';
export interface CreationExit { canLeave(): boolean; }
export const creationExit: CanDeactivateFn<CreationExit> = component => component.canLeave();
