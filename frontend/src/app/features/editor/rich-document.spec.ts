import { EditorState, TextSelection } from 'prosemirror-state';
import { history, undo, redo } from 'prosemirror-history';
import { DOMSerializer } from 'prosemirror-model';
import { RichDocument } from './rich-document';
import type { EditorModel, EditorEdit } from '../../core/api/editor-api';
import contract from '../../../../../fixtures/editor-contract.json';

interface Contract { name: string; model: EditorModel; rename: { key: string; text: string; source: string; edit: EditorEdit }; }
const cases = contract as unknown as Contract[];
const context = { repository: '00000000-0000-0000-0000-000000000001', proposal: '00000000-0000-0000-0000-000000000002', commit: 'a'.repeat(40) };
function position(state: EditorState, key: string): number {
  let found = -1;
  state.doc.forEach((node, pos) => { if (node.attrs['sourceKey'] === key) found = pos; });
  if (found < 0) throw new Error('MISSING_TEST_NODE');
  return found;
}

describe('Source-preserving rich document', () => {
  it('round trips actual Rust models and produces the exact Rust-validated heading edit', () => {
    for (const entry of cases) {
      const rich = new RichDocument(entry.model, context);
      let state = EditorState.create({ doc: rich.doc, plugins: [history(), rich.identityPlugin()] });
      const keys: unknown[] = []; state.doc.forEach(node => keys.push(node.attrs['sourceKey'] ?? node.type.name));
      expect(keys).toEqual(entry.model.blocks.map(block => block.key));
      expect(rich.changes(state.doc).changed, entry.name).toBe(false);
      expect(rich.changes(state.doc).edit.blocks.every(block => block.action === 'keep'), entry.name).toBe(true);
      entry.model.blocks.forEach((block, index) => {
        if (!block.protected_reason && block.kind !== 'protected') expect(state.doc.child(index).type.name, `${entry.name}/${block.kind}`).not.toBe('protected_source');
      });
      const pos = position(state, entry.rename.key);
      const node = state.doc.nodeAt(pos)!;
      const renamed = node.type.create(node.attrs, state.schema.text(entry.rename.text), node.marks);
      state = state.applyTransaction(state.tr.replaceWith(pos, pos + node.nodeSize, renamed)).state;
      const changed = rich.changes(state.doc);
      expect(changed.markdown, entry.name).toBe(entry.rename.source);
      expect(changed.edit, entry.name).toEqual(entry.rename.edit);
      undo(state, transaction => { state = state.applyTransaction(transaction).state; });
      expect(rich.changes(state.doc).changed, entry.name).toBe(false);
      redo(state, transaction => { state = state.applyTransaction(transaction).state; });
      expect(rich.changes(state.doc).markdown, entry.name).toBe(entry.rename.source);
    }
  });

  it('preserves original identities when a copy is inserted before its source', () => {
    const entry = cases[0];
    const rich = new RichDocument(entry.model, context);
    let state = EditorState.create({ doc: rich.doc, plugins: [rich.identityPlugin()] });
    const pos = position(state, entry.rename.key);
    const original = state.doc.nodeAt(pos)!;
    state = state.applyTransaction(state.tr.insert(pos, original)).state;
    const copy = state.doc.nodeAt(pos)!;
    const movedOriginal = state.doc.nodeAt(pos + original.nodeSize)!;
    expect(copy.attrs['sourceKey']).toBeNull();
    expect(copy.attrs['sectionId']).not.toBe(original.attrs['sectionId']);
    expect(movedOriginal.attrs['sourceKey']).toBe(entry.rename.key);
    expect(movedOriginal.attrs['sectionId']).toBe(original.attrs['sectionId']);
    expect(rich.changes(state.doc).edit.deleted_section_ids).toEqual([]);
  });

  it('splits a paragraph without reusing source keys and assigns IDs to new headings', () => {
    const entry = cases[0];
    const rich = new RichDocument(entry.model, context);
    let state = EditorState.create({ doc: rich.doc, plugins: [rich.identityPlugin()] });
    const paragraph = entry.model.blocks.find(block => block.kind === 'paragraph')!;
    const pos = position(state, paragraph.key);
    state = state.applyTransaction(state.tr.setSelection(TextSelection.create(state.doc, pos + 2)).split(pos + 2)).state;
    const keys: string[] = [];
    state.doc.forEach(node => { if (node.attrs['sourceKey']) keys.push(node.attrs['sourceKey']); });
    expect(new Set(keys).size).toBe(keys.length);
    state = state.applyTransaction(state.tr.insert(state.doc.content.size, state.schema.nodes['heading'].create({ level: 4 }, state.schema.text('חדש')))).state;
    const heading = state.doc.lastChild!;
    expect(heading.attrs['sectionId']).toMatch(/^SEC-[A-F0-9-]+$/);
    expect(rich.changes(state.doc).edit.blocks.some(block => block.action === 'insert')).toBe(true);
  });

  it('blocks rich deletion of protected source while allowing local undoable edits nearby', () => {
    const entry = cases.find(entry => entry.name === 'gfm-protected')!;
    const rich = new RichDocument(entry.model, context);
    const notice = vi.fn();
    let state = EditorState.create({ doc: rich.doc, plugins: [rich.identityPlugin(notice)] });
    const block = entry.model.blocks.find(block => !!block.protected_reason)!;
    const pos = position(state, block.key);
    const before = state.doc;
    state = state.applyTransaction(state.tr.delete(pos, pos + state.doc.nodeAt(pos)!.nodeSize)).state;
    expect(state.doc).toBe(before);
    expect(notice).toHaveBeenCalledWith('PROTECTED_EDITOR_BLOCK');
    expect(rich.changes(state.doc).changed).toBe(false);
  });

  it('keeps GFM table alignment, escaped pipes, task states and nested heading IDs through serialization', () => {
    const entry = cases.find(entry => entry.name === 'gfm-protected')!;
    const rich = new RichDocument(entry.model, context);
    const nodes = ['table', 'bullet_list', 'blockquote'];
    for (const name of nodes) {
      let found = false;
      rich.doc.forEach(node => {
        if (node.type.name !== name) return;
        found = true;
        const serialized = rich.format.serialize(node);
        const parsed = rich.format.parser.parse(serialized);
        expect(parsed.firstChild!.textContent, name).toBe(node.textContent);
        if (name === 'table') { expect(serialized).toContain(':---'); expect(serialized).toContain('---:'); expect(serialized).toContain('\\|'); }
        if (name === 'bullet_list') { expect(serialized).toContain('[x]'); expect(serialized).toContain('[ ]'); }
        if (name === 'blockquote') expect(serialized).toContain('SEC-NESTED');
      });
      expect(found, name).toBe(true);
    }
  });

  it('creates inert DOM, pinned private images and safe links from the schema', () => {
    const entry = cases.find(entry => entry.name === 'gfm-protected')!;
    const rich = new RichDocument(entry.model, context);
    const fragment = DOMSerializer.fromSchema(rich.format.schema).serializeFragment(rich.doc.content);
    const host = document.createElement('div'); host.append(fragment);
    expect(host.querySelector('script')).toBeNull();
    expect(host.querySelector('.protected-source')?.getAttribute('contenteditable')).toBe('false');
    expect(host.querySelector('img')?.getAttribute('src')).toBe(`/api/v1/proposals/${context.proposal}/assets/${'a'.repeat(64)}.png?commit=${context.commit}`);
    const unsafe = rich.format.schema.nodes['image'].create({ src: 'https://tracker.invalid/image', alt: 'blocked' });
    const image = DOMSerializer.fromSchema(rich.format.schema).serializeNode(unsafe) as HTMLElement;
    expect(image.hasAttribute('src')).toBe(false);
    const badLink = rich.format.schema.marks['link'].create({ href: 'javascript:alert(1)' });
    const linked = rich.format.schema.nodes['paragraph'].create(null, rich.format.schema.text('link', [badLink]));
    const dom = DOMSerializer.fromSchema(rich.format.schema).serializeNode(linked) as HTMLElement;
    expect(dom.querySelector('a')?.getAttribute('href')).toBe('#');
  });
});
