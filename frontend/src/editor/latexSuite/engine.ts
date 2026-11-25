/**
 * The editor-independent part of the Latex Suite port: matching snippets against the text before
 * the cursor, turning a replacement into text plus tabstops, autofraction, tabout targets and
 * bracket enlarging. Positions are plain document offsets; `extension.ts` applies the results.
 */
import { CompiledConfig, Snippet } from './config';
import { Context, inMath, isWithin } from './context';

/** How much text before the cursor the triggers see. */
export const LOOKBEHIND = 300;

export interface Expansion {
  from: number;
  to: number;
  /** Replacement source, still containing `$0`, `${1:default}` tabstops. */
  replacement: string;
  snippet: Snippet;
}

export interface Range {
  from: number;
  to: number;
}

function modeMatches(s: Snippet, ctx: Context): boolean {
  const o = s.options;
  if ((o.inlineMath && ctx.inlineMath) || (o.blockMath && ctx.blockMath)) {
    if (!ctx.textEnv) return true;
  }
  if (inMath(ctx) && ctx.textEnv && o.text) return true;
  if (o.text && ctx.text) return true;
  if (o.code && ctx.code) return true;
  return false;
}

/** Latex Suite's "remove trailing whitespace in inline math": `x ` → `x`, `x $1` → `x$1`. */
function trimInline(rep: string): string {
  if (rep.endsWith(' ')) return rep.trimEnd();
  const tail = rep.slice(-3);
  if (tail.startsWith(' $') && /\d/.test(tail[2] ?? '')) return rep.slice(0, -3) + rep.slice(-2);
  return rep;
}

export interface MatchInput {
  /** Text from `windowStart` up to the end of the selection. */
  before: string;
  windowStart: number;
  selection: Range;
  selected: string;
  /** The character being typed (automatic snippets), or '' for the trigger key. */
  key: string;
  charBefore: (pos: number) => string;
  charAfter: (pos: number) => string;
  ctx: Context;
  automatic: boolean;
}

/** The first snippet (in priority order) that fires, if any. */
export function findExpansion(cfg: CompiledConfig, m: MatchInput): Expansion | null {
  const effective = m.before + m.key;
  const hasSel = m.selection.from !== m.selection.to;
  for (const s of cfg.snippets) {
    if (s.options.automatic !== m.automatic) continue;
    if (!modeMatches(s, m.ctx)) continue;
    let triggerPos: number;
    let rep: string;
    if (s.kind === 'visual') {
      if (!hasSel || !effective.endsWith(s.trigger as string)) continue;
      triggerPos = m.selection.from;
      rep = s.replacement.split('${VISUAL}').join(m.selected);
    } else if (hasSel) {
      continue;
    } else if (s.kind === 'regex') {
      const r = (s.trigger as RegExp).exec(effective);
      if (!r) continue;
      triggerPos = m.windowStart + r.index;
      rep = s.replacement;
      for (let g = 1; g < r.length; g++) rep = rep.split(`[[${g - 1}]]`).join(r[g] ?? '');
    } else {
      const t = s.trigger as string;
      if (!effective.endsWith(t)) continue;
      triggerPos = m.windowStart + effective.length - t.length;
      rep = s.replacement;
    }
    if (s.excluded.some((e) => isWithin(m.ctx, e))) continue;
    if (s.options.wordBoundary) {
      const d = cfg.wordDelimiters;
      const before = m.charBefore(triggerPos);
      const after = m.charAfter(m.selection.to);
      if ((before && !d.includes(before)) || (after && !d.includes(after))) continue;
    }
    if (m.ctx.inlineMath && cfg.settings.removeSnippetWhitespace) rep = trimInline(rep);
    return { from: Math.min(triggerPos, m.selection.to), to: m.selection.to, replacement: rep, snippet: s };
  }
  return null;
}

export interface ParsedReplacement {
  text: string;
  /** Tabstop groups in visiting order; ranges are offsets into `text`. */
  groups: Range[][];
}

