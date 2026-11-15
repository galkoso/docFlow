import { Node as RichNode } from 'prosemirror-model';
import { Plugin } from 'prosemirror-state';
import { Mapping } from 'prosemirror-transform';
import type { EditorBlock, EditorEdit, EditorModel } from '../../core/api/editor-api';
import type { AssetContext } from '../specification/render-model';
import { markdownSchema, sectionId } from './markdown-schema';

const kinds: Record<string, string[]> = { heading: ['heading'], paragraph: ['paragraph'], list: ['ordered_list', 'bullet_list'],
  quote: ['blockquote'], code: ['code_block'], table: ['table'] };
const validId = (value: unknown): value is string => typeof value === 'string' && /^[A-Z0-9-]{1,120}$/.test(value);
const keyOf = (node: RichNode): string | null => typeof node.attrs['sourceKey'] === 'string' ? node.attrs['sourceKey'] : null;
function headings(doc: RichNode): Set<string> {
  const result = new Set<string>();
  doc.descendants(node => { if (node.type.name === 'heading' && validId(node.attrs['sectionId'])) result.add(node.attrs['sectionId']); });
  return result;
}
function semantic(node: RichNode): string {
  // ProseMirror's toJSON shares attribute objects with the immutable node.
  // Build fresh objects for comparison instead of deleting attributes in place.
  const clean = (current: RichNode): unknown => ({ type: current.type.name,
    attrs: Object.fromEntries(Object.entries(current.attrs).filter(([name]) => name !== 'sourceKey')),
    text: current.text, marks: current.marks.map(mark => mark.toJSON()),
    content: Array.from({ length: current.childCount }, (_, index) => clean(current.child(index))) });
  return JSON.stringify(clean(node));
}

/** One document/actor/version owns this instance. ProseMirror is a local buffer;
 * the returned changes still require server preview and the Git save boundary. */
export class RichDocument {
  readonly format;
  readonly doc: RichNode;
  readonly originalSource: string;
  private readonly originals = new Map<string, { block: EditorBlock; node: RichNode }>();
  private readonly protectedKeys = new Set<string>();
  private readonly originalHeadings: Set<string>;
  private readonly hiddenHeadings: Set<string>;

  constructor(readonly model: EditorModel, context: AssetContext) {
    if (model.blocks.length > 10000 || !/^[a-f0-9]{64}$/.test(model.sha256)) throw new Error('INVALID_EDITOR_MODEL');
    const encoder = new TextEncoder();
    let offset = encoder.encode(model.preamble).length;
    for (const block of model.blocks) {
      const start = offset + encoder.encode(block.prefix).length;
      offset = start + encoder.encode(block.markdown).length;
      if (start !== block.source_start_byte || offset !== block.source_end_byte || !/^[a-f0-9]{64}$/.test(block.key)) throw new Error('INVALID_EDITOR_MODEL');
    }
    this.originalSource = model.preamble + model.blocks.map(block => block.prefix + block.markdown).join('') + model.suffix;
    if (encoder.encode(this.originalSource).length > 2 * 1024 * 1024) throw new Error('FILE_TOO_LARGE');
    this.format = markdownSchema(context);
    const env = {};
    this.format.tokenizer.parse(model.blocks.map(block => block.prefix + block.markdown).join('') + model.suffix, env);
    const nodes = model.blocks.map(block => {
      let node: RichNode;
      try {
        if (block.protected_reason || block.kind === 'protected') throw new Error('PROTECTED_SOURCE');
        const parsed = this.format.parser.parse(block.markdown, env);
        if (parsed.childCount !== 1 || !kinds[block.kind]?.includes(parsed.firstChild!.type.name)) throw new Error('UNSUPPORTED_SOURCE');
        node = parsed.firstChild!;
        node.descendants(child => { if (child.type.name === 'section_marker') throw new Error('ORPHAN_MARKER'); });
        node = node.type.create({ ...node.attrs, sourceKey: block.key,
          ...(node.type.name === 'heading' ? { sectionId: block.section_id } : {}) }, node.content, node.marks);
        const canonical = this.format.parser.parse(this.format.serialize(node), env);
        if (canonical.childCount !== 1 || semantic(node) !== semantic(canonical.firstChild!)) throw new Error('UNSUPPORTED_ROUND_TRIP');
      } catch {
        node = this.format.schema.nodes['protected_source'].create({ sourceKey: block.key, reason: block.protected_reason ?? 'UNSUPPORTED_EDITOR_BLOCK' });
        this.protectedKeys.add(block.key);
      }
      if (this.originals.has(block.key)) throw new Error('DUPLICATE_EDITOR_BLOCK');
      this.originals.set(block.key, { block, node });
      return node;
    });
    this.doc = this.format.schema.nodes['doc'].createAndFill(null, nodes)!;
    this.originalHeadings = headings(this.doc);
    this.hiddenHeadings = new Set(model.section_ids.filter(id => !this.originalHeadings.has(id)));
  }

  protectedSource(key: string): EditorBlock | undefined { return this.protectedKeys.has(key) ? this.originals.get(key)?.block : undefined; }

