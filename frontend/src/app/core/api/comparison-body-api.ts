import { inject, InjectionToken } from '@angular/core';
import { ComparisonApi } from './comparison-api';

/** Exact transport for already selected immutable files. Page-local providers
 * choose shared review authority; private pages retain their original API. */
export type ComparisonBodyApi = Pick<ComparisonApi, 'raw' | 'readable' | 'readableRow' | 'readableUnit' | 'image'>;
export const COMPARISON_BODY_API = new InjectionToken<ComparisonBodyApi>('immutable comparison file transport', {
  providedIn: 'root', factory: () => inject(ComparisonApi),
});
