import { EditorSelection, EditorState, SelectionRange, Transaction } from '@codemirror/state';
import { KeyBinding } from '@codemirror/view';

// ---------------------------------------------------------------------------------------------
// Text formatting shortcuts (Mod-b, Mod-i, Mod-u) for LaTeX and Typst.
//
// Each command toggles: on plain text it wraps the selection (or inserts an empty pair at the
// cursor), and on text that is already wrapped (wrapper selected, or just outside the
// selection) it removes the wrapper.
// ---------------------------------------------------------------------------------------------

interface Wrap {
  open: string;
  close: string;
}

type Style = 'bold' | 'italic' | 'underline';
type Dialect = 'latex' | 'typst';

/** Recognised wrappers per style. The first one is used when adding formatting. */
const FORMS: Record<Dialect, Record<Style, Wrap[]>> = {
  latex: {
    bold: [{ open: '\\textbf{', close: '}' }],
    italic: [
      { open: '\\textit{', close: '}' },
      { open: '\\emph{', close: '}' },
    ],
    underline: [{ open: '\\underline{', close: '}' }],
  },
  typst: {
    // `*x*` / `_x_` markup does not work inside a word or across paragraphs; the function forms do.
    bold: [
      { open: '*', close: '*' },
      { open: '#strong[', close: ']' },
    ],
    italic: [
      { open: '_', close: '_' },
      { open: '#emph[', close: ']' },
    ],
    underline: [{ open: '#underline[', close: ']' }],
  },
};

const OPENER: Record<string, string> = { '}': '{', ']': '[' };

/** Whether `inner` can be the whole content of `wrap`, i.e. it does not close the wrapper early. */
function fitsInside(inner: string, wrap: Wrap): boolean {
  if (wrap.open === wrap.close) return !inner.includes(wrap.close);
  const opener = OPENER[wrap.close];
  if (!opener) return true;
  let depth = 0;
  for (let i = 0; i < inner.length; i++) {
    const c = inner[i]!;
    if (c === '\\') i++; // skip escaped characters such as \{ or \]
    else if (c === opener) depth++;
    else if (c === wrap.close && --depth < 0) return false;
  }
  return depth === 0;
}

/** A symmetric markup wrap (Typst `*`/`_`) only works at word boundaries and within one paragraph. */
function markupUsable(state: EditorState, from: number, to: number, wrap: Wrap): boolean {
  if (wrap.open !== wrap.close) return true;
  const word = /[\p{L}\p{N}_]/u;
  const before = state.sliceDoc(from - 1, from);
  const after = state.sliceDoc(to, to + 1);
  const text = state.sliceDoc(from, to);
  return !word.test(before) && !word.test(after) && !text.includes('\n') && !text.includes(wrap.open);
}

/** Shrink a range so that surrounding whitespace stays outside the wrapper. */
function trimRange(state: EditorState, range: SelectionRange): { from: number; to: number } {
  const text = state.sliceDoc(range.from, range.to);
  const start = text.length - text.trimStart().length;
  const end = text.trimEnd().length;
  return end <= start ? { from: range.from, to: range.to } : { from: range.from + start, to: range.from + end };
}

function toggleRange(state: EditorState, range: SelectionRange, forms: Wrap[]) {
  const { from, to } = trimRange(state, range);
  const text = state.sliceDoc(from, to);

  for (const w of forms) {
    // Wrapper just outside the selection: `\textbf{|sel|}`.
    if (
      state.sliceDoc(from - w.open.length, from) === w.open &&
      state.sliceDoc(to, to + w.close.length) === w.close &&
      fitsInside(text, w)
    ) {
      return {
        changes: [
          { from: from - w.open.length, to: from },
          { from: to, to: to + w.close.length },
        ],
        range: EditorSelection.range(from - w.open.length, to - w.open.length),
      };
    }
    // Wrapper included in the selection: `|\textbf{sel}|`.
    if (text.length >= w.open.length + w.close.length && text.startsWith(w.open) && text.endsWith(w.close)) {
      const inner = text.slice(w.open.length, text.length - w.close.length);
      if (fitsInside(inner, w)) {
        return { changes: { from, to, insert: inner }, range: EditorSelection.range(from, from + inner.length) };
      }
    }
  }

  const w = forms.find((f) => markupUsable(state, from, to, f)) ?? forms[forms.length - 1]!;
  return {
    changes: [
      { from, insert: w.open },
      { from: to, insert: w.close },
    ],
    range: EditorSelection.range(from + w.open.length, to + w.open.length),
  };
}

function toggleStyle(dialect: Dialect, style: Style) {
  const forms = FORMS[dialect][style];
  return ({ state, dispatch }: { state: EditorState; dispatch: (tr: Transaction) => void }): boolean => {
    if (state.readOnly) return false;
    const tr = state.update(state.changeByRange((range) => toggleRange(state, range, forms)), {
      scrollIntoView: true,
      userEvent: 'input',
    });
    dispatch(tr);
    return true;
  };
}

function dialectOf(path: string): Dialect | null {
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  if (ext === 'typ') return 'typst';
  if (ext === 'tex' || ext === 'ltx') return 'latex';
  return null;
}

/**
 * Formatting key bindings for the file at `path` (empty for files that are not LaTeX or Typst
 * documents). They take precedence over CodeMirror's defaults for Mod-i (select parent syntax)
 * and Mod-u (undo selection). Comment toggling (Mod-/) comes from the default keymap.
 */
export function formatKeymap(path: string): KeyBinding[] {
  const dialect = dialectOf(path);
  if (!dialect) return [];
  return [
    { key: 'Mod-b', run: toggleStyle(dialect, 'bold'), preventDefault: true },
    { key: 'Mod-i', run: toggleStyle(dialect, 'italic'), preventDefault: true },
    { key: 'Mod-u', run: toggleStyle(dialect, 'underline'), preventDefault: true },
  ];
}
