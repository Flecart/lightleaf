/**
 * Latex Suite for CodeMirror: snippets with tabstops, autofraction, tabout, matrix shortcuts and
 * auto-enlarged brackets, for LaTeX files only (the snippets produce LaTeX).
 *
 * Automatic snippets run from an input handler, so only characters the user types can trigger
 * them, never remote edits or pastes. An expansion is preceded by inserting the typed character
 * on its own and a break in undo capturing, so one undo gives back the typed trigger.
 */
import { EditorSelection, EditorState, Extension, Facet, Prec, StateEffect, StateField, TransactionSpec } from '@codemirror/state';
import { EditorView, KeyBinding, keymap } from '@codemirror/view';
import { CompiledConfig } from './config';
import { Context, inMath, isWithin, latexContext, strictlyInMath } from './context';
import { autofraction, enlargeBrackets, findExpansion, LOOKBEHIND, matrixTabout, parseReplacement, Range, taboutTarget } from './engine';

interface Hooks {
  config: CompiledConfig;
  /** Called before an expansion so it becomes its own undo step. */
  breakUndo: () => void;
}

const hooksFacet = Facet.define<Hooks, Hooks | null>({ combine: (v) => v[0] ?? null });

interface Tabstops {
  groups: Range[][];
  index: number;
}

const setTabstops = StateEffect.define<Tabstops | null>();

const tabstopField = StateField.define<Tabstops | null>({
  create: () => null,
  update(value, tr) {
    for (const e of tr.effects) if (e.is(setTabstops)) return e.value;
    if (!value) return null;
    let v = value;
    if (tr.docChanged) {
      v = {
        index: v.index,
        groups: v.groups.map((g) => g.map((r) => ({ from: tr.changes.mapPos(r.from, -1), to: tr.changes.mapPos(r.to, 1) }))),
      };
    }
    if (tr.selection) {
      const head = tr.state.selection.main.head;
      const inside = v.groups.slice(v.index).some((g) => g.some((r) => head >= r.from && head <= r.to));
      if (!inside) return null;
    }
    return v;
  },
});

function selectionFor(group: Range[]): EditorSelection {
  return EditorSelection.create(group.map((r) => EditorSelection.range(r.from, r.to)));
}

function context(state: EditorState): Context {
  return latexContext(state, state.selection.main.to);
}

/**
 * Replace `[from, to]` with a snippet replacement and enter its tabstops. Tabstops of a snippet
 * that is still being filled in stay queued after the new ones.
 */
function expandSpec(state: EditorState, from: number, to: number, replacement: string): TransactionSpec {
  const { text, groups } = parseReplacement(replacement);
  const abs = groups.map((g) => g.map((r) => ({ from: from + r.from, to: from + r.to })));
  const old = state.field(tabstopField, false);
  const changes = state.changes({ from, to, insert: text });
  const rest = old ? old.groups.slice(old.index + 1).map((g) => g.map((r) => ({ from: changes.mapPos(r.from, -1), to: changes.mapPos(r.to, 1) }))) : [];
  const selection = abs.length > 0 ? selectionFor(abs[0]!) : EditorSelection.cursor(from + text.length);
  return {
    changes,
    selection,
    // keep: without tabstops of its own, an expansion leaves the enclosing snippet's tabstops alone.
    effects: abs.length > 0 ? setTabstops.of({ groups: [...abs, ...rest], index: 0 }) : [],
    scrollIntoView: true,
    userEvent: 'input.complete',
  };
}

function enlargeAfter(view: EditorView, cfg: CompiledConfig) {
  if (!cfg.settings.autoEnlargeBrackets) return;
  const ctx = context(view.state);
  const b = ctx.bounds;
  if (!strictlyInMath(ctx) || !b) return;
  const changes = enlargeBrackets(cfg, view.state.sliceDoc(b.innerStart, b.innerEnd)).map((c) => ({
    from: c.from + b.innerStart,
    to: c.to + b.innerStart,
    insert: c.insert,
  }));
  if (changes.length > 0) view.dispatch({ changes, userEvent: 'input.complete' });
}

function expand(view: EditorView, hooks: Hooks, from: number, to: number, replacement: string, typed: string | null, enlarge: boolean) {
  let end = to;
  if (typed !== null) {
    view.dispatch({ changes: { from: to, insert: typed }, selection: EditorSelection.cursor(to + typed.length), userEvent: 'input.type' });
    end = to + typed.length;
  }
  hooks.breakUndo();
  view.dispatch(expandSpec(view.state, from, end, replacement));
  if (enlarge) enlargeAfter(view, hooks.config);
  hooks.breakUndo();
}

