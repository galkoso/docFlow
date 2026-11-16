import {
  afterRenderEffect, ChangeDetectionStrategy, Component, effect, ElementRef, input, output, signal, untracked, viewChild, ViewEncapsulation,
} from '@angular/core';
import { EditorView } from 'prosemirror-view';
import { Command, EditorState, NodeSelection, Plugin, TextSelection } from 'prosemirror-state';
import { baseKeymap, chainCommands, lift, toggleMark, wrapIn } from 'prosemirror-commands';
import { history, redo, undo } from 'prosemirror-history';
import { keymap } from 'prosemirror-keymap';
import { liftListItem, sinkListItem, splitListItem, wrapInList } from 'prosemirror-schema-list';
import { addColumnAfter, addRowAfter, deleteColumn, deleteRow, deleteTable, goToNextCell, isInTable, tableEditing } from 'prosemirror-tables';
import { gapCursor } from 'prosemirror-gapcursor';
import { dropCursor } from 'prosemirror-dropcursor';
import type { EditorEdit } from '../../core/api/editor-api';
import { RichDocument } from './rich-document';
import { safeLink } from './markdown-schema';

export interface RichChange { edit: EditorEdit; markdown: string; changed: boolean; }
export interface RichEditorSnapshot {
  state: EditorState; linkOpen: boolean; linkUrl: string; linkError: string; imageOpen: boolean; imageAlt: string; error: string;
}
interface SelectionInfo { bold: boolean; italic: boolean; strike: boolean; inlineCode: boolean; quote: boolean; list: string; block: string; table: boolean; header: boolean; image: boolean; }
const emptySelection: SelectionInfo = { bold: false, italic: false, strike: false, inlineCode: false, quote: false, list: '', block: 'paragraph', table: false, header: false, image: false };

@Component({
  selector: 'app-rich-editor', changeDetection: ChangeDetectionStrategy.OnPush, encapsulation: ViewEncapsulation.None,
  host: { class: 'rich-editor' }, templateUrl: './rich-editor.html', styleUrl: './rich-editor.css',
})
export class RichEditor {
  readonly document = input.required<RichDocument>();
  readonly restored = input<RichEditorSnapshot | null>(null);
  readonly editable = input(true);
  readonly changed = output<RichChange>();
  readonly serializationFailed = output<void>();
  readonly selection = signal<SelectionInfo>(emptySelection);
  readonly notice = signal('');
  readonly error = signal('');
  readonly linkOpen = signal(false);
  readonly linkUrl = signal('');
  readonly linkError = signal('');
  readonly imageOpen = signal(false);
  readonly imageAlt = signal('');
  private readonly mount = viewChild.required<ElementRef<HTMLElement>>('mount');
  private readonly linkInput = viewChild<ElementRef<HTMLInputElement>>('linkInput');
  private instance?: EditorView;
  get state(): EditorState | undefined { return this.instance?.state; }

