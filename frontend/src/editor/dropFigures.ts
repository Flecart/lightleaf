// Drop or paste an image or PDF onto a LaTeX/Typst file: it is uploaded into the project and a
// figure that includes it is inserted where it was dropped. Dragging an existing image from the
// file tree inserts a reference to it without uploading anything.

import { Extension } from '@codemirror/state';
import { EditorView } from '@codemirror/view';
import * as Y from 'yjs';

export interface FigureHost {
  /** Uploads the files under the given project paths; returns the paths saved, or null on failure. */
  upload(files: { file: File; path: string }[]): Promise<string[] | null>;
  /** Project paths of every file and folder, to pick a folder and avoid overwriting. */
  entries(): { path: string; kind: 'file' | 'dir' }[];
  /** The main document: LaTeX resolves graphics relative to its folder. */
  mainFile(): string;
}

/** pdflatex, xelatex and lualatex include these without extra packages. */
const LATEX_FIGURE = /\.(png|jpe?g|pdf|eps)$/i;
/** Typst 0.13 (the version in the Docker image) cannot include PDFs. */
const TYPST_FIGURE = /\.(png|jpe?g|gif|svg|webp)$/i;
/** Conventional figure folders, preferred in this order when the project has one. */
const FIGURE_DIRS = ['figures', 'figs', 'images', 'img', 'fig', 'graphics'];

const dirOf = (p: string) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '');
const join = (dir: string, name: string) => (dir ? `${dir}/${name}` : name);

/** `to` (a project path) as seen from the project folder `fromDir`. */
export function relativePath(fromDir: string, to: string): string {
  const from = fromDir ? fromDir.split('/') : [];
  const parts = to.split('/');
  let i = 0;
  while (i < from.length && i < parts.length - 1 && from[i] === parts[i]) i++;
  return [...from.slice(i).map(() => '..'), ...parts.slice(i)].join('/');
}

/**
 * A file name TeX accepts in `\includegraphics`: spaces, `%`, `#`, `~` and friends break it, so
 * anything outside `[A-Za-z0-9._-]` becomes `-`. Only the last dot is kept, as graphicx reads
 * everything after the first one as the extension.
 */
