/**
 * Where the cursor is in a LaTeX document: plain text, inline or display math, text inside math
 * (`\text{…}`), verbatim code, or a command argument where no snippet should fire.
 *
 * The scan starts at the closest blank line above the cursor: math cannot contain a paragraph
 * break in LaTeX, so no math region can begin before it. It runs past the cursor until the
 * enclosing math region closes, which gives the bounds that tabout and autofraction use.
 */
import { EditorState } from '@codemirror/state';
import { Env } from './config';

/** Environments that are display math on their own. */
const DISPLAY_ENVS = new Set(
  [
    'equation',
    'align',
    'gather',
    'multline',
    'flalign',
    'alignat',
    'eqnarray',
    'displaymath',
    'dmath',
    'dgroup',
    'darray',
    'IEEEeqnarray',
  ].flatMap((e) => [e, e + '*']),
);
const INLINE_ENVS = new Set(['math']);
const VERBATIM_ENVS = new Set(['verbatim', 'verbatim*', 'Verbatim', 'lstlisting', 'minted', 'comment', 'filecontents', 'filecontents*']);

/** Commands whose argument is text even inside math (the same list as Latex Suite). */
const TEXT_COMMANDS = new Set([
  'text', 'textrm', 'textup', 'textit', 'textbf', 'textsf', 'texttt', 'textnormal', 'clap', 'textllap',
  'textrlap', 'textclap', 'hbox', 'mbox', 'fbox', 'framebox', 'colorbox', 'fcolorbox', 'intertext', 'shortintertext',
]);
/** Commands whose argument is a name or key: no snippets at all in there. */
const NAME_COMMANDS = new Set([
  'begin', 'end', 'tag', 'label', 'ref', 'eqref', 'cref', 'Cref', 'autoref', 'pageref', 'nameref', 'cite', 'citep',
  'citet', 'parencite', 'textcite', 'autocite', 'nocite', 'usepackage', 'RequirePackage', 'documentclass', 'input',
  'include', 'includegraphics', 'bibliography', 'bibliographystyle', 'addbibresource', 'url', 'href', 'newcommand',
  'renewcommand', 'providecommand', 'DeclareMathOperator', 'newenvironment', 'renewenvironment', 'newtheorem',
  'mmlToken', 'unicode', 'hyperref',
]);

/** How far the scan may look above and below the cursor at most. */
const MAX_LINES = 400;

export interface MathBounds {
  outerStart: number;
  innerStart: number;
  innerEnd: number;
  outerEnd: number;
}

export interface Brace {
  pos: number;
  /** Up to 16 characters before the brace, plus the brace itself: `…\pu{`, `…^{`. */
  head: string;
  kind: 'text' | 'name' | 'plain';
}

export interface Context {
  pos: number;
  text: boolean;
  inlineMath: boolean;
  blockMath: boolean;
  /** Inside a text command within math. */
  textEnv: boolean;
  code: boolean;
  bounds: MathBounds | null;
  /** Braces open at the cursor that were opened inside the current math region. */
  braces: Brace[];
  /** Environments open at the cursor, innermost last. */
  envs: string[];
}

export function inMath(c: Context): boolean {
  return c.inlineMath || c.blockMath;
}

export function strictlyInMath(c: Context): boolean {
  return inMath(c) && !c.textEnv;
}

/** Whether the cursor is inside `open … close`: a brace group such as `^{…}`, or `\begin{env}`. */
export function isWithin(c: Context, env: Env): boolean {
  const m = /^\\begin\{(.+)\}$/.exec(env.open);
  if (m) return c.envs.includes(m[1]!);
  if (env.open.endsWith('{') && env.close === '}') return c.braces.some((b) => b.head.endsWith(env.open));
  return false;
}

interface Frame {
  block: boolean;
  /** `$`, `$$`, `\)`, `\]` or `env:<name>`. */
  closer: string;
  outerStart: number;
  innerStart: number;
  braceBase: number;
}

function scanStart(state: EditorState, pos: number): number {
  const doc = state.doc;
  let line = doc.lineAt(pos);
  for (let n = 0; n < MAX_LINES && line.number > 1; n++) {
    const prev = doc.line(line.number - 1);
    if (prev.text.trim() === '') return prev.to;
    line = prev;
  }
  return line.from;
}

function scanEnd(state: EditorState, pos: number): number {
  const doc = state.doc;
  let line = doc.lineAt(pos);
  for (let n = 0; n < MAX_LINES && line.number < doc.lines; n++) {
    const next = doc.line(line.number + 1);
    if (next.text.trim() === '') return next.from;
    line = next;
  }
  return line.to;
}

