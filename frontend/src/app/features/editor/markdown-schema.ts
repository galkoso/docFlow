import { Schema, Node as RichNode, NodeSpec, MarkSpec } from 'prosemirror-model';
import { defaultMarkdownParser, defaultMarkdownSerializer, MarkdownParser, MarkdownSerializer } from 'prosemirror-markdown';
import { tableNodes } from 'prosemirror-tables';
import MarkdownIt from 'markdown-it';
import { assetSource, AssetContext, externalLink } from '../specification/render-model';

export const sectionId = () => `SEC-${crypto.randomUUID().toUpperCase()}`;
const anchor = /^<!-- ss:section:([A-Z0-9-]{1,120}) -->$/;
const asset = /^assets\/[a-f0-9]{64}\.(png|jpg|jpeg|webp)$/;
export const safeLink = (href: string): boolean => !!externalLink(href) || /^(#|ss:|docs\/)/.test(href) && !href.includes('..');

/** Source is never assigned to innerHTML. Only schema-defined DOM is created. */
export function markdownSchema(context: AssetContext) {
  let nodes = defaultMarkdownParser.schema.spec.nodes;
  nodes.forEach((name, spec) => {
    if (spec.group?.split(' ').includes('block')) nodes = nodes.update(name, { ...spec, attrs: { ...spec.attrs, sourceKey: { default: null } } });
  });
  const heading = nodes.get('heading')!;
  nodes = nodes.update('heading', { ...heading, attrs: { ...heading.attrs, sectionId: { default: null } },
    parseDOM: [1, 2, 3, 4].map(level => ({ tag: `h${level}`, getAttrs: dom => ({ level,
      sectionId: (dom as HTMLElement).getAttribute('data-section-id'), sourceKey: (dom as HTMLElement).getAttribute('data-source-key') }) })),
    toDOM: node => [`h${Math.min(4, Math.max(1, node.attrs['level']))}`, { 'data-section-id': node.attrs['sectionId'], 'data-source-key': node.attrs['sourceKey'] }, 0] });
  const item = nodes.get('list_item')!;
  nodes = nodes.update('list_item', { ...item, attrs: { ...item.attrs, checked: { default: null } },
    parseDOM: [{ tag: 'li', getAttrs: dom => ({ checked: (dom as HTMLElement).hasAttribute('data-task')
      ? (dom as HTMLElement).getAttribute('data-task') === 'true' : null }) }],
    toDOM: node => ['li', node.attrs['checked'] === null ? {} : { 'data-task': String(node.attrs['checked']) }, 0] });
  nodes = nodes.update('image', { ...nodes.get('image')!,
    parseDOM: [{ tag: 'img[data-asset-path]', getAttrs: dom => {
      const path = (dom as HTMLElement).getAttribute('data-asset-path') ?? '';
      return asset.test(path) ? { src: path, alt: (dom as HTMLElement).getAttribute('alt') ?? '', title: (dom as HTMLElement).getAttribute('title') } : false;
    } }],
    toDOM: node => ['img', { src: assetSource(context, node.attrs['src']) ?? undefined, 'data-asset-path': node.attrs['src'],
      alt: node.attrs['alt'] ?? '', title: node.attrs['title'], loading: 'lazy', referrerpolicy: 'no-referrer' }] });
  const tables = tableNodes({ tableGroup: 'block', cellContent: 'paragraph', cellAttributes: {
    alignment: { default: null, getFromDOM: dom => ['left', 'right', 'center'].includes(dom.style.textAlign) ? dom.style.textAlign : null,
      setDOMAttr: (value, attrs) => { if (['left', 'right', 'center'].includes(String(value))) attrs['style'] = `text-align:${value}`; } },
  } });
  tables.table = { ...tables.table, attrs: { sourceKey: { default: null } } };
  nodes = nodes.append(tables);
  const protectedNode: NodeSpec = { group: 'block', atom: true, isolating: true, selectable: true,
    attrs: { sourceKey: { default: null }, reason: { default: 'UNSUPPORTED_EDITOR_BLOCK' } },
    toDOM: node => ['div', { class: 'protected-source', contenteditable: 'false', 'data-protected-key': node.attrs['sourceKey'] }, 'רכיב מקור מוגן'] };
  nodes = nodes.addBefore('text', 'protected_source', protectedNode);
  nodes = nodes.addBefore('text', 'section_marker', { group: 'block', atom: true, attrs: { id: {} },
    toDOM: node => ['div', { contenteditable: 'false', class: 'source-marker' }, `מזהה מקור: ${node.attrs['id']}`] });
  const link: MarkSpec = { ...defaultMarkdownParser.schema.spec.marks.get('link')!,
    parseDOM: [{ tag: 'a[href]', getAttrs: dom => {
      const href = (dom as HTMLElement).getAttribute('data-destination') ?? (dom as HTMLElement).getAttribute('href') ?? '';
      return safeLink(href) ? { href, title: (dom as HTMLElement).getAttribute('title') } : false;
    } }],
    toDOM: mark => ['a', { href: externalLink(mark.attrs['href']) ?? '#', 'data-destination': mark.attrs['href'],
      title: mark.attrs['title'], rel: 'noopener noreferrer' }, 0] };
  let marks = defaultMarkdownParser.schema.spec.marks.update('link', link);
  marks = marks.addBefore('code', 'strike', { parseDOM: [{ tag: 's' }, { tag: 'del' }], toDOM: () => ['s', 0] });
  const schema = new Schema({ nodes, marks });
  const tokenizer = new MarkdownIt('default', { html: false, linkify: false, typographer: false });
  tokenizer.block.ruler.before('paragraph', 'ss_section', (state, start, _end, silent) => {
    const line = state.src.slice(state.bMarks[start] + state.tShift[start], state.eMarks[start]).trim();
    const match = anchor.exec(line);
    if (!match) return false;
    if (silent) return true;
    const token = state.push('ss_section', '', 0); token.meta = { id: match[1] }; token.map = [start, start + 1];
    state.line = start + 1;
    return true;
  });
  tokenizer.core.ruler.after('inline', 'editor_metadata', state => {
    const tokens = state.tokens;
    for (let i = 0; i < tokens.length; ++i) {
      if (tokens[i].type === 'ss_section' && tokens[i + 1]?.type === 'heading_open') {
        tokens[i + 1].meta = { sectionId: tokens[i].meta?.['id'] }; tokens[i].type = 'ss_attached';
      }
      if (tokens[i].type === 'list_item_open' && tokens[i + 1]?.type === 'paragraph_open' && tokens[i + 2]?.type === 'inline') {
        const inline = tokens[i + 2];
        const text = inline.children?.[0];
        const task = text?.type === 'text' ? /^\[([ xX])\]\s+/.exec(text.content) : null;
        if (task && text) { tokens[i].meta = { checked: task[1] !== ' ' }; text.content = text.content.slice(task[0].length); }
      }
    }
  });
  const parser = new MarkdownParser(schema, tokenizer, { ...defaultMarkdownParser.tokens,
    heading: { block: 'heading', getAttrs: token => ({ level: Number(token.tag.slice(1)), sectionId: token.meta?.['sectionId'] ?? null }) },
    list_item: { block: 'list_item', getAttrs: token => ({ checked: token.meta?.['checked'] ?? null }) },
    image: { node: 'image', getAttrs: token => {
      const src = token.attrGet('src') ?? ''; if (!asset.test(src)) throw new Error('UNSAFE_IMAGE');
      return { src, alt: token.content, title: token.attrGet('title') ?? null };
    } },
    link: { mark: 'link', getAttrs: token => {
      const href = token.attrGet('href') ?? ''; if (!safeLink(href)) throw new Error('UNSAFE_LINK');
      return { href, title: token.attrGet('title') ?? null };
    } },
    s: { mark: 'strike' }, ss_attached: { ignore: true, noCloseToken: true },
    ss_section: { node: 'section_marker', getAttrs: token => ({ id: token.meta?.['id'] }) },
    table: { block: 'table' }, thead: { ignore: true }, tbody: { ignore: true }, tr: { block: 'table_row' },
    th: { block: 'table_header', getAttrs: token => ({ alignment: token.attrGet('style')?.split(':')[1] ?? null }) },
    td: { block: 'table_cell', getAttrs: token => ({ alignment: token.attrGet('style')?.split(':')[1] ?? null }) },
  });
  // Markdown tables have inline cells; the schema deliberately supports one
  // paragraph per cell. Wrap those inline tokens before MarkdownParser sees them.
  tokenizer.core.ruler.after('editor_metadata', 'editor_table_paragraphs', state => {
    const result = [];
    for (const token of state.tokens) {
      if (token.type === 'th_close' || token.type === 'td_close') result.push(new state.Token('paragraph_close', 'p', -1));
      result.push(token);
      if (token.type === 'th_open' || token.type === 'td_open') result.push(new state.Token('paragraph_open', 'p', 1));
    }
    state.tokens = result;
  });
  return { schema, parser, tokenizer, serialize: (node: RichNode, skipTopHeading = false) => serializeBlock(node, skipTopHeading) };
}

function serializeBlock(node: RichNode, skipTopHeading: boolean): string {
  let serializer: MarkdownSerializer;
  serializer = new MarkdownSerializer({ ...defaultMarkdownSerializer.nodes,
    heading: (state, current, parent, index) => {
      if (!(current === node && skipTopHeading)) {
        if (!current.attrs['sectionId']) throw new Error('SECTION_ID_REQUIRED');
        state.write(`<!-- ss:section:${current.attrs['sectionId']} -->\n`);
      }
      defaultMarkdownSerializer.nodes['heading'](state, current, parent, index);
    },
    section_marker: (state, current) => { state.write(`<!-- ss:section:${current.attrs['id']} -->`); state.closeBlock(current); },
    list_item: (state, current, parent, index) => {
      if (current.attrs['checked'] !== null) state.write(`[${current.attrs['checked'] ? 'x' : ' '}] `);
      defaultMarkdownSerializer.nodes['list_item'](state, current, parent, index);
    },
    protected_source: () => { throw new Error('PROTECTED_EDITOR_BLOCK'); },
    table: (state, table) => {
      const rows: string[][] = [];
      const alignments: (string | null)[] = [];
      table.forEach((row, _offset, rowIndex) => {
        const cells: string[] = [];
        row.forEach((cell, _offset, column) => {
          if (cell.attrs['colspan'] !== 1 || cell.attrs['rowspan'] !== 1 || cell.childCount !== 1
            || cell.type.name !== (rowIndex === 0 ? 'table_header' : 'table_cell')) throw new Error('UNSUPPORTED_TABLE_SHAPE');
          cell.descendants(child => { if (child.type.name === 'hard_break' || child.isText && child.text?.includes('\n')) throw new Error('UNSUPPORTED_TABLE_LINE_BREAK'); });
          const text = serializer.serialize(table.type.schema.nodes['doc'].create(null, cell.content)).trimEnd();
          cells.push(text.replace(/\|/g, '\\|'));
          if (!rowIndex) alignments[column] = cell.attrs['alignment'];
        });
        if (rows.length && cells.length !== rows[0].length) throw new Error('UNSUPPORTED_TABLE_SHAPE');
        rows.push(cells);
      });
      if (!rows.length) throw new Error('UNSUPPORTED_TABLE_SHAPE');
      const lines = rows.map(row => `| ${row.join(' | ')} |`);
      lines.splice(1, 0, `| ${alignments.map(alignment => alignment === 'left' ? ':---' : alignment === 'right' ? '---:' : alignment === 'center' ? ':---:' : '---').join(' | ')} |`);
      for (const line of lines) { state.write(line); state.ensureNewLine(); }
      state.closeBlock(table);
    },
  }, { ...defaultMarkdownSerializer.marks, strike: { open: '~~', close: '~~', mixable: true, expelEnclosingWhitespace: true } });
  return serializer.serialize(node.type.schema.nodes['doc'].create(null, [node])).trimEnd() + '\n\n';
}