export function safeName(name: string): string {
  const dot = name.lastIndexOf('.');
  const ext = dot > 0 ? name.slice(dot + 1).toLowerCase() : '';
  const stem = (dot > 0 ? name.slice(0, dot) : name)
    .normalize('NFKD')
    .replace(/[^A-Za-z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '') || 'figure';
  return ext ? `${stem}.${ext}` : stem;
}

/**
 * Where an uploaded figure goes: a conventional figure folder next to the open file or at the
 * project root, else the open file's folder. Existing names get a `-2`, `-3`, … suffix so a drop
 * never overwrites a file.
 */
export function figurePath(docPath: string, name: string, entries: { path: string; kind: string }[], taken: Set<string>): string {
  const dirs = new Set(entries.filter((e) => e.kind === 'dir').map((e) => e.path));
  const here = dirOf(docPath);
  const candidates = [...FIGURE_DIRS.map((d) => join(here, d)), ...FIGURE_DIRS];
  const dir = candidates.find((d) => dirs.has(d)) ?? here;
  const safe = safeName(name);
  const dot = safe.lastIndexOf('.');
  const [stem, ext] = dot > 0 ? [safe.slice(0, dot), safe.slice(dot)] : [safe, ''];
  for (let n = 1; ; n++) {
    const p = join(dir, n === 1 ? safe : `${stem}-${n}${ext}`);
    if (!taken.has(p)) return p;
  }
}

/** A figure label derived from the file name: `fig:my-plot`. */
const labelFor = (path: string) => `fig:${path.split('/').pop()!.replace(/\.[^.]*$/, '')}`;

/**
 * The text to insert for `paths` (project paths) into `docPath`. On an empty line each becomes a
 * full figure with an empty caption; inside text, a bare include. Returns the text and the offset
 * of the first caption (where the cursor goes), or of the end.
 */
export function figureText(docPath: string, mainFile: string, paths: string[], block: boolean, indent: string): { text: string; cursor: number } {
  const typst = docPath.endsWith('.typ');
  // keep: LaTeX resolves graphics against the main file's folder (the compile cwd); Typst
  // resolves `/…` against the project root, which `typst compile --root .` makes the project.
  const ref = (p: string) => (typst ? `/${p}` : relativePath(dirOf(mainFile), p));
  if (!block) {
    const text = paths
      .map((p) => (typst ? `#image("${ref(p)}", width: 80%)` : `\\includegraphics[width=0.8\\linewidth]{${ref(p)}}`))
      .join(' ');
    return { text, cursor: text.length };
  }
  let cursor = -1;
  const parts = paths.map((p) => {
    const lines = typst
      ? ['#figure(', `  image("${ref(p)}", width: 80%),`, '  caption: [\u0000],', `) <${labelFor(p)}>`]
      : [
          '\\begin{figure}[htbp]',
          '  \\centering',
          `  \\includegraphics[width=0.8\\linewidth]{${ref(p)}}`,
          '  \\caption{\u0000}',
          `  \\label{${labelFor(p)}}`,
          '\\end{figure}',
        ];
    return lines.map((l, i) => (i === 0 ? l : indent + l)).join('\n');
  });
  let text = parts.join(`\n${indent}`);
  for (;;) {
    const i = text.indexOf('\u0000');
    if (i < 0) break;
    if (cursor < 0) cursor = i;
    text = text.slice(0, i) + text.slice(i + 1);
  }
  return { text, cursor: cursor < 0 ? text.length : cursor };
}

/** A timestamped name for a pasted screenshot, which browsers all call `image.png`. */
function pastedName(file: File): string {
  const ext = file.type === 'image/jpeg' ? 'jpg' : file.type.split('/')[1] ?? 'png';
  const d = new Date();
  const pad = (n: number) => String(n).padStart(2, '0');
  return `pasted-${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}.${ext}`;
}

export function dropFigures(docPath: string, text: Y.Text, host: FigureHost): Extension {
  const typst = docPath.endsWith('.typ');
  if (!typst && !docPath.endsWith('.tex')) return [];
  const accepts = (name: string) => (typst ? TYPST_FIGURE : LATEX_FIGURE).test(name);

  /** Inserts the figures for `paths` at `pos`: a figure block on an empty line, else inline. */
  const insert = (view: EditorView, paths: string[], pos: number) => {
    const line = view.state.doc.lineAt(pos);
    const block = line.text.trim() === '';
    const indent = /^\s*/.exec(line.text)![0];
    const from = block ? line.from : pos;
    const to = block ? line.to : pos;
    const { text: t, cursor } = figureText(docPath, host.mainFile(), paths, block, indent);
    const insertText = block ? indent + t : t;
    view.dispatch({
      changes: { from, to, insert: insertText },
      selection: { anchor: from + (block ? indent.length : 0) + cursor },
      scrollIntoView: true,
      userEvent: 'input.drop',
    });
    view.focus();
  };

  const uploadAt = (view: EditorView, files: File[], pos: number) => {
    // keep: collaborators keep typing during the upload; a Yjs relative position follows their
    // edits so the figure lands where it was dropped, not at a stale offset.
    const anchor = Y.createRelativePositionFromTypeIndex(text, pos);
    const entries = host.entries();
    const taken = new Set(entries.map((e) => e.path));
    const jobs = files.map((file) => {
      const path = figurePath(docPath, file.name, entries, taken);
      taken.add(path);
      return { file, path };
    });
    host.upload(jobs).then((saved) => {
      if (!saved?.length || view.state.readOnly) return;
      const abs = text.doc && Y.createAbsolutePositionFromRelativePosition(anchor, text.doc);
      const at = Math.min(abs && abs.type === text ? abs.index : pos, view.state.doc.length);
      insert(view, saved, at);
    });
  };

  return EditorView.domEventHandlers({
    drop(e, view) {
      const dt = e.dataTransfer;
      if (!dt || view.state.readOnly) return false;
      const pos = view.posAtCoords({ x: e.clientX, y: e.clientY }) ?? view.state.selection.main.head;
      const treePath = dt.getData('text/x-lightleaf-path');
      if (treePath) {
        if (!accepts(treePath)) return false;
        e.preventDefault();
        insert(view, [treePath], pos);
        return true;
      }
      const files = Array.from(dt.files);
      if (!files.length || !files.every((f) => accepts(f.name))) return false;
      e.preventDefault();
      uploadAt(view, files, pos);
      return true;
    },
    paste(e, view) {
      const files = Array.from(e.clipboardData?.files ?? []);
      if (!files.length || view.state.readOnly) return false;
      const named = files.map((f) => (f.name && f.name !== 'image.png' ? f : new File([f], pastedName(f), { type: f.type })));
      if (!named.every((f) => accepts(f.name))) return false;
      e.preventDefault();
      uploadAt(view, named, view.state.selection.main.head);
      return true;
    },
  });
}
