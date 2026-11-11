// Editor UI for AI edit markup (\AIadd, \AIdel, \AIdelmoved, \AInote; see aiMarkup.ts):
// highlights each mark, puts accept/reject buttons in front of it, and shows a panel with a count,
// navigation and accept all / reject all. Edits go through normal transactions, so they sync and undo.
import { EditorState, Extension, RangeSetBuilder, StateField } from '@codemirror/state';
import { Decoration, EditorView, Panel, showPanel, WidgetType } from '@codemirror/view';
import { AIMark, findAIMarks, resolveAll, resolveMark } from './aiMarkup';

const marksField = StateField.define<AIMark[]>({
  create: (s) => findAIMarks(s.doc.toString()),
  update: (marks, tr) => (tr.docChanged ? findAIMarks(tr.state.doc.toString()) : marks),
});

const LABEL: Record<AIMark['kind'], string> = { add: 'AI add', del: 'AI delete', delmoved: 'AI moved', note: 'AI note' };

/** Resolve the mark that starts at `from` (looked up again, since the document may have changed). */
function resolveAt(view: EditorView, from: number, accept: boolean): boolean {
  const mark = view.state.field(marksField).find((m) => m.from === from);
  if (!mark || view.state.readOnly) return false;
  const change = resolveMark(view.state.doc.toString(), mark, accept);
  view.dispatch({ changes: change, userEvent: accept ? 'input.ai.accept' : 'delete.ai.reject' });
  return true;
}

/** Resolve every mark, repeating for marks that were nested inside others (bounded, in case of surprises). */
function resolveEverything(view: EditorView, accept: boolean) {
  if (view.state.readOnly) return;
  for (let pass = 0; pass < 20; pass++) {
    const changes = resolveAll(view.state.doc.toString(), accept);
    if (!changes.length) return;
    view.dispatch({ changes, userEvent: accept ? 'input.ai.accept' : 'delete.ai.reject' });
  }
}

class ButtonsWidget extends WidgetType {
  constructor(readonly mark: AIMark, readonly readOnly: boolean) {
    super();
  }
  eq(o: ButtonsWidget) {
    return o.mark.from === this.mark.from && o.mark.kind === this.mark.kind && o.mark.tag === this.mark.tag && o.readOnly === this.readOnly;
  }
  toDOM(view: EditorView) {
    const wrap = document.createElement('span');
    wrap.className = `cm-ai-btns cm-ai-btns-${this.mark.kind}`;
    const label = document.createElement('span');
    label.className = 'cm-ai-label';
    label.textContent = this.mark.tag ? `${LABEL[this.mark.kind]} · ${this.mark.tag}` : LABEL[this.mark.kind];
    if (this.mark.where) label.title = `Moved to ${this.mark.where}`;
    wrap.append(label);
    if (this.readOnly) return wrap;
    const button = (text: string, title: string, accept: boolean, cls: string) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.className = `cm-ai-btn ${cls}`;
      b.textContent = text;
      b.title = title;
      // mousedown, not click: keeps the editor's selection and focus where they were.
      b.onmousedown = (e) => {
        e.preventDefault();
        resolveAt(view, this.mark.from, accept);
      };
      wrap.append(b);
    };
    if (this.mark.kind === 'note') {
      button('✓', 'Resolve: remove the note', true, 'cm-ai-accept');
    } else {
      const keeps = this.mark.kind === 'add' ? 'keep the new text' : 'remove the text';
      const undoes = this.mark.kind === 'add' ? 'drop the new text' : 'keep the text';
      button('✓', `Accept: ${keeps}`, true, 'cm-ai-accept');
      button('✗', `Reject: ${undoes}`, false, 'cm-ai-reject');
    }
    return wrap;
  }
  ignoreEvent() {
    return true;
  }
}

const decorations = EditorView.decorations.compute([marksField, EditorState.readOnly], (state) => {
  const b = new RangeSetBuilder<Decoration>();
  const ro = state.readOnly;
  for (const m of state.field(marksField)) {
    b.add(m.from, m.from, Decoration.widget({ widget: new ButtonsWidget(m, ro), side: -1 }));
    // Highlight the macro name/arguments faintly and the text that the decision is about strongly.
    if (m.from < m.body.from) b.add(m.from, m.body.from, Decoration.mark({ class: `cm-ai-${m.kind} cm-ai-syntax` }));
    if (m.body.from < m.body.to) b.add(m.body.from, m.body.to, Decoration.mark({ class: `cm-ai-${m.kind} cm-ai-body` }));
    if (m.body.to < m.to) b.add(m.body.to, m.to, Decoration.mark({ class: `cm-ai-${m.kind} cm-ai-syntax` }));
  }
  return b.finish();
});

function jump(view: EditorView, dir: 1 | -1) {
  const marks = view.state.field(marksField);
  if (!marks.length) return;
  const head = view.state.selection.main.head;
  const next = dir === 1 ? marks.find((m) => m.from > head) ?? marks[0] : [...marks].reverse().find((m) => m.from < head) ?? marks[marks.length - 1];
  view.dispatch({ selection: { anchor: next.from }, effects: EditorView.scrollIntoView(next.from, { y: 'center' }) });
  view.focus();
}

