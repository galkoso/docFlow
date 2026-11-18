import type { CanDeactivateFn } from '@angular/router';
import type { ReviewSnapshotsPage } from './review-snapshots-page';
export const reviewSnapshotExit: CanDeactivateFn<ReviewSnapshotsPage> = component => component.canLeave();