  changes(doc: RichNode): { edit: EditorEdit; markdown: string; changed: boolean } {
    if (doc.type.schema !== this.format.schema || doc.childCount > 10000) throw new Error('INVALID_EDITOR_MODEL');
    const seen = new Set<string>();
    const blocks: EditorEdit['blocks'] = [];
    const encoder = new TextEncoder();
    const parts: string[] = [];
    let bytes = 0;
    const append = (value: string) => { bytes += encoder.encode(value).length;
      if (bytes > 2 * 1024 * 1024) throw new Error('FILE_TOO_LARGE'); parts.push(value); };
    append(this.model.preamble);
    doc.forEach(node => {
      const key = keyOf(node);
      const original = key ? this.originals.get(key) : undefined;
      if (key && !original) throw new Error('UNKNOWN_EDITOR_BLOCK');
      if (key && seen.has(key)) throw new Error('DUPLICATE_EDITOR_BLOCK');
      if (original && this.protectedKeys.has(key!)) {
        if (!node.eq(original.node)) throw new Error('PROTECTED_EDITOR_BLOCK');
        seen.add(key!); blocks.push({ action: 'keep', key: key! }); append(original.block.prefix + original.block.markdown);
      } else if (original && original.node.type === node.type) {
        seen.add(key!);
        if (node.eq(original.node)) { blocks.push({ action: 'keep', key: key! }); append(original.block.prefix + original.block.markdown); }
        else {
          const source = this.format.serialize(node, node.type.name === 'heading' && original.block.section_id === node.attrs['sectionId']);
          blocks.push({ action: 'replace', key: key!, markdown: source }); append(original.block.prefix + source);
        }
      } else if (!(node.type.name === 'paragraph' && node.content.size === 0)) {
        const source = '\n\n' + this.format.serialize(node);
        blocks.push({ action: 'insert', markdown: source }); append(source);
      }
    });
    for (const key of this.protectedKeys) if (!seen.has(key)) throw new Error('PROTECTED_EDITOR_BLOCK');
    const currentHeadings = headings(doc);
    const deleted_section_ids = [...this.originalHeadings].filter(id => !currentHeadings.has(id)).sort();
    const deleted_keys = [...this.originals.keys()].filter(key => !seen.has(key));
    append(this.model.suffix);
    const markdown = parts.join('');
    return { edit: { expected_sha256: this.model.sha256, blocks, deleted_keys, deleted_section_ids }, markdown, changed: markdown !== this.originalSource };
  }

  identityPlugin(notice: (code: string) => void = () => {}): Plugin {
    return new Plugin({
      filterTransaction: transaction => {
        if (!transaction.docChanged) return true;
        const retained = new Set<string>();
        let valid = true;
        transaction.doc.forEach(node => {
          const key = keyOf(node);
          if (key && this.protectedKeys.has(key)) {
            if (retained.has(key) || !node.eq(this.originals.get(key)!.node)) valid = false;
            retained.add(key);
          }
        });
        if (!valid || retained.size !== this.protectedKeys.size) { notice('PROTECTED_EDITOR_BLOCK'); return false; }
        return true;
      },
      appendTransaction: (transactions, oldState, state) => {
        if (!transactions.some(transaction => transaction.docChanged)) return null;
        const mapping = new Mapping();
        for (const transaction of transactions) mapping.appendMapping(transaction.mapping);
        const preferredKeys = new Map<string, number>();
        const preferredIds = new Map<string, number>();
        oldState.doc.forEach((node, pos) => {
          const key = keyOf(node); const mapped = mapping.mapResult(pos, 1);
          const current = state.doc.nodeAt(mapped.pos);
          if (key && !mapped.deleted && current && keyOf(current) === key) preferredKeys.set(key, mapped.pos);
        });
        oldState.doc.descendants((node, pos) => {
          const id = node.attrs['sectionId']; const mapped = mapping.mapResult(pos, 1);
          if (node.type.name === 'heading' && validId(id) && !mapped.deleted && state.doc.nodeAt(mapped.pos)?.attrs['sectionId'] === id) preferredIds.set(id, mapped.pos);
        });
        const usedKeys = new Set<string>();
        const usedIds = new Set<string>(this.hiddenHeadings);
        const allIds = new Set<string>(this.model.section_ids);
        state.doc.descendants(node => { if (node.type.name === 'heading' && validId(node.attrs['sectionId'])) allIds.add(node.attrs['sectionId']); });
        const transaction = state.tr;
        state.doc.descendants((node, pos, parent) => {
          const attrs = { ...node.attrs };
          let changed = false;
          const key = keyOf(node);
          if (key && (parent !== state.doc || !this.originals.has(key) || usedKeys.has(key)
            || preferredKeys.has(key) && preferredKeys.get(key) !== pos)) { attrs['sourceKey'] = null; changed = true; }
          else if (key) usedKeys.add(key);
          if (node.type.name === 'heading') {
            let id = attrs['sectionId'];
            if (!validId(id) || usedIds.has(id) || preferredIds.has(id) && preferredIds.get(id) !== pos) {
              do { id = sectionId(); } while (allIds.has(id));
              attrs['sectionId'] = id; allIds.add(id); changed = true;
            }
            usedIds.add(id);
          }
          if (changed) transaction.setNodeMarkup(pos, undefined, attrs);
        });
        return transaction.docChanged ? transaction : null;
      },
    });
  }
}
