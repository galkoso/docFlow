import { TestBed } from '@angular/core/testing';
import { signal } from '@angular/core';
import { provideHttpClient } from '@angular/common/http';
import { HttpTestingController, provideHttpClientTesting } from '@angular/common/http/testing';
import { ActivatedRoute, convertToParamMap, provideRouter, Router } from '@angular/router';
import { RouterTestingHarness } from '@angular/router/testing';
import { BehaviorSubject } from 'rxjs';
import type { ReviewConfiguration, ReviewArea } from '../../core/api/review-configuration-api';
import { SessionState } from '../../core/auth/session-state';
import { ReviewConfigurationPage } from './review-configuration-page';
import { reviewExit } from './review-exit';

const proposal = '00000000-0000-4000-8000-000000000001', product = '00000000-0000-4000-8000-000000000010';
const url = `/api/v1/proposals/${proposal}/review/configuration`;
const config: ReviewConfiguration = { proposal_id: proposal, number: 25, title: 'שינוי הרשאות', branch: 'change/25-review', target_branch: 'main',
  head_sha: 'a'.repeat(40), proposal_version: 1, configuration_version: 1, policy_version: 1, state: 'DRAFT', can_edit: true,
  settings: { ui_impact: 'undecided', ui_impact_reason: null, assignments: [] }, reviewers: [] };
function candidates(http: HttpTestingController, version = 1): void {
  for (const request of http.match(req => req.url.endsWith('/review/reviewers'))) {
    const domain = request.request.params.get('domain') as ReviewArea;
    request.flush({ proposal_id: proposal, configuration_version: version, domain, next_after: null,
      reviewers: domain === 'PRODUCT' ? [{ reviewer_id: product, display_name: 'סוקרת מוצר' }] : [] });
  }
}

