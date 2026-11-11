// Finds AI edit markup in LaTeX source and computes what accepting or rejecting it does.
//   \AIadd[tag]{text}               accept -> text,  reject -> nothing
//   \AIdel[tag]{text}               accept -> nothing, reject -> text
//   \AIdelmoved[tag]{where}{block}  accept -> nothing, reject -> block
//   \AInote[tag]{text}              resolve -> nothing
// Pure functions (no CodeMirror) so they can be tested in isolation.

export type AIKind = 'add' | 'del' | 'delmoved' | 'note';

export interface AIMark {
  kind: AIKind;
  /** Whole macro, from the backslash to the closing brace of the last argument. */
  from: number;
  to: number;
  tag: string;
  /** Text kept on accept (add) or on reject (del, delmoved). */
  body: { from: number; to: number };
  /** \AIdelmoved destination. */
  where?: string;
}

const KINDS: Record<string, { kind: AIKind; args: number }> = {
  AIadd: { kind: 'add', args: 1 },
  AIdel: { kind: 'del', args: 1 },
  AIdelmoved: { kind: 'delmoved', args: 2 },
  AInote: { kind: 'note', args: 1 },
};

/** Index of the matching close bracket for the open bracket at `i`, or -1. Skips \-escapes and % comments. */
function matchClose(text: string, i: number, open: string, close: string): number {
  let depth = 0;
  for (let j = i; j < text.length; j++) {
    const c = text[j];
    if (c === '\\') j++;
    else if (c === '%') {
      const nl = text.indexOf('\n', j);
      if (nl < 0) return -1;
      j = nl;
    } else if (c === open) depth++;
    else if (c === close && --depth === 0) return j;
  }
  return -1;
}

function skipSpace(text: string, i: number): number {
  while (i < text.length && (text[i] === ' ' || text[i] === '\t' || text[i] === '\n')) i++;
  return i;
}

/** All complete AI marks, outermost only (marks nested inside another mark are left to a later pass). */
export function findAIMarks(text: string): AIMark[] {
  const marks: AIMark[] = [];
  const re = /\\(AIadd|AIdelmoved|AIdel|AInote)(?![A-Za-z@])/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    const from = m.index;
    if (isEscapedOrCommented(text, from)) continue;
    const spec = KINDS[m[1]];
    let i = skipSpace(text, re.lastIndex);
    let tag = '';
    if (text[i] === '[') {
      const end = matchClose(text, i, '[', ']');
      if (end < 0) continue;
      tag = text.slice(i + 1, end).trim();
      i = skipSpace(text, end + 1);
    }
    const args: { from: number; to: number }[] = [];
    for (let a = 0; a < spec.args; a++) {
      if (text[i] !== '{') break;
      const end = matchClose(text, i, '{', '}');
      if (end < 0) break;
      args.push({ from: i + 1, to: end });
      i = a + 1 < spec.args ? skipSpace(text, end + 1) : end + 1;
    }
    if (args.length < spec.args) continue;
    const body = args[args.length - 1];
    marks.push({
      kind: spec.kind,
      from,
      to: i,
      tag,
      body,
      where: spec.kind === 'delmoved' ? text.slice(args[0].from, args[0].to) : undefined,
    });
    re.lastIndex = i;
  }
  return marks;
}

/** The backslash at `pos` is itself escaped (\\AIadd) or sits in a % comment on its line. */
function isEscapedOrCommented(text: string, pos: number): boolean {
  let bs = 0;
  for (let j = pos - 1; j >= 0 && text[j] === '\\'; j--) bs++;
  if (bs % 2 === 1) return true;
  const lineStart = text.lastIndexOf('\n', pos - 1) + 1;
  for (let j = lineStart; j < pos; j++) {
    if (text[j] === '\\') j++;
    else if (text[j] === '%') return true;
  }
  return false;
}

export interface Change {
  from: number;
  to: number;
  insert: string;
}

/** The edit that accepts (`true`) or rejects (`false`) a mark. Notes are removed either way. */
export function resolveMark(text: string, mark: AIMark, accept: boolean): Change {
  const keep = mark.kind === 'add' ? accept : mark.kind === 'note' ? false : !accept;
  if (keep) return { from: mark.from, to: mark.to, insert: text.slice(mark.body.from, mark.body.to) };
  // Removing the whole macro: also drop one adjacent space so "a \AIdel{b} c" becomes "a c", not "a  c",
  // and "a \AInote{b}." becomes "a.".
  let { from, to } = mark;
  const before = text[from - 1];
  const after = text[to];
  if (before === ' ' && (after === undefined || /[\s.,;:!?)\]]/.test(after))) from--;
  else if ((before === undefined || before === '\n') && after === ' ') to++;
  return { from, to, insert: '' };
}

/** Changes that resolve every mark, in document order and non-overlapping. Nested marks need another pass. */
export function resolveAll(text: string, accept: boolean): Change[] {
  const out: Change[] = [];
  let last = -1;
  for (const mark of findAIMarks(text)) {
    const c = resolveMark(text, mark, accept);
    if (c.from < last) c.from = last; // two removals may both claim the space between them
    out.push(c);
    last = c.to;
  }
  return out;
}

/** Apply non-overlapping changes in document order (for tests and previews). */
export function applyChanges(text: string, changes: Change[]): string {
  let out = '';
  let pos = 0;
  for (const c of changes) {
    out += text.slice(pos, c.from) + c.insert;
    pos = c.to;
  }
  return out + text.slice(pos);
}