export function latexContext(state: EditorState, pos: number): Context {
  const start = scanStart(state, pos);
  const end = scanEnd(state, pos);
  const s = state.sliceDoc(start, end);
  const rel = pos - start;

  const frames: Frame[] = [];
  const braces: Brace[] = [];
  const envs: string[] = [];
  let verbatim: string | null = null;
  let lastCmd: string | null = null;
  let snap: { frames: Frame[]; braces: Brace[]; envs: string[]; code: boolean; comment: boolean } | null = null;
  let bounds: MathBounds | null = null;

  const top = () => frames[frames.length - 1];
  const canOpenMath = () => {
    const f = top();
    if (!f) return true;
    const inner = braces.slice(f.braceBase);
    return inner.length > 0 && inner[inner.length - 1]!.kind === 'text';
  };
  const open = (block: boolean, closer: string, outerStart: number, innerStart: number) =>
    frames.push({ block, closer, outerStart, innerStart, braceBase: braces.length });
  const close = (innerEnd: number, outerEnd: number) => {
    const f = frames.pop()!;
    braces.length = Math.min(braces.length, f.braceBase);
    if (snap && !bounds && snap.frames.length > 0 && snap.frames[snap.frames.length - 1] === f) {
      bounds = { outerStart: f.outerStart + start, innerStart: f.innerStart + start, innerEnd: innerEnd + start, outerEnd: outerEnd + start };
    }
  };

  let i = 0;
  while (i <= s.length) {
    if (i >= rel && !snap) {
      snap = { frames: [...frames], braces: [...braces], envs: [...envs], code: verbatim !== null, comment: false };
      if (frames.length === 0) break;
    }
    if (snap && (bounds || frames.length < snap.frames.length)) break;
    if (i === s.length) break;
    const c = s[i]!;

    if (verbatim !== null) {
      const endTag = `\\end{${verbatim}}`;
      if (s.startsWith(endTag, i)) {
        verbatim = null;
        envs.pop();
        i += endTag.length;
      } else i++;
      continue;
    }

    if (c === '%') {
      const nl = s.indexOf('\n', i);
      const stop = nl < 0 ? s.length : nl;
      if (!snap && rel > i && rel <= stop) {
        snap = { frames: [], braces: [], envs: [...envs], code: false, comment: true };
        break;
      }
      i = stop;
      continue;
    }

    if (c === '\\') {
      const m = /^[A-Za-z]+\*?/.exec(s.slice(i + 1, i + 40));
      const name = m ? m[0] : (s[i + 1] ?? '');
      const after = i + 1 + name.length;
      if (name === '(' && frames.length === 0) open(false, '\\)', i, after);
      else if (name === '[' && frames.length === 0) open(true, '\\]', i, after);
      else if ((name === ')' || name === ']') && top()?.closer === '\\' + name) close(i, after);
      else if (name === 'begin' || name === 'end') {
        const arg = /^\s*\{([^{}\n]*)\}/.exec(s.slice(after, after + 80));
        if (arg && !(rel > after && rel < after + arg[0].length)) {
          const env = arg[1]!.trim();
          const argEnd = after + arg[0].length;
          if (name === 'begin') {
            envs.push(env);
            if (VERBATIM_ENVS.has(env)) verbatim = env;
            else if (DISPLAY_ENVS.has(env) && frames.length === 0) open(true, 'env:' + env, i, argEnd);
            else if (INLINE_ENVS.has(env) && frames.length === 0) open(false, 'env:' + env, i, argEnd);
          } else {
            if (top()?.closer === 'env:' + env) close(i, argEnd);
            const at = envs.lastIndexOf(env);
            if (at >= 0) envs.length = at;
          }
          i = argEnd;
          lastCmd = null;
          continue;
        }
      }
      lastCmd = m ? name.replace(/\*$/, '') : null;
      i = after;
      continue;
    }

    if (c === '$') {
      // keep: `$|$` with the cursor in between is empty inline math (what `mk` leaves), not `$$`.
      const dbl = s[i + 1] === '$' && rel !== i + 1;
      const f = top();
      if (f && canOpenMath()) open(dbl, dbl ? '$$' : '$', i, i + (dbl ? 2 : 1));
      else if (dbl && f?.closer === '$$') close(i, i + 2);
      else if (f?.closer === '$') {
        close(i, i + 1);
        i += 1;
        continue;
      } else if (canOpenMath()) {
        if (dbl) open(true, '$$', i, i + 2);
        else open(false, '$', i, i + 1);
      }
      i += dbl ? 2 : 1;
      lastCmd = null;
      continue;
    }

    if (c === '{') {
      const kind = lastCmd && TEXT_COMMANDS.has(lastCmd) ? 'text' : lastCmd && NAME_COMMANDS.has(lastCmd) ? 'name' : 'plain';
      braces.push({ pos: i + start, head: s.slice(Math.max(0, i - 16), i + 1), kind });
      lastCmd = kind === 'plain' ? null : lastCmd;
    } else if (c === '}') {
      const f = top();
      if (braces.length > (f?.braceBase ?? 0)) braces.pop();
    } else if (c !== ' ' && c !== '*' && c !== '[' && c !== ']') lastCmd = null;
    i++;
  }

  const at = snap ?? { frames, braces, envs, code: verbatim !== null, comment: false };
  const f = at.frames[at.frames.length - 1];
  const ctx: Context = {
    pos,
    text: false,
    inlineMath: false,
    blockMath: false,
    textEnv: false,
    code: false,
    bounds: null,
    braces: [],
    envs: at.envs,
  };
  if (at.code) {
    ctx.code = true;
    return ctx;
  }
  const innermost = at.braces[at.braces.length - 1];
  if (innermost?.kind === 'name') return ctx;
  if (!f) {
    ctx.text = true;
    return ctx;
  }
  ctx.braces = at.braces.slice(f.braceBase);
  ctx.inlineMath = !f.block;
  ctx.blockMath = f.block;
  ctx.textEnv = ctx.braces.some((b) => b.kind === 'text');
  ctx.bounds = bounds ?? { outerStart: f.outerStart + start, innerStart: f.innerStart + start, innerEnd: end, outerEnd: end };
  return ctx;
}
