// Forward and backward search for Typst projects.
//
// The Typst CLI writes no SyncTeX data, so the PDF and the sources are matched by their text:
// the words of the source (with Typst markup and code that does not reach the page removed)
// are aligned against the words pdf.js extracts from the PDF. A short window of words around
// the cursor (or the clicked word) is scored against every position on the other side; the
// alignment tolerates words that exist on only one side (heading numbers, function names,
// hyphenation), and ties are broken by the relative position in the document.

import type { PDFDocumentProxy } from 'pdfjs-dist';

/** Normalized word plus where it sits in its source. */
interface SrcWord {
  w: string;
  /** Character offset in the source text. */
  at: number;
}

export interface PdfWord {
  w: string;
  page: number;
  /** Top-left page coordinates at scale 1 (CSS pixels), `y` is the baseline. */
  x: number;
  y: number;
  width: number;
  height: number;
}

const WORD = /[\p{L}\p{N}]+/gu;
const WORD_AT = /[\p{L}\p{N}]+/uy;
const IDENT = /[\p{L}_][\p{L}\p{N}_-]*/uy;
const IDENT_START = /[\p{L}_]/u;

const norm = (s: string) => s.normalize('NFKC').toLowerCase();

/**
 * Tokenizes Typst source into the words that can appear in the output. Skips comments, math,
 * labels, references, escapes and code (identifiers, arguments, strings, `#let`/`#set`/...
 * statements), but descends into content blocks `[...]` anywhere, since those are rendered.
 */
export function typstWords(src: string): SrcWord[] {
  const out: SrcWord[] = [];
  let i = 0;
  const n = src.length;

  const emitWords = (from: number, to: number) => {
    const s = src.slice(from, to);
    for (const m of s.matchAll(WORD)) out.push({ w: norm(m[0]), at: from + m.index! });
  };
  const skipComment = (): boolean => {
    if (src.startsWith('//', i)) {
      const e = src.indexOf('\n', i);
      i = e < 0 ? n : e;
      return true;
    }
    if (src.startsWith('/*', i)) {
      const e = src.indexOf('*/', i + 2);
      i = e < 0 ? n : e + 2;
      return true;
    }
    return false;
  };
  const skipString = () => {
    i++; // opening quote
    while (i < n && src[i] !== '"') i += src[i] === '\\' ? 2 : 1;
    i++;
  };
  const ident = (): string | null => {
    IDENT.lastIndex = i;
    const m = IDENT.exec(src);
    if (!m) return null;
    i += m[0].length;
    return m[0];
  };

  // Markup until `]` (when nested) or the end of input.
  const markup = (nested: boolean) => {
    while (i < n) {
      const c = src[i]!;
      if (nested && c === ']') return void i++;
      if (skipComment()) continue;
      if (c === '\\') {
        i += 2;
      } else if (c === '$') {
        i++;
        while (i < n && src[i] !== '$') i += src[i] === '\\' ? 2 : 1;
        i++;
      } else if (c === '`') {
        let ticks = 0;
        while (src[i + ticks] === '`') ticks++;
        const fence = '`'.repeat(ticks);
        i += ticks;
        if (ticks >= 3) ident(); // language tag
        const e = src.indexOf(fence, i);
        const end = e < 0 ? n : e;
        emitWords(i, end);
        i = end + ticks;
      } else if (c === '<' && /^<[\p{L}\p{N}_.:-]+>/u.test(src.slice(i, i + 200))) {
        i = src.indexOf('>', i) + 1;
      } else if (c === '@') {
        i++;
        ident();
      } else if (c === '#') {
        i++;
        embedded();
      } else if (c === '[' && nested) {
        i++;
        markup(true); // balanced brackets inside a content block
      } else {
        WORD_AT.lastIndex = i;
        const r = WORD_AT.exec(src);
        if (r) {
          out.push({ w: norm(r[0]), at: i });
          i += r[0].length;
        } else i++;
      }
    }
  };

  // Code up to `close` (or, with close = '\n', to the end of the statement).
  const code = (close: string) => {
    while (i < n) {
      const c = src[i]!;
      if (c === close) {
        if (close !== '\n') i++;
        return;
      }
      if (close === '\n' && c === ';') return void i++;
      if (skipComment()) continue;
      if (c === '"') skipString();
      else if (c === '(') {
        i++;
        code(')');
      } else if (c === '{') {
        i++;
        code('}');
      } else if (c === '[') {
        i++;
        markup(true);
      } else i++;
    }
  };

  const STATEMENTS = new Set(['let', 'set', 'show', 'import', 'include', 'return', 'if', 'for', 'while', 'context']);

  // An embedded expression after `#` in markup.
  const embedded = () => {
    const c = src[i];
    if (c === '(') return void (i++, code(')'));
    if (c === '{') return void (i++, code('}'));
    if (c === '[') return void (i++, markup(true));
    if (c === '"') return skipString();
    const id = ident();
    if (!id) return;
    if (STATEMENTS.has(id)) return code('\n');
    // Postfix chain: field access, calls and trailing content blocks.
    while (i < n) {
      const d = src[i];
      if (d === '.' && IDENT_START.test(src[i + 1] ?? '')) {
        i++;
        ident();
      } else if (d === '(') {
        i++;
        code(')');
      } else if (d === '[') {
        i++;
        markup(true);
      } else return;
    }
  };

  markup(false);
  return out;
}

