import type { CanDeactivateFn } from '@angular/router';
import type { ReviewSnapshotsPage } from './review-snapshots-page';
export const reviewSnapshotExit: CanDeactivateFn<ReviewSnapshotsPage> = (component, _route, _state, next) => component.canNavigate(next?.url);