  constructor() {
    afterRenderEffect(onCleanup => {
      const rich = this.document();
      const host = this.mount().nativeElement;
      const schema = rich.format.schema;
      const restored = this.restored();
      this.notice.set(''); this.error.set(''); this.linkOpen.set(false); this.imageOpen.set(false);
      this.linkUrl.set(''); this.imageAlt.set(''); this.linkError.set('');
      const mark = (name: string) => toggleMark(schema.marks[name]);
      const keys: Record<string, Command> = {
        'Mod-z': undo, 'Mod-y': redo, 'Mod-Shift-z': redo, 'Mod-b': mark('strong'), 'Mod-i': mark('em'), 'Mod-`': mark('code'),
        Enter: (state, dispatch) => {
          if (isInTable(state)) {
            if (!goToNextCell(1)(state, dispatch) && addRowAfter(state, dispatch) && this.instance) goToNextCell(1)(this.instance.state, dispatch);
            return true;
          }
          const item = this.ancestor(state, 'list_item');
          return splitListItem(schema.nodes['list_item'], { checked: item?.node.attrs['checked'] == null ? null : false })(state, dispatch);
        },
        Tab: chainCommands(goToNextCell(1), sinkListItem(schema.nodes['list_item'])),
        'Shift-Tab': chainCommands(goToNextCell(-1), liftListItem(schema.nodes['list_item'])),
        'Shift-Enter': (state, dispatch) => {
          if (isInTable(state)) { this.notice.set('בתא בטבלה, Enter עובר לתא הבא.'); return true; }
          if (dispatch) dispatch(state.tr.replaceSelectionWith(schema.nodes['hard_break'].create()).scrollIntoView());
          return true;
        },
      };
      const plugins = [new Plugin({ filterTransaction: transaction => !transaction.docChanged || untracked(() => this.editable()) }),
          history(), rich.identityPlugin(() => this.notice.set('רכיב מקור מוגן נשמר ללא שינוי. אפשר לערוך את הטקסט שלצדו.')),
          keymap(keys), keymap(baseKeymap), gapCursor(), dropCursor(), tableEditing()];
      // Reconfigure with fresh view/permission closures while preserving plugin
      // state (including history) for the same in-memory document/schema only.
      const state = restored?.state.schema === schema ? restored.state.reconfigure({ plugins })
        : EditorState.create({ doc: rich.doc, plugins });
      if (restored?.state.schema === schema) {
        this.linkOpen.set(restored.linkOpen); this.linkUrl.set(restored.linkUrl); this.linkError.set(restored.linkError);
        this.imageOpen.set(restored.imageOpen); this.imageAlt.set(restored.imageAlt); this.error.set(restored.error);
      }
      const view = new EditorView(host, {
        state,
        editable: () => untracked(() => this.editable()),
        attributes: () => ({ role: 'textbox', 'aria-label': 'עריכת תוכן המסמך', 'aria-multiline': 'true',
          'aria-readonly': String(!untracked(() => this.editable())), tabindex: '0', dir: 'auto', spellcheck: 'true' }),
        dispatchTransaction: transaction => {
          const next = view.state.applyTransaction(transaction);
          view.updateState(next.state);
          this.describe(next.state);
          if (next.transactions.some(transaction => transaction.docChanged)) {
            try { const change = rich.changes(next.state.doc); this.error.set(''); this.changed.emit(change); }
            catch { this.error.set('לא ניתן לייצג את השינוי במקור הנתמך. השינוי עדיין בעורך; אפשר לבטל אותו או להמשיך לתקנו.'); this.serializationFailed.emit(); }
          }
        },
        handleClick: (_view, _pos, event) => {
          if (event.target instanceof Element && event.target.closest('a')) { event.preventDefault(); return true; }
          return false;
        },
        handlePaste: (view, event, slice) => {
          let unsafe = false;
          let protectedSource = false;
          slice.content.descendants(node => {
            if (node.type.name === 'protected_source' || node.type.name === 'section_marker') protectedSource = true;
            if (node.attrs['colspan'] > 1 || node.attrs['rowspan'] > 1) unsafe = true;
          });
          if (protectedSource) { this.notice.set('לא הועתק רכיב מקור מוגן. המקור נשאר במקומו.'); return true; }
          if (unsafe) {
            view.dispatch(view.state.tr.insertText(event.clipboardData?.getData('text/plain') ?? ''));
            this.notice.set('הודבק טקסט בלבד. מבנה טבלה מורכב אינו נתמך בעריכה זו.'); return true;
          }
          if (event.clipboardData?.getData('text/html')) this.notice.set('עיצוב ההדבקה הומר לרכיבים הנתמכים. יש לבדוק תמונות ועיצוב לפני שמירה.');
          return false;
        },
        nodeViews: { protected_source: node => {
          const source = rich.protectedSource(node.attrs['sourceKey']);
          const dom = window.document.createElement('details'); dom.className = 'protected-source'; dom.contentEditable = 'false';
          const summary = window.document.createElement('summary'); summary.textContent = 'רכיב מקור מוגן — צפייה במקור';
          const pre = window.document.createElement('pre'); pre.dir = 'auto'; pre.textContent = source?.markdown ?? 'המקור אינו זמין.';
          dom.append(summary, pre);
          return { dom, ignoreMutation: () => true, stopEvent: event => event.target instanceof Element && event.target.closest('summary, pre') != null };
        } },
      });
      this.instance = view; this.describe(view.state);
      onCleanup(() => { if (this.instance === view) this.instance = undefined; view.destroy(); });
    });
    effect(() => { const editable = this.editable(); untracked(() => this.instance?.setProps({ editable: () => editable })); });
  }