describe('Review configuration page', () => {
  afterEach(() => vi.restoreAllMocks());
  async function create() {
    const profile = signal<{ user_id: string } | null>({ user_id: 'actor-a' }), logoutEpoch = signal(0);
    const params = new BehaviorSubject(convertToParamMap({ id: proposal }));
    let logoutGuard = () => true;
    await TestBed.configureTestingModule({ imports: [ReviewConfigurationPage], providers: [provideHttpClient(), provideHttpClientTesting(), provideRouter([]),
      { provide: SessionState, useValue: { profile, logoutEpoch, status: () => profile() ? 'authenticated' : 'anonymous', refresh: vi.fn(),
        guardLogout: (guard: () => boolean) => { logoutGuard = guard; return vi.fn(); } } },
      { provide: ActivatedRoute, useValue: { paramMap: params, snapshot: { paramMap: params.value } } }],
    }).compileComponents();
    const fixture = TestBed.createComponent(ReviewConfigurationPage), page = fixture.componentInstance;
    const http = TestBed.inject(HttpTestingController); fixture.detectChanges();
    const opened = async (value: ReviewConfiguration = config) => {
      http.expectOne(url).flush(value); if (value.can_edit) candidates(http);
      fixture.detectChanges(); await fixture.whenStable(); fixture.detectChanges();
    };
    return { fixture, page, http, opened, profile, logoutEpoch, guard: () => logoutGuard() };
  }

  it('renders named picker controls and saves the UI-impact form through its submit button', async () => {
    const { fixture, page, http, opened } = await create(); await opened();
    const element = fixture.nativeElement as HTMLElement;
    expect(element.textContent).toContain('change/25-review'); expect(element.textContent).toContain('main');
    expect(element.querySelector('label[for="candidate-PRODUCT"]')?.textContent).toContain('מוצר');
    const select = element.querySelector<HTMLSelectElement>('#candidate-PRODUCT')!;
    select.value = product; select.dispatchEvent(new Event('change')); fixture.detectChanges(); await fixture.whenStable();
    const add = [...element.querySelectorAll('button')].find(button => button.textContent?.trim() === 'הוספת סוקר מוצר')!;
    add.click(); fixture.detectChanges(); await fixture.whenStable();
    expect(page.state.settings().assignments).toEqual([{ reviewer_id: product, domain: 'PRODUCT', required: true }]);
    expect(element.querySelector('.assignments')?.textContent).toContain('סוקרת מוצר');
    const impact = element.querySelector<HTMLSelectElement>('#ui-impact')!;
    impact.value = 'no'; impact.dispatchEvent(new Event('change')); fixture.detectChanges(); await fixture.whenStable();
    const submit = element.querySelector<HTMLButtonElement>('button[type="submit"]')!;
    expect(submit.disabled).toBe(true);
    const reason = element.querySelector<HTMLTextAreaElement>('#ui-reason')!;
    reason.value = 'הבהרת כללי הרשאות בלבד'; reason.dispatchEvent(new Event('input')); fixture.detectChanges(); await fixture.whenStable();
    expect(submit.disabled).toBe(false); submit.click(); fixture.detectChanges();
    const save = http.expectOne(req => req.method === 'PUT');
    expect(save.request.body.settings.ui_impact_reason).toBe(reason.value);
    expect(element.querySelector<HTMLFieldSetElement>('fieldset')!.disabled).toBe(true);
    save.flush({ proposal_id: proposal, request_id: save.request.body.request_id, applied_proposal_version: 2, applied_configuration_version: 2,
      changed: true, replayed: false, current: { ...config, state: 'IN_REVIEW', proposal_version: 2, configuration_version: 2,
        settings: save.request.body.settings, reviewers: [{ ...save.request.body.settings.assignments[0], display_name: 'סוקרת מוצר', active: true, qualified: true }] } });
    candidates(http, 2); fixture.detectChanges(); await fixture.whenStable();
    expect(element.textContent).toContain('הגדרת הסקירה נשמרה'); expect(page.state.dirty()).toBe(false);
    expect(element.querySelector('button[type="submit"]')?.textContent).not.toContain('אישור'); fixture.destroy(); http.verify();
  });

  it('hides private edits during refresh and purges them on account change without an automatic new intent', async () => {
    const { fixture, page, http, opened, profile, logoutEpoch } = await create(); await opened();
    page.state.setReason('private rationale'); profile.set(null); fixture.detectChanges(); await fixture.whenStable();
    expect(fixture.nativeElement.textContent).not.toContain('שינוי הרשאות'); expect(page.state.settings().ui_impact_reason).toBe('private rationale');
    profile.set({ user_id: 'actor-a' }); fixture.detectChanges(); await fixture.whenStable();
    expect(fixture.nativeElement.textContent).toContain('שינוי הרשאות'); http.expectNone(url);
    profile.set({ user_id: 'actor-b' }); fixture.detectChanges();
    expect(page.state.settings().ui_impact_reason).toBeNull(); expect(page.picked().PRODUCT).toBe('');
    expect(page.blocked()).toBe(true); http.expectNone(url);
    page.reload(); const reload = http.expectOne(url); expect(reload.request.headers.get('X-Expected-Actor')).toBe('actor-b');
    reload.flush({ ...config, can_edit: false }); fixture.detectChanges();
    logoutEpoch.update(value => value + 1); fixture.detectChanges(); expect(page.state.data()).toBeNull();
    fixture.destroy(); http.verify();
  });

  it('guards logout, reload, unload and route exit while edits or uncertain writes remain', async () => {
    const { fixture, page, http, opened, guard } = await create(); await opened(); page.state.setImpact('yes');
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    expect(page.canLeave()).toBe(false); expect(guard()).toBe(false); page.reload(); http.expectNone(url);
    const unload = new Event('beforeunload', { cancelable: true }) as BeforeUnloadEvent;
    page.beforeUnload(unload); expect(unload.defaultPrevented).toBe(true);
    page.state.save(); http.expectOne(req => req.method === 'PUT').error(new ProgressEvent('error'));
    fixture.detectChanges(); await fixture.whenStable();
    expect(page.canLeave()).toBe(false); expect(confirm).toHaveBeenLastCalledWith(expect.stringContaining('ודאית'));
    expect(fixture.nativeElement.textContent).toContain('בדיקת תוצאת בקשת השמירה');
    confirm.mockReturnValue(true); expect(guard()).toBe(true); fixture.destroy(); http.verify();
  });

  it('shows a removed reviewer read-only and keeps server strings inert', async () => {
    const { fixture, page, http, opened } = await create();
    const selected = { reviewer_id: product, domain: 'PRODUCT' as const, required: true };
    await opened({ ...config, can_edit: false, state: 'CLOSED', title: '<script>bad()</script>',
      settings: { ui_impact: 'yes', assignments: [selected] }, reviewers: [{ ...selected, display_name: '<img src=x onerror=bad()>', active: false, qualified: false }] });
    const element = fixture.nativeElement as HTMLElement;
    expect(element.querySelector('script, img')).toBeNull(); expect(element.textContent).toContain('<script>bad()</script>');
    expect(element.textContent).toContain('אינו כשיר כעת');
    expect([...element.querySelectorAll<HTMLButtonElement>('.assignments button')].every(button => button.disabled)).toBe(true);
    expect(page.state.editable()).toBe(false); fixture.destroy(); http.verify();
  });

  it('runs the loss guard on proposal changes through the actual router', async () => {
    const profile = signal<{ user_id: string } | null>(null);
    await TestBed.configureTestingModule({ imports: [ReviewConfigurationPage], providers: [provideHttpClient(), provideHttpClientTesting(),
      provideRouter([{ path: 'changes/:id/review', component: ReviewConfigurationPage, canDeactivate: [reviewExit] }]),
      { provide: SessionState, useValue: { profile, logoutEpoch: signal(0), status: () => 'authenticated', refresh: vi.fn(), guardLogout: () => () => {} } }],
    }).compileComponents();
    const harness = await RouterTestingHarness.create();
    const page = await harness.navigateByUrl(`/changes/${proposal}/review`, ReviewConfigurationPage);
    const http = TestBed.inject(HttpTestingController), router = TestBed.inject(Router);
    profile.set({ user_id: 'actor-a' }); harness.detectChanges(); http.expectOne(url).flush(config); candidates(http);
    harness.detectChanges(); await harness.fixture.whenStable(); page.state.setReason('private rationale');
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false), another = '00000000-0000-4000-8000-000000000002';
    await harness.navigateByUrl(`/changes/${another}/review`);
    expect(router.url).toBe(`/changes/${proposal}/review`); expect(confirm).toHaveBeenCalled(); http.expectNone(req => req.url.includes(another));
    confirm.mockReturnValue(true); await harness.navigateByUrl(`/changes/${another}/review`);
    expect(router.url).toContain(another); http.expectOne(`/api/v1/proposals/${another}/review/configuration`).flush({ ...config, proposal_id: another, can_edit: false });
    harness.detectChanges(); expect(page.state.settings().ui_impact_reason).toBeNull(); harness.fixture.destroy(); http.verify();
  });
});