function panel(view: EditorView): Panel {
  const dom = document.createElement('div');
  dom.className = 'cm-ai-panel';
  const count = document.createElement('span');
  const btn = (text: string, title: string, run: () => void) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = text;
    b.title = title;
    b.onclick = run;
    return b;
  };
  const prev = btn('‹', 'Previous AI edit', () => jump(view, -1));
  const next = btn('›', 'Next AI edit', () => jump(view, 1));
  const acceptAll = btn('Accept all', 'Keep every \\AIadd, apply every \\AIdel, remove every \\AInote in this file', () => resolveEverything(view, true));
  const rejectAll = btn('Reject all', 'Drop every \\AIadd, restore every \\AIdel, remove every \\AInote in this file', () => resolveEverything(view, false));
  acceptAll.className = 'cm-ai-accept';
  rejectAll.className = 'cm-ai-reject';
  dom.append(count, prev, next, acceptAll, rejectAll);
  const render = (s: EditorState) => {
    const marks = s.field(marksField);
    const n = (k: AIMark['kind'][]) => marks.filter((m) => k.includes(m.kind)).length;
    count.textContent = `${marks.length} AI edit${marks.length === 1 ? '' : 's'} (${n(['add'])} add, ${n(['del', 'delmoved'])} delete, ${n(['note'])} note)`;
    acceptAll.disabled = rejectAll.disabled = s.readOnly;
  };
  render(view.state);
  return { dom, top: true, update: (u) => render(u.state) };
}

const panelExt = showPanel.compute([marksField], (s) => (s.field(marksField).length ? panel : null));

const theme = EditorView.baseTheme({
  '.cm-ai-syntax': { opacity: '0.55' },
  '&light .cm-ai-add.cm-ai-body': { backgroundColor: 'rgba(200, 0, 160, 0.10)', color: '#8a0070' },
  '&dark .cm-ai-add.cm-ai-body': { backgroundColor: 'rgba(255, 90, 220, 0.14)', color: '#ff9be8' },
  '.cm-ai-del.cm-ai-body, .cm-ai-delmoved.cm-ai-body': { textDecoration: 'line-through', textDecorationColor: '#d33' },
  '&light .cm-ai-del.cm-ai-body, &light .cm-ai-delmoved.cm-ai-body': { backgroundColor: 'rgba(220, 40, 40, 0.08)' },
  '&dark .cm-ai-del.cm-ai-body, &dark .cm-ai-delmoved.cm-ai-body': { backgroundColor: 'rgba(255, 80, 80, 0.12)' },
  '&light .cm-ai-note.cm-ai-body': { backgroundColor: 'rgba(120, 60, 200, 0.10)', fontStyle: 'italic' },
  '&dark .cm-ai-note.cm-ai-body': { backgroundColor: 'rgba(170, 120, 255, 0.14)', fontStyle: 'italic' },
  '.cm-ai-btns': {
    display: 'inline-flex', alignItems: 'center', gap: '2px', margin: '0 3px 0 1px', padding: '0 2px',
    borderRadius: '4px', fontFamily: 'system-ui, sans-serif', fontSize: '0.78em', verticalAlign: 'baseline',
    border: '1px solid rgba(128,128,128,0.35)',
  },
  '.cm-ai-label': { opacity: '0.75', padding: '0 3px' },
  '.cm-ai-btn': {
    border: 'none', background: 'transparent', cursor: 'pointer', padding: '0 4px', borderRadius: '3px',
    font: 'inherit', fontWeight: '700', color: 'inherit', lineHeight: '1.4',
  },
  '.cm-ai-btn.cm-ai-accept': { color: '#1a8f3c' },
  '.cm-ai-btn.cm-ai-reject': { color: '#c62828' },
  '.cm-ai-btn:hover': { backgroundColor: 'rgba(128,128,128,0.2)' },
  '.cm-ai-panel': {
    display: 'flex', alignItems: 'center', gap: '6px', padding: '4px 8px', fontFamily: 'system-ui, sans-serif', fontSize: '13px',
  },
  '.cm-ai-panel > span': { marginRight: 'auto', opacity: '0.85' },
  '.cm-ai-panel button': {
    font: 'inherit', padding: '1px 8px', borderRadius: '4px', cursor: 'pointer',
    border: '1px solid rgba(128,128,128,0.4)', background: 'transparent', color: 'inherit',
  },
  '.cm-ai-panel button.cm-ai-accept': { color: '#1a8f3c', borderColor: 'rgba(26,143,60,0.5)' },
  '.cm-ai-panel button.cm-ai-reject': { color: '#c62828', borderColor: 'rgba(198,40,40,0.5)' },
  '.cm-ai-panel button:disabled': { opacity: '0.4', cursor: 'default' },
});

/** AI edit markup support, for LaTeX sources only. */
export function aiMarkup(path: string): Extension {
  return /\.(tex|ltx)$/i.test(path) ? [marksField, decorations, panelExt, theme] : [];
}