/** Split `a$0b${1:def}c` into text and numbered tabstops (`$0` first; equal numbers are mirrored). */
export function parseReplacement(rep: string): ParsedReplacement {
  let text = '';
  const byNum = new Map<number, Range[]>();
  const add = (n: number, r: Range) => {
    const g = byNum.get(n) ?? [];
    g.push(r);
    byNum.set(n, g);
  };
  let i = 0;
  while (i < rep.length) {
    const c = rep[i]!;
    if (c === '$') {
      const plain = /^\$(\d+)/.exec(rep.slice(i));
      if (plain) {
        add(Number(plain[1]), { from: text.length, to: text.length });
        i += plain[0].length;
        continue;
      }
      const braced = /^\$\{(\d+)(:|\})/.exec(rep.slice(i));
      if (braced) {
        const n = Number(braced[1]);
        i += braced[0].length;
        if (braced[2] === '}') {
          add(n, { from: text.length, to: text.length });
          continue;
        }
        let depth = 1;
        let def = '';
        while (i < rep.length) {
          const d = rep[i]!;
          if (d === '{') depth++;
          else if (d === '}' && --depth === 0) break;
          def += d;
          i++;
        }
        i++;
        add(n, { from: text.length, to: text.length + def.length });
        text += def;
        continue;
      }
    }
    text += c;
    i++;
  }
  const groups = [...byNum.keys()].sort((a, b) => a - b).map((n) => byNum.get(n)!);
  return { text, groups };
}

export interface Token {
  start: number;
  end: number;
  text: string;
}

/** Latex Suite's math tokenizer: commands, comments and single characters; whitespace skipped. */
export function tokenize(s: string): Token[] {
  const out: Token[] = [];
  let i = 0;
  while (i < s.length) {
    const c = s[i]!;
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    let end = i + 1;
    if (c === '%') {
      const nl = s.indexOf('\n', i);
      end = nl < 0 ? s.length : nl;
    } else if (c === '\\') {
      end = i + 2;
      if (/[A-Za-z]/.test(s[i + 1] ?? '')) while (end < s.length && /[A-Za-z]/.test(s[end]!)) end++;
    }
    out.push({ start: i, end, text: s.slice(i, end) });
    i = end;
  }
  return out;
}

export type TaboutTarget = { kind: 'cursor'; pos: number } | { kind: 'leaveBlock'; after: number };

/** Where Tab should move the cursor inside math: past the next closing symbol, or out of the math. */
export function taboutTarget(cfg: CompiledConfig, ctx: Context, doc: string, docOffset: number, cursor: number): TaboutTarget | null {
  const b = ctx.bounds;
  if (!inMath(ctx) || !b || b.outerEnd <= cursor) return null;
  const inner = doc.slice(b.innerStart - docOffset, b.innerEnd - docOffset);
  const rel = cursor - b.innerStart;
  const tokens = tokenize(inner);
  const closing = new Set(cfg.taboutClosing);
  for (const t of tokens) {
    if (t.end <= rel) continue;
    if (closing.has(t.text)) return { kind: 'cursor', pos: b.innerStart + t.end };
  }
  if (inner.slice(rel).trim() !== '') return null;
  return ctx.inlineMath ? { kind: 'cursor', pos: b.outerEnd } : { kind: 'leaveBlock', after: b.outerEnd };
}

/** Inside a matrix row: the position just past an unmatched closing symbol later on the line. */
export function matrixTabout(cfg: CompiledConfig, restOfLine: string): number | null {
  const pairs: Record<string, string> = {
    '(': ')', '[': ']', '{': '}', '\\lbrack': '\\rbrack', '\\lbrace': '\\rbrace', '\\{': '\\}',
    '\\langle': '\\rangle', '\\lvert': '\\rvert', '\\lVert': '\\rVert', '\\lfloor': '\\rfloor', '\\lceil': '\\rceil',
    '\\ulcorner': '\\urcorner',
  };
  const closing = new Set(cfg.taboutClosing.filter((c) => Object.values(pairs).includes(c)));
  const opening = new Set(Object.keys(pairs).filter((o) => closing.has(pairs[o]!)));
  const stack: string[] = [];
  for (const t of tokenize(restOfLine)) {
    if (closing.has(t.text)) {
      if (stack.length === 0) return t.end;
      stack.pop();
    } else if (opening.has(t.text)) stack.push(t.text);
  }
  return null;
}

const GREEK =
  'alpha|beta|gamma|Gamma|delta|Delta|epsilon|varepsilon|zeta|eta|theta|Theta|iota|kappa|lambda|Lambda|mu|nu|omicron|xi|Xi|pi|Pi|rho|sigma|Sigma|tau|upsilon|Upsilon|varphi|phi|Phi|chi|psi|Psi|omega|Omega';

