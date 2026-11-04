import { StreamLanguage, StringStream } from '@codemirror/language';
import { stex } from '@codemirror/legacy-modes/mode/stex';
import { Completion, CompletionContext, CompletionResult, snippetCompletion } from '@codemirror/autocomplete';
import { Extension } from '@codemirror/state';

// ---------------------------------------------------------------------------------------------
// Typst — a compact stream tokenizer (markup, code after '#', math between '$').
// ---------------------------------------------------------------------------------------------
interface TypstState {
  math: boolean;
  blockComment: number;
  rawFence: string | null;
}

const typstKeywords = new Set(['let', 'set', 'show', 'if', 'else', 'for', 'in', 'while', 'return', 'import', 'include', 'context', 'and', 'or', 'not', 'none', 'auto', 'true', 'false', 'break', 'continue', 'as']);

const typst = StreamLanguage.define<TypstState>({
  name: 'typst',
  startState: () => ({ math: false, blockComment: 0, rawFence: null }),
  token(stream: StringStream, state: TypstState) {
    if (state.blockComment > 0) {
      while (!stream.eol()) {
        if (stream.match('*/')) {
          state.blockComment--;
          if (!state.blockComment) break;
        } else if (stream.match('/*')) state.blockComment++;
        else stream.next();
      }
      return 'comment';
    }
    if (state.rawFence) {
      if (stream.skipTo(state.rawFence)) {
        stream.match(state.rawFence);
        state.rawFence = null;
      } else stream.skipToEnd();
      return 'string';
    }
    if (stream.match('//')) {
      stream.skipToEnd();
      return 'comment';
    }
    if (stream.match('/*')) {
      state.blockComment = 1;
      return 'comment';
    }
    if (stream.match('```')) {
      state.rawFence = '```';
      return 'string';
    }
    if (stream.match(/^`[^`]*`/)) return 'string';
    if (stream.peek() === '$') {
      stream.next();
      state.math = !state.math;
      return 'keyword';
    }
    if (state.math) {
      if (stream.match(/^"[^"]*"/)) return 'string';
      if (stream.match(/^[a-zA-Z][a-zA-Z.]+/)) return 'variableName.special';
      if (stream.match(/^[0-9.]+/)) return 'number';
      stream.next();
      return 'operator';
    }
    if (stream.sol() && stream.match(/^\s*=+\s.*/)) return 'heading';
    if (stream.match(/^#[a-zA-Z_][\w-]*/)) {
      const w = stream.current().slice(1);
      return typstKeywords.has(w) ? 'keyword' : 'function';
    }
    if (stream.match(/^"(?:[^"\\]|\\.)*"/)) return 'string';
    if (stream.match(/^<[\w:.-]+>/)) return 'labelName';
    if (stream.match(/^@[\w:.-]+/)) return 'link';
    if (stream.match(/^\*[^*\n]+\*/)) return 'strong';
    if (stream.match(/^_[^_\n]+_/)) return 'emphasis';
    if (stream.sol() && stream.match(/^\s*[-+]\s/)) return 'list';
    if (stream.match(/^\\./)) return 'escape';
    stream.next();
    return null;
  },
  languageData: { commentTokens: { line: '//', block: { open: '/*', close: '*/' } } },
});

const latex = StreamLanguage.define(stex);

// ---------------------------------------------------------------------------------------------
// LaTeX completions
// ---------------------------------------------------------------------------------------------
const COMMANDS = [
  'section', 'subsection', 'subsubsection', 'paragraph', 'chapter', 'part', 'textbf', 'textit', 'emph', 'underline',
  'texttt', 'textsc', 'cite', 'citep', 'citet', 'ref', 'eqref', 'autoref', 'cref', 'label', 'footnote', 'url', 'href',
  'includegraphics', 'caption', 'centering', 'item', 'usepackage', 'documentclass', 'input', 'include', 'newcommand',
  'renewcommand', 'title', 'author', 'date', 'maketitle', 'tableofcontents', 'bibliography', 'bibliographystyle',
  'printbibliography', 'addbibresource', 'frac', 'sqrt', 'sum', 'int', 'prod', 'lim', 'infty', 'alpha', 'beta',
  'gamma', 'delta', 'epsilon', 'lambda', 'mu', 'pi', 'sigma', 'theta', 'omega', 'mathbb', 'mathcal', 'mathbf',
  'mathrm', 'left', 'right', 'quad', 'qquad', 'hline', 'toprule', 'midrule', 'bottomrule', 'newpage', 'clearpage',
  'vspace', 'hspace', 'noindent', 'small', 'large', 'Large', 'textwidth', 'linewidth', 'today', 'appendix', 'cdot',
  'ldots', 'leq', 'geq', 'neq', 'approx', 'times', 'partial', 'nabla', 'forall', 'exists', 'in', 'subset', 'cup', 'cap',
];
const ENVS = ['figure', 'table', 'equation', 'equation*', 'align', 'align*', 'itemize', 'enumerate', 'description', 'tabular', 'abstract', 'center', 'theorem', 'lemma', 'proof', 'verbatim', 'minipage', 'quote', 'gather', 'cases', 'matrix', 'pmatrix', 'bmatrix', 'frame', 'thebibliography', 'document'];

const envSnippets: Completion[] = ENVS.map((e) =>
  snippetCompletion(`\\begin{${e}}\n\t\${}\n\\end{${e}}`, { label: `\\begin{${e}}`, type: 'keyword', boost: 1 }),
);
const cmdCompletions: Completion[] = COMMANDS.map((c) => ({ label: `\\${c}`, type: 'function' }));

export interface RefSource {
  labels(): string[];
  citations(): string[];
}

function latexCompletions(refs: RefSource) {
  return (ctx: CompletionContext): CompletionResult | null => {
    const arg = ctx.matchBefore(/\\(\w*(?:ref|cite\w*|cref|autoref))\{[^}]*$/);
    if (arg) {
      const isCite = /cite/.test(arg.text);
      const start = arg.from + arg.text.lastIndexOf('{') + 1;
      const pieceStart = Math.max(start, arg.from + arg.text.lastIndexOf(',') + 1);
      const opts = (isCite ? refs.citations() : refs.labels()).map((l) => ({ label: l, type: isCite ? 'text' : 'constant' }));
      return { from: pieceStart, options: opts, validFor: /^[^},]*$/ };
    }
    const env = ctx.matchBefore(/\\(begin|end)\{\w*$/);
    if (env) {
      const from = env.from + env.text.indexOf('{') + 1;
      return { from, options: ENVS.map((e) => ({ label: e, type: 'keyword' })) };
    }
    const word = ctx.matchBefore(/\\[a-zA-Z]*/);
    if (!word || (word.from === word.to && !ctx.explicit)) return null;
    return { from: word.from, options: [...envSnippets, ...cmdCompletions], validFor: /^\\[a-zA-Z]*$/ };
  };
}

export function languageFor(path: string, refs: RefSource): Extension[] {
  const ext = path.split('.').pop()?.toLowerCase() ?? '';
  if (ext === 'typ') return [typst];
  if (['tex', 'sty', 'cls', 'ltx', 'bib', 'bbx', 'cbx', 'dtx', 'tikz'].includes(ext)) {
    return [latex, latex.data.of({ autocomplete: latexCompletions(refs) })];
  }
  return [];
}

// ---------------------------------------------------------------------------------------------
// Document outline
// ---------------------------------------------------------------------------------------------
export interface OutlineItem {
  level: number;
  title: string;
  line: number;
}

const LATEX_LEVELS: Record<string, number> = { part: 0, chapter: 1, section: 2, subsection: 3, subsubsection: 4, paragraph: 5 };

export function outline(path: string, text: string): OutlineItem[] {
  const out: OutlineItem[] = [];
  const lines = text.split('\n');
  if (path.endsWith('.typ')) {
    lines.forEach((l, i) => {
      const m = l.match(/^(=+)\s+(.*)$/);
      if (m) out.push({ level: m[1]!.length, title: m[2]!.trim(), line: i + 1 });
    });
  } else if (path.endsWith('.tex')) {
    lines.forEach((l, i) => {
      const m = l.match(/^[^%]*?\\(part|chapter|section|subsection|subsubsection|paragraph)\*?\s*(?:\[[^\]]*\])?\{(.*)\}/);
      if (m) out.push({ level: LATEX_LEVELS[m[1]!]!, title: m[2]!.replace(/\}.*$/, ''), line: i + 1 });
    });
  }
  return out;
}