const docWords = new WeakMap<PDFDocumentProxy, Promise<PdfWord[]>>();

/** Words of the PDF with their positions, extracted once per document. */
export function pdfWords(doc: PDFDocumentProxy): Promise<PdfWord[]> {
  let p = docWords.get(doc);
  if (!p) {
    p = extract(doc);
    docWords.set(doc, p);
  }
  return p;
}

async function extract(doc: PDFDocumentProxy): Promise<PdfWord[]> {
  const out: PdfWord[] = [];
  for (let pn = 1; pn <= doc.numPages; pn++) {
    const page = await doc.getPage(pn);
    const vp = page.getViewport({ scale: 1 });
    const tc = await page.getTextContent();
    // A word hyphenated across lines: "exam-" at the end of a line continues on the next.
    let carry: PdfWord | null = null;
    for (const item of tc.items) {
      if (!('str' in item) || !item.str) continue;
      const [a, b, , , e, f] = item.transform as number[];
      const len = item.str.length;
      const fontH = item.height || Math.hypot(a!, b!) || 10;
      const [x0, y0] = vp.convertToViewportPoint(e!, f!);
      const [x1] = vp.convertToViewportPoint(e! + item.width, f!);
      const span = x1! - x0!;
      const hyphenated = item.hasEOL && /[\p{L}]-$/u.test(item.str);
      const matches = [...item.str.matchAll(WORD)];
      matches.forEach((m, k) => {
        const from = m.index!;
        const word: PdfWord = {
          w: norm(m[0]),
          page: pn,
          x: x0! + (span * from) / len,
          y: y0!,
          width: (span * m[0].length) / len,
          height: fontH,
        };
        if (k === 0 && carry && from === 0) {
          carry.w += word.w;
          carry = null;
          return;
        }
        carry = null;
        out.push(word);
        if (hyphenated && k === matches.length - 1) carry = word;
      });
    }
  }
  return out;
}

/**
 * Aligns `window` against `stream` starting at `start`, letting each window word match up to
 * `slack` positions ahead of the previous match. Returns the matched stream index per window
 * word (-1 when unmatched) and the number of matches.
 */
function align(window: string[], stream: { w: string }[], start: number, slack: number) {
  const pos: number[] = [];
  let score = 0;
  let j = start;
  for (let k = 0; k < window.length; k++) {
    let hit = -1;
    // The first word must match exactly at `start`; others may skip a few stream words.
    const lim = Math.min(stream.length, k === 0 ? j + 1 : j + slack + 1);
    for (let t = j; t < lim; t++) {
      if (stream[t]!.w === window[k]) {
        hit = t;
        break;
      }
    }
    pos.push(hit);
    if (hit >= 0) {
      score++;
      j = hit + 1;
    } else if (k === 0) j = start + 1;
  }
  return { pos, score };
}

interface Best {
  score: number;
  dist: number;
  pos: number[];
}

