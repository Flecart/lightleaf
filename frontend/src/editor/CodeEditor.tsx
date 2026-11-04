import { useEffect, useRef } from 'preact/hooks';
import { Compartment, EditorSelection, EditorState } from '@codemirror/state';
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from '@codemirror/view';
import { defaultKeymap, indentWithTab } from '@codemirror/commands';
import { bracketMatching, defaultHighlightStyle, foldGutter, foldKeymap, indentOnInput, syntaxHighlighting } from '@codemirror/language';
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete';
import { highlightSelectionMatches, searchKeymap } from '@codemirror/search';
import { Diagnostic, lintGutter, setDiagnostics } from '@codemirror/lint';
import { oneDark } from '@codemirror/theme-one-dark';
import * as Y from 'yjs';
import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next';
import { DocBinding } from './socket';
import { languageFor, RefSource } from './lang';
import { Issue } from '../api';

export interface EditorHandle {
  view: EditorView | null;
  gotoLine(line: number): void;
  cursorLine(): number;
}

interface Props {
  binding: DocBinding;
  readOnly: boolean;
  dark: boolean;
  fontSize: number;
  refs: RefSource;
  issues: Issue[];
  onCompile: () => void;
  onCursor?: (line: number) => void;
  handle: { current: EditorHandle | null };
}

const lightTheme = EditorView.theme({
  '&': { backgroundColor: 'var(--editor-bg)', color: 'var(--text)' },
  '.cm-gutters': { backgroundColor: 'var(--gutter-bg)', color: 'var(--muted)', border: 'none' },
  '.cm-activeLine': { backgroundColor: 'var(--active-line)' },
  '.cm-activeLineGutter': { backgroundColor: 'var(--active-line)' },
});

export function CodeEditor(props: Props) {
  const host = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const theme = useRef(new Compartment());
  const font = useRef(new Compartment());
  const ro = useRef(new Compartment());
  const propsRef = useRef(props);
  propsRef.current = props;

  const fontTheme = (size: number) => EditorView.theme({ '&': { fontSize: `${size}px` }, '.cm-content': { fontFamily: 'var(--mono)' } });
  const themeExt = (dark: boolean) => (dark ? oneDark : [lightTheme, syntaxHighlighting(defaultHighlightStyle, { fallback: true })]);

  const build = () => {
    const b = propsRef.current.binding;
    viewRef.current?.destroy();
    const undo = new Y.UndoManager(b.text);
    const state = EditorState.create({
      doc: b.text.toString(),
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightSpecialChars(),
        foldGutter(),
        drawSelection(),
        dropCursor(),
        EditorState.allowMultipleSelections.of(true),
        indentOnInput(),
        bracketMatching(),
        closeBrackets(),
        autocompletion({ activateOnTyping: true }),
        rectangularSelection(),
        crosshairCursor(),
        highlightActiveLine(),
        highlightSelectionMatches(),
        lintGutter(),
        EditorView.lineWrapping,
        EditorView.contentAttributes.of({ spellcheck: 'true', autocorrect: 'off', autocapitalize: 'off' }),
        keymap.of([
          { key: 'Mod-s', run: () => (propsRef.current.onCompile(), true), preventDefault: true },
          { key: 'Mod-Enter', run: () => (propsRef.current.onCompile(), true) },
          ...closeBracketsKeymap,
          ...yUndoManagerKeymap,
          ...defaultKeymap,
          ...searchKeymap,
          ...foldKeymap,
          ...completionKeymap,
          indentWithTab,
        ]),
        ...languageFor(b.path, propsRef.current.refs),
        yCollab(b.text, b.awareness, { undoManager: undo }),
        theme.current.of(themeExt(propsRef.current.dark)),
        font.current.of(fontTheme(propsRef.current.fontSize)),
        ro.current.of(EditorState.readOnly.of(propsRef.current.readOnly)),
        EditorView.updateListener.of((u) => {
          if (u.selectionSet || u.docChanged) {
            const line = u.state.doc.lineAt(u.state.selection.main.head).number;
            propsRef.current.onCursor?.(line);
          }
        }),
      ],
    });
    viewRef.current = new EditorView({ state, parent: host.current! });
    applyDiagnostics();
  };

  const applyDiagnostics = () => {
    const view = viewRef.current;
    if (!view) return;
    const p = propsRef.current;
    const diags: Diagnostic[] = [];
    for (const is of p.issues) {
      if (!is.line || (is.file && is.file !== p.binding.path)) continue;
      if (!is.file && is.level === 'typesetting') continue;
      if (is.line > view.state.doc.lines) continue;
      const line = view.state.doc.line(is.line);
      diags.push({
        from: line.from,
        to: line.to,
        severity: is.level === 'error' ? 'error' : is.level === 'warning' ? 'warning' : 'info',
        message: is.message,
      });
    }
    view.dispatch(setDiagnostics(view.state, diags));
  };

  useEffect(() => {
    build();
    props.binding.onReset = build;
    props.handle.current = {
      get view() {
        return viewRef.current;
      },
      gotoLine(line: number) {
        const v = viewRef.current;
        if (!v) return;
        const l = v.state.doc.line(Math.max(1, Math.min(line, v.state.doc.lines)));
        v.dispatch({ selection: EditorSelection.cursor(l.from), effects: EditorView.scrollIntoView(l.from, { y: 'center' }) });
        v.focus();
      },
      cursorLine() {
        const v = viewRef.current;
        return v ? v.state.doc.lineAt(v.state.selection.main.head).number : 1;
      },
    };
    return () => {
      props.binding.onReset = null;
      viewRef.current?.destroy();
      viewRef.current = null;
    };
  }, [props.binding]);

  useEffect(() => {
    viewRef.current?.dispatch({ effects: theme.current.reconfigure(themeExt(props.dark)) });
  }, [props.dark]);
  useEffect(() => {
    viewRef.current?.dispatch({ effects: font.current.reconfigure(fontTheme(props.fontSize)) });
  }, [props.fontSize]);
  useEffect(() => {
    viewRef.current?.dispatch({ effects: ro.current.reconfigure(EditorState.readOnly.of(props.readOnly)) });
  }, [props.readOnly]);
  useEffect(applyDiagnostics, [props.issues]);

  return <div class="code-editor" ref={host} />;
}
