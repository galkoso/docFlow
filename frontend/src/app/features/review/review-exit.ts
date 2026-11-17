import type { CanDeactivateFn } from '@angular/router';
import type { ReviewConfigurationPage } from './review-configuration-page';
export const reviewExit: CanDeactivateFn<ReviewConfigurationPage> = component => component.canLeave();