  private ancestor(state: EditorState, name: string) {
    for (let depth = state.selection.$from.depth; depth > 0; --depth) {
      const node = state.selection.$from.node(depth);
      if (node.type.name === name) return { node, pos: state.selection.$from.before(depth) };
    }
    return null;
  }
  private describe(state: EditorState): void {
    const { from, to, empty, $from } = state.selection;
    const active = (name: string) => empty ? !!state.schema.marks[name].isInSet(state.storedMarks ?? $from.marks()) : state.doc.rangeHasMark(from, to, state.schema.marks[name]);
    const list = this.ancestor(state, 'bullet_list') ?? this.ancestor(state, 'ordered_list');
    const block = $from.parent;
    this.selection.set({ bold: active('strong'), italic: active('em'), strike: active('strike'), inlineCode: active('code'), quote: !!this.ancestor(state, 'blockquote'),
      list: list?.node.type.name ?? '', block: block.type.name === 'heading' ? `h${block.attrs['level']}` : block.type.name,
      table: isInTable(state), header: !!this.ancestor(state, 'table_header'), image: state.selection instanceof NodeSelection && state.selection.node.type.name === 'image' });
  }
  private run(command: Command): void {
    const view = this.instance; if (!view || !this.editable()) return;
    command(view.state, view.dispatch, view); view.focus();
  }
  focusSection(id: string): void {
    const view = this.instance; if (!view) return;
    let pos: number | undefined;
    view.state.doc.descendants((node, start) => { if (pos === undefined && node.type.name === 'heading' && node.attrs['sectionId'] === id) pos = start + 1; });
    if (pos !== undefined) { view.dispatch(view.state.tr.setSelection(TextSelection.near(view.state.doc.resolve(pos))).scrollIntoView()); view.focus(); }
  }
  hasPendingFields(): boolean { return this.linkOpen() || this.imageOpen(); }
  snapshot(): RichEditorSnapshot | null {
    return this.instance ? { state: this.instance.state, linkOpen: this.linkOpen(), linkUrl: this.linkUrl(), linkError: this.linkError(),
      imageOpen: this.imageOpen(), imageAlt: this.imageAlt(), error: this.error() } : null;
  }
  flushFields(): boolean {
    if (this.hasPendingFields() && !this.editable()) return false;
    if (this.linkOpen()) this.applyLink();
    if (this.imageOpen()) this.applyImage();
    return !this.hasPendingFields() && !this.error();
  }
  mark(name: string): void { const schema = this.instance?.state.schema; if (schema) this.run(toggleMark(schema.marks[name])); }
  undo(): void { this.run(undo); }
  redo(): void { this.run(redo); }
  block(value: string): void {
    if (this.selection().table) return;
    this.run((state, dispatch) => {
      const type = value.startsWith('h') ? state.schema.nodes['heading'] : state.schema.nodes[value];
      if (!type) return false;
      const transaction = state.tr;
      state.doc.nodesBetween(state.selection.from, state.selection.to, (node, pos) => {
        if (!node.isTextblock) return;
        const attrs = { sourceKey: node.attrs['sourceKey'] ?? null,
          ...(value.startsWith('h') ? { level: Number(value.slice(1)), sectionId: node.type.name === 'heading' ? node.attrs['sectionId'] : null } : {}),
          ...(value === 'code_block' ? { params: node.attrs['params'] ?? '' } : {}) };
        // Apply each selected block separately to keep its own stable ID.
        const start = transaction.mapping.map(pos);
        transaction.setBlockType(start, start + node.nodeSize, type, attrs);
      });
      if (!transaction.docChanged) return false;
      if (dispatch) dispatch(transaction.scrollIntoView()); return true;
    });
  }
  quote(): void { const schema = this.instance?.state.schema; if (schema) this.run(this.selection().quote ? lift : wrapIn(schema.nodes['blockquote'])); }
  list(kind: 'bullet_list' | 'ordered_list'): void {
    this.run((state, dispatch) => {
      const current = this.ancestor(state, 'bullet_list') ?? this.ancestor(state, 'ordered_list');
      if (current?.node.type.name === kind) return liftListItem(state.schema.nodes['list_item'])(state, dispatch);
      if (current) { if (dispatch) dispatch(state.tr.setNodeMarkup(current.pos, state.schema.nodes[kind], { ...current.node.attrs, order: 1 })); return true; }
      return wrapInList(state.schema.nodes[kind], { tight: true, order: 1 })(state, dispatch);
    });
  }
  task(): void {
    if (!this.instance) return;
    if (!this.ancestor(this.instance.state, 'list_item')) this.list('bullet_list');
    this.run((state, dispatch) => {
      const item = this.ancestor(state, 'list_item'); if (!item) return false;
      if (dispatch) dispatch(state.tr.setNodeMarkup(item.pos, undefined, { ...item.node.attrs, checked: !item.node.attrs['checked'] })); return true;
    });
  }
  table(): void {
    this.run((state, dispatch) => {
      const nodes = state.schema.nodes;
      const cell = (header: boolean) => nodes[header ? 'table_header' : 'table_cell'].createAndFill()!;
      const table = nodes['table'].create(null, [nodes['table_row'].create(null, [cell(true), cell(true)]), nodes['table_row'].create(null, [cell(false), cell(false)])]);
      if (dispatch) {
        const transaction = state.tr.replaceSelectionWith(table);
        let start: number | undefined;
        transaction.doc.descendants((node, pos) => { if (node === table) start = pos; });
        if (start !== undefined) transaction.setSelection(TextSelection.near(transaction.doc.resolve(start + 4)));
        dispatch(transaction.scrollIntoView());
      }
      return true;
    });
  }
  tableAction(action: 'row' | 'column' | 'deleteRow' | 'deleteColumn' | 'deleteTable'): void {
    if (!this.selection().table || action === 'deleteRow' && this.selection().header) return;
    this.run({ row: addRowAfter, column: addColumnAfter, deleteRow, deleteColumn, deleteTable }[action]);
  }
  openLink(): void {
    const state = this.instance?.state; if (!state) return;
    const mark = state.schema.marks['link'].isInSet(state.storedMarks ?? state.selection.$from.marks());
    this.linkUrl.set(mark?.attrs['href'] ?? ''); this.linkError.set(''); this.linkOpen.set(true);
    requestAnimationFrame(() => this.linkInput()?.nativeElement.focus());
  }
  applyLink(): void {
    if (!this.editable()) return;
    const href = this.linkUrl().trim();
    if (!safeLink(href)) { this.linkError.set('יש להזין קישור HTTPS או הפניה פנימית תקינה.'); return; }
    const schema = this.instance?.state.schema;
    if (schema) this.run((state, dispatch) => {
      const { from, to, empty } = state.selection;
      const mark = schema.marks['link'].create({ href, title: null });
      if (dispatch) dispatch(empty ? state.tr.addStoredMark(mark) : state.tr.addMark(from, to, mark)); return true;
    });
    this.linkOpen.set(false);
  }
  removeLink(): void {
    if (!this.editable()) return;
    this.run((state, dispatch) => {
      if (dispatch) dispatch(state.tr.removeMark(state.selection.from, state.selection.to, state.schema.marks['link']).removeStoredMark(state.schema.marks['link']));
      return true;
    });
    this.linkOpen.set(false);
  }
  closeLink(): void { this.linkOpen.set(false); this.instance?.focus(); }
  openImage(): void {
    const selection = this.instance?.state.selection;
    if (selection instanceof NodeSelection && selection.node.type.name === 'image') {
      this.imageAlt.set(selection.node.attrs['alt'] ?? ''); this.imageOpen.set(true);
    }
  }
  applyImage(): void {
    if (!this.editable()) return;
    if (!this.imageAlt().trim()) return;
    this.run((state, dispatch) => { const selection = state.selection; if (!(selection instanceof NodeSelection) || selection.node.type.name !== 'image') return false;
      if (dispatch) dispatch(state.tr.setNodeMarkup(selection.from, undefined, { ...selection.node.attrs, alt: this.imageAlt() })); return true; });
    this.imageOpen.set(false);
  }
}