/** Best alignment of `window` over `stream`; `rel` is the expected relative position (0..1). */
function search(window: string[], stream: { w: string }[], slack: number, rel: number, minScore: number): Best | null {
  let best: Best | null = null;
  // Start at any window word, so a missing first word does not hide the match.
  const first = new Set(window);
  for (let s = 0; s < stream.length; s++) {
    if (!first.has(stream[s]!.w)) continue;
    // Align the window suffix that begins with this word.
    const k0 = window.indexOf(stream[s]!.w);
    const { pos, score } = align(window.slice(k0), stream, s, slack);
    if (score < minScore) continue;
    const full = Array<number>(k0).fill(-1).concat(pos);
    const dist = Math.abs(s / Math.max(1, stream.length) - rel);
    if (!best || score > best.score || (score === best.score && dist < best.dist)) best = { score, dist, pos: full };
  }
  return best;
}

const WINDOW = 8;

/** Source offset → PDF box, for jumping from the editor to the PDF. */
export function forward(
  src: string,
  offset: number,
  words: PdfWord[],
): { page: number; h: number; v: number; w: number; hh: number } | null {
  const sw = typstWords(src);
  let a = sw.findIndex((t) => t.at >= offset);
  if (a < 0) a = Math.max(0, sw.length - WINDOW);
  // Prefer the word under the cursor when the cursor is inside one.
  if (a > 0 && sw[a - 1]!.at + sw[a - 1]!.w.length > offset) a--;
  const window = sw.slice(a, a + WINDOW).map((t) => t.w);
  if (!window.length || !words.length) return null;
  const min = Math.min(window.length, window.length >= 4 ? 3 : window.length > 1 ? 2 : 1);
  const best = search(window, words, 3, a / Math.max(1, sw.length), min);
  if (!best) return null;
  const hits = best.pos.filter((p) => p >= 0).map((p) => words[p]!);
  const head = hits[0]!;
  // Highlight the matched words on the first matched line.
  const line = hits.filter((w) => w.page === head.page && Math.abs(w.y - head.y) < head.height / 2);
  const left = Math.min(...line.map((w) => w.x));
  const right = Math.max(...line.map((w) => w.x + w.width));
  return { page: head.page, h: left, v: head.y, w: right - left, hh: head.height };
}

export interface SourceFile {
  path: string;
  text: string;
}

/** PDF click → source line, for jumping from the PDF to the editor. */
export function backward(
  page: number,
  x: number,
  y: number,
  words: PdfWord[],
  files: SourceFile[],
): { file: string; line: number } | null {
  // Word under the click, or the nearest one on the page.
  let k = -1;
  let bestD = Infinity;
  words.forEach((w, i) => {
    if (w.page !== page) return;
    const dx = x < w.x ? w.x - x : x > w.x + w.width ? x - w.x - w.width : 0;
    const dy = y < w.y - w.height ? w.y - w.height - y : y > w.y ? (y - w.y) * 2 : 0;
    const d = dx + dy * 2;
    if (d < bestD) {
      bestD = d;
      k = i;
    }
  });
  if (k < 0 || bestD > 60) return null;
  const from = Math.max(0, k - 3);
  const window = words.slice(from, from + WINDOW).map((w) => w.w);
  const target = k - from;
  const rel = k / Math.max(1, words.length);
  const min = Math.min(window.length, 3);

  let best: (Best & { file: SourceFile; sw: SrcWord[] }) | null = null;
  for (const file of files) {
    const sw = typstWords(file.text);
    const b = search(window, sw, 4, rel, min);
    if (b && (!best || b.score > best.score || (b.score === best.score && b.dist < best.dist))) best = { ...b, file, sw };
  }
  if (!best) return null;
  // The clicked word if it matched, else the nearest matched one.
  let p = -1;
  for (let d = 0; p < 0 && d < window.length; d++) p = Math.max(best.pos[target - d] ?? -1, best.pos[target + d] ?? -1);
  if (p < 0) return null;
  const at = best.sw[p]!.at;
  let line = 1;
  for (let i = 0; i < at; i++) if (best.file.text.charCodeAt(i) === 10) line++;
  return { file: best.file.path, line };
}
