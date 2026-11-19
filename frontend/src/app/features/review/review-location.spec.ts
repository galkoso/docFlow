import { convertToParamMap } from '@angular/router';
import { reviewLocation } from './review-location';
import { snapshot } from '../../../testing/review-fixtures';

describe('Shared review content URL', () => {
  const file = 'a'.repeat(64), unit = 'b'.repeat(64);
  it('restores selected snapshot, file page, mode and paired-unit identity', () => {
    expect(reviewLocation(convertToParamMap({ snapshot, file, files: '50', mode: 'readable', unit })))
      .toEqual({ snapshot, file, files: 50, media: 0, mode: 'readable', unit });
  });
  it('restores an independently paged mandatory requirement list', () => {
    expect(reviewLocation(convertToParamMap({ snapshot, requirements: '40000' })))
      .toEqual({ snapshot, files: 0, media: 0, requirements: 40000 });
  });
  it.each([
    { requirements: '20' }, { snapshot, requirements: '-1' }, { snapshot, requirements: '40001' },
    { snapshot, requirements: ['20', '40'] }, { snapshot, requirements: '01' }, { snapshot, requirements: '1.5' },
    { file }, { snapshot, mode: 'raw' }, { snapshot, file, mode: 'image' }, { snapshot, file: '../docs/a.md' },
    { snapshot, file, unit: 'bad' }, { snapshot, file, mode: 'raw', unit }, { snapshot, unit },
    { snapshot, file: [file, file] }, { snapshot, file, unit: [unit, unit] }, { snapshot, file, head: 'main' },
  ])('rejects ambiguous or unbound file/unit coordinates: %j', query => {
    expect(reviewLocation(convertToParamMap(query))).toBeNull();
  });
});
