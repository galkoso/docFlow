import { TestBed } from '@angular/core/testing';
import { RichEditor, RichChange } from './rich-editor';
import { RichDocument } from './rich-document';
import type { EditorModel } from '../../core/api/editor-api';
import contract from '../../../../../fixtures/editor-contract.json';

const context = { repository: '00000000-0000-0000-0000-000000000001', proposal: '00000000-0000-0000-0000-000000000002', commit: 'a'.repeat(40) };
const model = contract[0].model as unknown as EditorModel;

describe('Rich editor controls', () => {
  const geometry = new Map<string, PropertyDescriptor | undefined>();
  beforeEach(() => {
    for (const name of ['getClientRects', 'getBoundingClientRect']) {
      geometry.set(name, Object.getOwnPropertyDescriptor(Range.prototype, name));
      Object.defineProperty(Range.prototype, name, { configurable: true, value: () => name === 'getClientRects' ? [] : new DOMRect() });
    }
    vi.spyOn(window, 'scrollBy').mockImplementation(() => {});
  });
  afterEach(() => { vi.restoreAllMocks(); for (const [name, descriptor] of geometry) {
    if (descriptor) Object.defineProperty(Range.prototype, name, descriptor); else Reflect.deleteProperty(Range.prototype, name);
  } });
  async function create() {
    await TestBed.configureTestingModule({ imports: [RichEditor] }).compileComponents();
    const fixture = TestBed.createComponent(RichEditor);
    const rich = new RichDocument(model, context);
    fixture.componentRef.setInput('document', rich);
    fixture.detectChanges(); await fixture.whenStable();
    return { fixture, component: fixture.componentInstance, rich };
  }

  it('changes a heading level with its stable ID, emits source, and supports undo/redo', async () => {
    const { fixture, component, rich } = await create();
    const changes: RichChange[] = [];
    component.changed.subscribe(change => changes.push(change));
    const id = model.blocks.find(block => block.kind === 'heading')!.section_id!;
    component.focusSection(id); component.block('h3');
    expect(component.state!.doc.firstChild!.attrs['sectionId']).toBe(id);
    expect(component.state!.doc.firstChild!.attrs['level']).toBe(3);
    expect(changes.at(-1)?.markdown).toContain(`<!-- ss:section:${id} -->`);
    expect(changes.at(-1)?.changed).toBe(true);
    component.undo(); expect(changes.at(-1)?.markdown).toBe(rich.originalSource);
    component.redo(); expect(component.state!.doc.firstChild!.attrs['level']).toBe(3);
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('[role="textbox"]')?.getAttribute('aria-multiline')).toBe('true');
  });

  it('keeps local content when permission changes and rejects controls in readonly mode', async () => {
    const { fixture, component } = await create();
    const id = model.blocks.find(block => block.kind === 'heading')!.section_id!;
    component.focusSection(id); component.block('h4');
    const doc = component.state!.doc;
    fixture.componentRef.setInput('editable', false); fixture.detectChanges(); await fixture.whenStable();
    expect(component.state!.doc).toBe(doc);
    expect(fixture.nativeElement.querySelector('.ProseMirror')?.getAttribute('contenteditable')).toBe('false');
    component.undo(); component.block('paragraph'); component.mark('strong');
    fixture.nativeElement.querySelector('.ProseMirror').dispatchEvent(new KeyboardEvent('keydown', { key: 'z', code: 'KeyZ', keyCode: 90, ctrlKey: true, bubbles: true, cancelable: true }));
    expect(component.state!.doc).toBe(doc);
    expect([...fixture.nativeElement.querySelectorAll('.rich-toolbar button')].every(button => (button as HTMLButtonElement).disabled)).toBe(true);
  });

  it('validates link input and renders protected source as inert text', async () => {
    await TestBed.configureTestingModule({ imports: [RichEditor] }).compileComponents();
    const fixture = TestBed.createComponent(RichEditor);
    const protectedModel = contract.find(entry => entry.name === 'gfm-protected')!.model as unknown as EditorModel;
    fixture.componentRef.setInput('document', new RichDocument(protectedModel, context));
    fixture.detectChanges(); await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('script')).toBeNull();
    expect(fixture.nativeElement.querySelector('.protected-source pre')?.textContent).toContain('<script>');
    fixture.componentInstance.openLink(); fixture.componentInstance.linkUrl.set('javascript:alert(1)'); fixture.componentInstance.applyLink();
    expect(fixture.componentInstance.linkError()).toContain('HTTPS');
    expect(fixture.componentInstance.linkOpen()).toBe(true);
    fixture.componentInstance.closeLink();
    const changes: RichChange[] = []; fixture.componentInstance.changed.subscribe(change => changes.push(change));
    fixture.componentInstance.focusSection('SEC-NESTED'); fixture.componentInstance.block('h4');
    expect(changes.at(-1)?.markdown).toContain('SEC-NESTED');
    expect(changes.at(-1)?.edit.deleted_section_ids).toEqual([]);
  });

  it('inserts a simple table and edits its shape without producing unsupported Markdown', async () => {
    const { fixture, component } = await create();
    const changes: RichChange[] = [];
    component.changed.subscribe(change => changes.push(change));
    component.table();
    expect(component.selection().table).toBe(true);
    expect(component.error()).toBe('');
    component.tableAction('row'); component.tableAction('column');
    expect(component.error()).toBe('');
    expect(changes.at(-1)?.edit.blocks.some(block => block.action === 'insert' && block.markdown.includes('|'))).toBe(true);
    const doc = component.state!.doc;
    component.tableAction('deleteRow');
    expect(component.state!.doc).toBe(doc); // A simple GFM table retains its header.
    await fixture.whenStable();
    expect(fixture.nativeElement.querySelector('table')).not.toBeNull();
  });
});