/** Run snippets (automatic ones for a typed `key`, the others for Tab with `key === null`). */
function runSnippets(view: EditorView, hooks: Hooks, key: string | null): boolean {
  const cfg = hooks.config;
  if (!cfg.settings.snippetsEnabled) return false;
  const state = view.state;
  if (state.selection.ranges.length !== 1) return false;
  const sel = state.selection.main;
  const ctx = context(state);
  const windowStart = Math.max(0, sel.to - LOOKBEHIND);
  const found = findExpansion(cfg, {
    before: state.sliceDoc(windowStart, sel.to),
    windowStart,
    selection: { from: sel.from, to: sel.to },
    selected: state.sliceDoc(sel.from, sel.to),
    key: key ?? '',
    charBefore: (p) => state.sliceDoc(p - 1, p),
    charAfter: (p) => state.sliceDoc(p, p + 1),
    ctx,
    automatic: key !== null,
  });
  if (!found) return false;
  const typed = key !== null && found.snippet.kind !== 'visual' ? key : null;
  const enlarge = inMath(ctx) && cfg.enlargeTriggers.some((t) => found.replacement.includes(t));
  expand(view, hooks, found.from, found.to, found.replacement, typed, enlarge);
  return true;
}

function runAutofraction(view: EditorView, hooks: Hooks): boolean {
  const cfg = hooks.config;
  const state = view.state;
  if (!cfg.settings.autofractionEnabled || state.selection.ranges.length !== 1) return false;
  const ctx = context(state);
  if (!strictlyInMath(ctx) || !ctx.bounds) return false;
  const sel = state.selection.main;
  const offset = ctx.bounds.innerStart;
  const r = autofraction(cfg, ctx, state.sliceDoc(offset, sel.to), offset, { from: sel.from, to: sel.to }, state.sliceDoc(sel.from, sel.to));
  if (!r) return false;
  expand(view, hooks, r.from, r.to, r.replacement, sel.empty ? '/' : null, true);
  return true;
}

function runTabout(view: EditorView, hooks: Hooks): boolean {
  const cfg = hooks.config;
  const state = view.state;
  if (!cfg.settings.taboutEnabled || !state.selection.main.empty) return false;
  const ctx = context(state);
  const pos = state.selection.main.head;
  const t = taboutTarget(cfg, ctx, state.doc.toString(), 0, pos);
  if (!t) return false;
  if (t.kind === 'cursor') {
    view.dispatch({ selection: EditorSelection.cursor(t.pos), scrollIntoView: true });
    return true;
  }
  const doc = state.doc;
  const closeLine = doc.lineAt(t.after);
  if (closeLine.number === doc.lines) {
    view.dispatch({ changes: { from: closeLine.to, insert: '\n' }, selection: EditorSelection.cursor(closeLine.to + 1), scrollIntoView: true });
  } else {
    view.dispatch({ selection: EditorSelection.cursor(doc.line(closeLine.number + 1).to), scrollIntoView: true });
  }
  return true;
}

function matrixContext(hooks: Hooks, state: EditorState): Context | null {
  const cfg = hooks.config;
  if (!cfg.settings.matrixShortcutsEnabled) return null;
  const ctx = context(state);
  if (!strictlyInMath(ctx)) return null;
  return cfg.matrixEnvs.some((name) => isWithin(ctx, { open: `\\begin{${name}}`, close: `\\end{${name}}` })) ? ctx : null;
}

function matrixTab(view: EditorView, hooks: Hooks): boolean {
  const state = view.state;
  if (!state.selection.main.empty || !matrixContext(hooks, state)) return false;
  const pos = state.selection.main.head;
  if (hooks.config.settings.taboutEnabled) {
    const line = state.doc.lineAt(pos);
    const end = matrixTabout(hooks.config, state.sliceDoc(pos, line.to));
    if (end !== null) {
      view.dispatch({ selection: EditorSelection.cursor(pos + end) });
      return true;
    }
  }
  view.dispatch(state.replaceSelection(' & '));
  return true;
}