function matchingOpen(s: string, closeAt: number, open: string, close: string): number {
  let depth = 0;
  for (let i = closeAt; i >= 0; i--) {
    if (s[i] === close) depth++;
    else if (s[i] === open && --depth === 0) return i;
  }
  return -1;
}

/**
 * Autofraction for `/` typed at `cursor`: the numerator is the text back to the previous break
 * (space, operator, unmatched opening bracket), skipping over whole bracket groups; `(x+1)/`
 * drops the outer parentheses. Returns the replacement for `[from, to]`, or null.
 */
export function autofraction(cfg: CompiledConfig, ctx: Context, docBefore: string, docOffset: number, selection: Range, selected: string): { from: number; to: number; replacement: string } | null {
  if (cfg.autofractionExcluded.some((e) => isWithin(ctx, e))) return null;
  const b = ctx.bounds;
  if (!b) return null;
  const sym = cfg.settings.autofractionSymbol;
  let start: number;
  let numerator: string;
  if (selection.from !== selection.to) {
    start = selection.from;
    numerator = selected;
  } else {
    const lo = b.innerStart - docOffset;
    // keep: a space after a Greek letter command belongs to the numerator (`\alpha x/`).
    const line = docBefore.replace(new RegExp(`(${GREEK}) ([^ ])`, 'g'), '$1#$2');
    let s = lo;
    const breaking = ' $([{\n' + cfg.settings.autofractionBreakingChars;
    for (let i = line.length - 1; i >= lo; i--) {
      const c = line[i]!;
      if (c === ')' || c === ']' || c === '}') {
        const j = matchingOpen(line, i, c === ')' ? '(' : c === ']' ? '[' : '{', c);
        if (j < 0) return null;
        i = j;
        if (i < lo) {
          s = lo;
          break;
        }
        continue;
      }
      if (breaking.includes(c)) {
        s = i + 1;
        break;
      }
    }
    start = s + docOffset;
    if (start >= selection.to) return null;
    numerator = docBefore.slice(s);
  }
  if (numerator.startsWith('(') && numerator.endsWith(')') && matchingOpen(numerator, numerator.length - 1, '(', ')') === 0) {
    numerator = numerator.slice(1, -1);
  }
  const replacement = numerator === '' ? `${sym}{$0}{$1}$2` : `${sym}{${numerator}}{$0}$1`;
  return { from: start, to: selection.to, replacement };
}

const ENLARGE_PAIRS: Record<string, string> = {
  '(': ')',
  '[': ']',
  '\\{': '\\}',
  '\\lbrace': '\\rbrace',
  '\\langle': '\\rangle',
  '\\lvert': '\\rvert',
  '\\lVert': '\\rVert',
  '\\lfloor': '\\rfloor',
  '\\lceil': '\\rceil',
};
const SIZERS = new Set(['\\left', '\\right', '\\middle', '\\big', '\\Big', '\\bigg', '\\Bigg', '\\bigl', '\\Bigl', '\\biggl', '\\Biggl', '\\bigr', '\\Bigr', '\\biggr', '\\Biggr']);

/**
 * Auto-enlarge brackets: bracket pairs in `math` whose content mentions one of the triggers
 * (`sum`, `frac`, …) become `\left( … \right)`. Returns changes relative to `math`'s start.
 */
export function enlargeBrackets(cfg: CompiledConfig, math: string): { from: number; to: number; insert: string }[] {
  const tokens = tokenize(math);
  const stack: { tok: Token; sized: boolean; close: string }[] = [];
  const changes: { from: number; to: number; insert: string }[] = [];
  tokens.forEach((t, i) => {
    const sized = i > 0 && SIZERS.has(tokens[i - 1]!.text);
    if (t.text in ENLARGE_PAIRS) {
      stack.push({ tok: t, sized, close: ENLARGE_PAIRS[t.text]! });
      return;
    }
    const k = stack.map((e) => e.close).lastIndexOf(t.text);
    if (k < 0) return;
    const open = stack[k]!;
    stack.length = k;
    if (open.sized || sized) return;
    const inner = math.slice(open.tok.end, t.start);
    if (!cfg.enlargeTriggers.some((trig) => inner.includes(trig))) return;
    changes.push({ from: open.tok.start, to: open.tok.end, insert: `\\left${open.tok.text} ` });
    changes.push({ from: t.start, to: t.end, insert: ` \\right${t.text}` });
  });
  return changes;
}
