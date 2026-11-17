import { convertToParamMap } from '@angular/router';
import { comparisonLocation } from './comparison-location';
import { id, snapshot, head, file } from './comparison.testing';

describe('Comparison location grammar', () => {
  it('restores immutable file and unit identities with a view, without actor/source/line authority', () => {
    const value = comparisonLocation(convertToParamMap({ snapshot, file: file().id, unit: 'd'.repeat(64), view: 'readable' }));
    expect(value).toEqual({ snapshot, file: file().id, unit: 'd'.repeat(64), view: 'readable', offset: 0, request: null, head: null, target: null });
    expect(comparisonLocation(convertToParamMap({ request_id: id(6), head }))?.request).toBe(id(6));
    expect(comparisonLocation(convertToParamMap({ snapshot, offset: '100' }))?.offset).toBe(100);
  });
  it('rejects duplicate, mixed, malformed, missing-scope and unknown parameters', () => {
    const invalid: Array<Record<string, string | string[]>> = [
      { snapshot, file: [file().id, file(1).id] }, { snapshot, file: file().id, offset: '0' }, { snapshot, unit: 'd'.repeat(64) },
      { file: file().id }, { snapshot, file: file().id, unit: 'd'.repeat(64), view: 'raw' }, { snapshot, file: 'invalid' }, { snapshot, view: 'html' }, { snapshot, actor: id(1) },
      { snapshot, request_id: id(6) }, { snapshot, head }, { snapshot, offset: '-1' }, { snapshot, offset: '40001' },
      { snapshot, offset: '1e2' }, { request_id: id(6), offset: '0' }, { snapshot: '' },
    ];
    for (const value of invalid) expect(comparisonLocation(convertToParamMap(value)), JSON.stringify(value)).toBeNull();
  });
});