function matrixEnter(view: EditorView, hooks: Hooks): boolean {
  const state = view.state;
  if (state.selection.ranges.length !== 1 || !state.selection.main.empty) return false;
  const ctx = matrixContext(hooks, state);
  if (!ctx) return false;
  const pos = state.selection.main.head;
  const line = state.doc.lineAt(pos);
  const before = line.text.slice(0, pos - line.from);
  // keep: a bare `\begin{…}` line or a row that already ends in `\\` gets a plain newline.
  if (before.trim() === '' || /\\begin\{[^}]*\}(\{[^}]*\}|\[[^\]]*\])*\s*$/.test(before) || /\\\\\s*$/.test(before)) return false;
  const indent = /^\s*/.exec(line.text)![0];
  const amp = (/(?<=\\begin\{[^}]*\}|\\\\|^)(\s|&)+/.exec(line.text)?.[0] ?? '').trimStart();
  if (ctx.blockMath) {
    view.dispatch({ ...expandSpec(state, pos, pos, ` \\\\\n${indent}${amp}$0`), userEvent: 'input' });
  } else {
    view.dispatch(state.replaceSelection(` \\\\ ${amp}`));
  }
  return true;
}

function matrixShiftEnter(view: EditorView, hooks: Hooks): boolean {
  const state = view.state;
  const ctx = matrixContext(hooks, state);
  if (!ctx) return false;
  const doc = state.doc;
  if (ctx.blockMath) {
    const line = doc.lineAt(ctx.pos);
    if (line.number >= doc.lines) return false;
    view.dispatch({ selection: EditorSelection.cursor(doc.line(line.number + 1).to), scrollIntoView: true });
  } else if (ctx.bounds) {
    view.dispatch({ selection: EditorSelection.cursor(ctx.bounds.outerEnd), scrollIntoView: true });
  }
  return true;
}

function moveTabstop(view: EditorView, dir: 1 | -1): boolean {
  const ts = view.state.field(tabstopField, false);
  if (!ts) return false;
  for (let i = ts.index + dir; i >= 0 && i < ts.groups.length; i += dir) {
    const sel = selectionFor(ts.groups[i]!);
    if (sel.eq(view.state.selection)) continue;
    view.dispatch({ selection: sel, effects: setTabstops.of({ groups: ts.groups, index: i }), scrollIntoView: true });
    return true;
  }
  if (dir === 1) view.dispatch({ effects: setTabstops.of(null) });
  return false;
}

function withHooks(f: (view: EditorView, hooks: Hooks) => boolean) {
  return (view: EditorView) => {
    const hooks = view.state.facet(hooksFacet);
    return hooks ? f(view, hooks) : false;
  };
}

/** Handle one typed character; true when it was consumed by a snippet, autofraction or tabout. */
export function handleTyped(view: EditorView, from: number, to: number, text: string): boolean {
  const hooks = view.state.facet(hooksFacet);
  if (!hooks || view.composing || view.state.readOnly) return false;
  if ([...text].length !== 1) return false;
  const sel = view.state.selection.main;
  if (view.state.selection.ranges.length !== 1 || from !== sel.from || to !== sel.to) return false;
  if (runSnippets(view, hooks, text)) return true;
  if (text === '/' && runAutofraction(view, hooks)) return true;
  if ((text === ')' || text === ']' || text === '}') && sel.empty && view.state.sliceDoc(sel.head, sel.head + 1) === text) {
    const ctx = context(view.state);
    if (inMath(ctx) && runTabout(view, hooks)) return true;
    // keep: in a name argument (`\label{…}` in math) the `}` came from the `{` snippet: step over it.
    if (!ctx.text && !ctx.code && !inMath(ctx)) {
      view.dispatch({ selection: EditorSelection.cursor(sel.head + 1) });
      return true;
    }
  }
  return false;
}

const inputHandler = EditorView.inputHandler.of(handleTyped);

/** Tab, Shift-Tab, Enter and Shift-Enter bindings. */
export const suiteBindings: KeyBinding[] = [
  {
    key: 'Tab',
    run: withHooks((view, hooks) => {
      if (view.state.readOnly) return false;
      return runSnippets(view, hooks, null) || moveTabstop(view, 1) || matrixTab(view, hooks) || runTabout(view, hooks);
    }),
  },
  { key: 'Shift-Tab', run: (view) => moveTabstop(view, -1) },
  { key: 'Enter', run: withHooks(matrixEnter) },
  { key: 'Shift-Enter', run: withHooks(matrixShiftEnter) },
];

export function isLatexPath(path: string): boolean {
  return /\.(tex|ltx)$/i.test(path);
}

/** The Latex Suite extension for one editor; empty for non-LaTeX files. */
export function latexSuite(path: string, config: CompiledConfig | null, breakUndo: () => void): Extension {
  if (!config || !isLatexPath(path)) return [];
  return [hooksFacet.of({ config, breakUndo }), tabstopField, Prec.highest(inputHandler), Prec.high(keymap.of(suiteBindings))];
}

