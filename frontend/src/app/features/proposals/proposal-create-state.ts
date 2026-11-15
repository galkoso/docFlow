import { computed, inject, Injectable, signal } from '@angular/core';
import { HttpErrorResponse } from '@angular/common/http';
import { Subscription } from 'rxjs';
import { CreateProposal, CreationAccepted, PreparedCreation, ProposalApi, ProposalChoices } from '../../core/api/proposal-api';

export interface CreationDraft {
  repository_id: string; title: string; branch: string; description: string; team_id: string; owner_id: string;
  initial_mode: string; ui_impact: string; reason: string; figma: string; ticket: string; development_pr: string;
}
const blank = (): CreationDraft => ({ repository_id: '', title: '', branch: '', description: '', team_id: '', owner_id: '',
  initial_mode: 'manual', ui_impact: 'undecided', reason: '', figma: '', ticket: '', development_pr: '' });
function errorMessage(error: unknown): string {
  const message: unknown = error instanceof HttpErrorResponse ? error.error?.error?.message_he : null;
  return typeof message === 'string' ? message : 'לא ניתן לאמת את תוצאת הפעולה. יש לנסות שוב עם אותה בקשה.';
}
export function defaultBranch(number: number, title: string): string {
  const slug = /^[\x00-\x7f]*$/.test(title) ? title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 50).replace(/-$/g, '') : '';
  return `change/${number}-${slug || 'new-proposal'}`;
}
function branchValid(branch: string): boolean {
  return /^change\/[\x21-\x7e]+$/.test(branch) && branch.length <= 100 && !/[~^:?*\[\\]/.test(branch)
    && !['..', '@{', '//'].some(part => branch.includes(part)) && !branch.endsWith('.')
    && branch.split('/').every(part => !!part && !part.startsWith('.') && !part.endsWith('.lock'));
}

@Injectable()
export class ProposalCreateState {
  private readonly api = inject(ProposalApi);
  private requests = new Subscription();
  private generation = 0;
  private owner?: string;
  private activeActor?: string;
  private previewKey?: { repository: string; key: string };
  private attempt?: { body: CreateProposal; key: string; context: PreparedCreation };
  private completed?: CreationAccepted;
  private customBranch = false;
  readonly form = signal<CreationDraft>(blank());
  readonly choices = signal<ProposalChoices | null>(null);
  readonly context = signal<PreparedCreation | null>(null);
  readonly ticket = signal<CreationAccepted | null>(null);
  readonly ticketOwner = signal<string | null>(null);
  readonly loading = signal(false);
  readonly preparing = signal(false);
  readonly submitting = signal(false);
  readonly uncertain = signal(false);
  readonly dirty = signal(false);
  readonly error = signal<string | null>(null);
  readonly fieldErrors = signal<Record<string, string>>({});
  readonly movedSource = signal<string | null>(null);
  readonly authRequired = signal(false);
  readonly submittedLabels = signal<{ owner: string; team: string } | null>(null);
  readonly locked = computed(() => this.submitting() || this.uncertain() || !!this.ticket());
  readonly teams = computed(() => this.choices()?.teams.filter(item => item.repository_ids.includes(this.form().repository_id)) ?? []);
  readonly owners = computed(() => this.choices()?.owners.filter(item => item.repository_ids.includes(this.form().repository_id)) ?? []);
  readonly canCreate = computed(() => !!this.choices()?.repositories.some(item => item.repository_id === this.form().repository_id && item.can_create));

  private cancel(): void { ++this.generation; this.requests.unsubscribe(); this.requests = new Subscription(); this.loading.set(false); this.preparing.set(false); this.submitting.set(false); }
  dispose(): void {
    this.cancel(); this.form.set(blank()); this.choices.set(null); this.context.set(null); this.ticket.set(null); this.ticketOwner.set(null);
    this.owner = undefined; this.activeActor = undefined; this.previewKey = undefined; this.attempt = undefined; this.completed = undefined;
    this.customBranch = false; this.uncertain.set(false); this.dirty.set(false); this.error.set(null); this.fieldErrors.set({}); this.movedSource.set(null);
    this.authRequired.set(false); this.submittedLabels.set(null);
  }
  scope(actor?: string, requestedRepository?: string): void {
    if (this.submitting()) this.uncertain.set(true);
    this.cancel(); this.choices.set(null); this.context.set(null); this.ticket.set(null); this.ticketOwner.set(null); this.activeActor = actor;
    // Keep only human-entered, in-memory recovery data while signed out. A
    // different identity purges it; no draft is ever submitted automatically.
    if (!actor) return;
    if (actor !== this.owner) { this.dispose(); this.owner = actor; this.activeActor = actor; }
    if (this.completed) { this.ticketOwner.set(actor); this.ticket.set(this.completed); return; }
    if (this.uncertain() && this.attempt) this.context.set(this.attempt.context);
    const generation = this.generation;
    this.loading.set(true);
    this.requests.add(this.api.choices().subscribe({
      next: choices => {
        if (generation !== this.generation) return;
        this.choices.set(choices); this.loading.set(false);
        if (!choices.repositories.length) { this.error.set('אין עדיין מאגר מורשה לפתיחת הצעות. נדרשת הגדרת מאגר בסביבת העבודה.'); return; }
        const repository = this.uncertain() && this.attempt ? this.attempt.body.repository_id
          : requestedRepository ?? (this.form().repository_id || choices.repositories.find(item => item.can_create)?.repository_id || '');
        this.form.update(form => ({ ...form, repository_id: repository }));
        if (!choices.repositories.some(item => item.repository_id === repository && item.can_create)) {
          this.error.set('אין הרשאה לפתוח הצעה במאגר שנבחר. יש לבחור מאגר מורשה.'); return;
        }
        if (!this.uncertain()) this.form.update(form => ({ ...form,
          owner_id: form.owner_id || (this.owners().some(item => item.owner_id === actor) ? actor : ''),
          team_id: form.team_id || (this.teams().length === 1 ? this.teams()[0].team_id : ''),
        }));
        if (!this.uncertain()) this.fieldErrors.update(errors => ({ ...errors,
          owner_id: this.form().owner_id && !this.owners().some(item => item.owner_id === this.form().owner_id) ? 'הבעלים שנבחרו אינם זמינים כעת. יש לבחור בעלים מורשים.' : '',
          team_id: this.form().team_id && !this.teams().some(item => item.team_id === this.form().team_id) ? 'הצוות שנבחר אינו זמין במאגר זה. יש לבחור צוות מורשה.' : '',
        }));
        if (!this.uncertain()) this.prepare(false);
      },
      error: error => { if (generation === this.generation) { this.loading.set(false); this.error.set(errorMessage(error)); if (error instanceof HttpErrorResponse && error.status === 401) this.authRequired.set(true); } },
    }));
  }
  update(field: keyof CreationDraft, value: string): void {
    if (this.locked()) return;
    if (field === 'branch') this.customBranch = true;
    this.form.update(form => ({ ...form, [field]: value })); this.dirty.set(true); this.fieldErrors.update(errors => ({ ...errors, [field]: '' }));
    if (field === 'repository_id') this.context.set(null);
    if (field === 'title' && !this.customBranch && this.context()) this.form.update(form => ({ ...form, branch: defaultBranch(this.context()!.number, value) }));
  }
  suggestBranch(): void {
    if (this.locked() || !this.context()) return;
    this.customBranch = false; this.form.update(form => ({ ...form, branch: defaultBranch(this.context()!.number, form.title) })); this.dirty.set(true);
  }
  prepare(fresh = true): void {
    const repository = this.form().repository_id;
    if (!this.activeActor || this.locked() || !this.canCreate() || !repository) return;
    if (fresh || this.previewKey?.repository !== repository) this.previewKey = { repository, key: crypto.randomUUID() };
    const key = this.previewKey!.key, generation = this.generation;
    this.preparing.set(true); this.context.set(null); this.error.set(null);
    this.requests.add(this.api.prepare(repository, key).subscribe({
      next: context => {
        if (generation !== this.generation || this.previewKey?.key !== key) return;
        if (context.repository_id !== repository) { this.preparing.set(false); this.error.set('פרטי המקור אינם תואמים למאגר.'); return; }
        this.context.set(context); this.preparing.set(false); if (fresh) this.movedSource.set(null);
        if (!this.customBranch) this.form.update(form => ({ ...form, branch: defaultBranch(context.number, form.title) }));
      },
      error: error => { if (generation === this.generation && this.previewKey?.key === key) { this.preparing.set(false); this.error.set(errorMessage(error)); if (error instanceof HttpErrorResponse && error.status === 401) this.authRequired.set(true); } },
    }));
  }
  submit(): void {
    if (!this.activeActor || this.submitting() || this.ticket()) return;
    const context = this.context();
    if (!context) { this.error.set('יש להכין את גרסת המקור לפני יצירה.'); return; }
    if (context.repository_id !== this.form().repository_id) { this.error.set('גרסת המקור אינה תואמת למאגר. יש להכין שוב את הטופס.'); return; }
    if (!this.uncertain()) {
      const fields = this.validate(); this.fieldErrors.set(fields);
      if (Object.keys(fields).length || !this.canCreate()) { this.error.set('יש לתקן את השדות המסומנים לפני יצירה.'); return; }
      const form = this.form();
      const body: CreateProposal = { repository_id: form.repository_id, title: form.title, branch: form.branch, description: form.description,
        source_branch: 'main', expected_source_sha: context.source_sha, creation_context_id: context.context_id, team_id: form.team_id,
        owner_id: form.owner_id, initial_mode: form.initial_mode as 'manual' | 'ai', ui_impact: form.ui_impact as 'yes' | 'no' | 'undecided',
        ui_impact_reason: form.reason || null, links: (['figma', 'ticket', 'development_pr'] as const).filter(kind => !!form[kind]).map(kind => ({ kind, url: form[kind] })),
      };
      if (!this.attempt || JSON.stringify(this.attempt.body) !== JSON.stringify(body)) {
        this.attempt = { body, key: crypto.randomUUID(), context };
        this.submittedLabels.set({ owner: this.owners().find(item => item.owner_id === form.owner_id)?.display_name ?? '', team: this.teams().find(item => item.team_id === form.team_id)?.name ?? '' });
      }
    }
    const attempt = this.attempt;
    if (!attempt) return;
    const generation = this.generation, previouslyUncertain = this.uncertain();
    this.submitting.set(true); this.error.set(null);
    this.requests.add(this.api.create(attempt.body, attempt.key).subscribe({
      next: ticket => {
        if (generation !== this.generation) return;
        if (ticket.number !== attempt.context.number || !ticket.proposal_id || !ticket.job_id) {
          this.submitting.set(false); this.uncertain.set(true); this.error.set('תוצאת היצירה אינה תואמת לבקשה. יש לבדוק שוב את אותה פעולה.'); return;
        }
        this.submitting.set(false); this.uncertain.set(false); this.dirty.set(false); this.completed = ticket; this.ticketOwner.set(this.activeActor ?? null); this.ticket.set(ticket);
      },
      error: error => {
        if (generation !== this.generation) return;
        this.submitting.set(false); this.error.set(errorMessage(error));
        const definite = error instanceof HttpErrorResponse && error.status >= 400 && error.status < 500;
        this.uncertain.set(previouslyUncertain || !definite);
        if (error instanceof HttpErrorResponse) {
          if (error.status === 401) this.authRequired.set(true);
          const report = error.error?.error;
          if (report?.code === 'SOURCE_MOVED' && typeof report.details?.actual_source_sha === 'string') this.movedSource.set(report.details.actual_source_sha);
          if (report?.field_errors) {
            const fields: Record<string, string> = {};
            for (const name of Object.keys(report.field_errors)) fields[name === 'ui_impact_reason' ? 'reason' : name] = 'יש לבדוק ולתקן את השדה.';
            this.fieldErrors.set(fields);
          }
        }
      },
    }));
  }
  abandonUncertain(): void { this.attempt = undefined; this.uncertain.set(false); this.prepare(true); }
  private validate(): Record<string, string> {
    const form = this.form(), errors: Record<string, string> = {};
    if (Array.from(form.title).length < 3 || Array.from(form.title).length > 120 || !form.title.trim() || /[\x00-\x1f\x7f-\x9f]/.test(form.title)) errors['title'] = 'נדרש שם בן 3–120 תווים, בשורה אחת.';
    if (Array.from(form.description).length < 10 || Array.from(form.description).length > 10000 || !form.description.trim()) errors['description'] = 'נדרש תיאור בן 10–10,000 תווים.';
    if (!branchValid(form.branch)) errors['branch'] = 'נדרש שם ענף ASCII תקין שמתחיל ב־change/ ועד 100 תווים.';
    if (!this.teams().some(item => item.team_id === form.team_id)) errors['team_id'] = 'יש לבחור צוות מורשה.';
    if (!this.owners().some(item => item.owner_id === form.owner_id)) errors['owner_id'] = 'יש לבחור בעלים מורשים.';
    if (!['yes', 'no', 'undecided'].includes(form.ui_impact)) errors['ui_impact'] = 'יש לבחור השפעת UI.';
    if (!['manual', 'ai'].includes(form.initial_mode)) errors['initial_mode'] = 'יש לבחור מסלול התחלה.';
    if ((form.ui_impact === 'no' && !form.reason.trim()) || Array.from(form.reason).length > 10000) errors['reason'] = 'סימון ללא השפעת UI מחייב נימוק, עד 10,000 תווים.';
    for (const kind of ['figma', 'ticket', 'development_pr'] as const) if (form[kind]) {
      try { const url = new URL(form[kind]); if (url.protocol !== 'https:' || !url.hostname || url.username || url.password || /\s/.test(form[kind]) || new TextEncoder().encode(form[kind]).length > 2000) throw new Error(); }
      catch { errors[kind] = 'נדרש קישור HTTPS תקין, ללא פרטי התחברות.'; }
    }
    return errors;
  }
}
