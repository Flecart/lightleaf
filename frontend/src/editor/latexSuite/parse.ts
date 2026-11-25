/**
 * A parser for the JavaScript literal subset used by Latex Suite snippet and variable files.
 *
 * Latex Suite loads those files with `import()`, which our Content-Security-Policy forbids (no
 * `unsafe-eval`), so they are parsed here instead: arrays, objects (quoted or bare keys), strings
 * in any of the three quote styles with JavaScript escapes, numbers, booleans, `null`, regular
 * expression literals, `//` and block comments, and trailing commas. Functions cannot be run;
 * they are skipped and come back as {@link Unsupported} so callers can report them.
 */

export class Unsupported {
  constructor(public what: string, public line: number) {}
}

export type Value = string | number | boolean | null | RegExp | Unsupported | Value[] | { [key: string]: Value };

export class ParseError extends Error {
  constructor(message: string, public line: number, public col: number) {
    super(`line ${line}, column ${col}: ${message}`);
  }
}

const SIMPLE_ESCAPES: Record<string, string> = { n: '\n', t: '\t', r: '\r', b: '\b', f: '\f', v: '\v', '0': '\0' };

class Parser {
  pos = 0;
  constructor(private src: string) {}

  fail(message: string, at = this.pos): never {
    const before = this.src.slice(0, at);
    const line = before.split('\n').length;
    throw new ParseError(message, line, at - before.lastIndexOf('\n'));
  }

  lineAt(at: number): number {
    return this.src.slice(0, at).split('\n').length;
  }

  skipSpace() {
    const s = this.src;
    while (this.pos < s.length) {
      const c = s[this.pos]!;
      if (c === ' ' || c === '\t' || c === '\n' || c === '\r' || c === '﻿' || c === ' ') this.pos++;
      else if (s.startsWith('//', this.pos)) {
        const nl = s.indexOf('\n', this.pos);
        this.pos = nl < 0 ? s.length : nl;
      } else if (s.startsWith('/*', this.pos)) {
        const end = s.indexOf('*/', this.pos + 2);
        if (end < 0) this.fail('unterminated comment');
        this.pos = end + 2;
      } else break;
    }
  }

  peek(): string {
    return this.src[this.pos] ?? '';
  }

  expect(c: string) {
    this.skipSpace();
    if (this.peek() !== c) this.fail(`expected '${c}'`);
    this.pos++;
  }

  value(): Value {
    this.skipSpace();
    const c = this.peek();
    if (c === '[') return this.array();
    if (c === '{') return this.object();
    if (c === '"' || c === "'") return this.string(c);
    if (c === '`') return this.template();
    if (c === '/') return this.regex();
    if (c === '(') return this.skipFunction();
    if (c === '-' || c === '+' || c === '.' || (c >= '0' && c <= '9')) return this.number();
    const word = this.identifier();
    if (word === null) this.fail(c ? `unexpected '${c}'` : 'unexpected end of input');
    switch (word) {
      case 'true':
        return true;
      case 'false':
        return false;
      case 'null':
      case 'undefined':
        return null;
      case 'function':
      case 'async':
        return this.skipFunction(this.pos - word.length);
      default:
        this.skipSpace();
        if (this.src.startsWith('=>', this.pos)) return this.skipFunction(this.pos - word.length);
        return this.fail(`unexpected identifier '${word}'`, this.pos - word.length);
    }
  }

  identifier(): string | null {
    const m = /^[A-Za-z_$][\w$]*/.exec(this.src.slice(this.pos, this.pos + 200));
    if (!m) return null;
    this.pos += m[0].length;
    return m[0];
  }

  array(): Value[] {
    this.pos++;
    const out: Value[] = [];
    for (;;) {
      this.skipSpace();
      if (this.peek() === ']') {
        this.pos++;
        return out;
      }
      out.push(this.value());
      this.skipSpace();
      if (this.peek() === ',') this.pos++;
      else if (this.peek() !== ']') this.fail("expected ',' or ']'");
    }
  }

  object(): { [key: string]: Value } {
    this.pos++;
    const out: { [key: string]: Value } = {};
    for (;;) {
      this.skipSpace();
      const c = this.peek();
      if (c === '}') {
        this.pos++;
        return out;
      }
      let key: string;
      if (c === '"' || c === "'") key = this.string(c);
      else {
        const id = this.identifier() ?? (/^\d+/.exec(this.src.slice(this.pos))?.[0] ?? null);
        if (id === null) this.fail('expected a property name');
        if (/^\d/.test(id)) this.pos += id.length;
        key = id;
      }
      this.skipSpace();
      if (this.peek() === '(') {
        out[key] = this.skipFunction(this.pos);
      } else {
        this.expect(':');
        out[key] = this.value();
      }
      this.skipSpace();
      if (this.peek() === ',') this.pos++;
      else if (this.peek() !== '}') this.fail("expected ',' or '}'");
    }
  }

  escape(): string {
    const s = this.src;
    const c = s[this.pos++];
    if (c === undefined) this.fail('unterminated string');
    if (c in SIMPLE_ESCAPES) return SIMPLE_ESCAPES[c]!;
    if (c === '\r') {
      if (s[this.pos] === '\n') this.pos++;
      return '';
    }
    if (c === '\n' || c === ' ' || c === ' ') return '';
    if (c === 'x') {
      const hex = s.slice(this.pos, this.pos + 2);
      if (!/^[0-9a-fA-F]{2}$/.test(hex)) this.fail('bad \\x escape');
      this.pos += 2;
      return String.fromCharCode(parseInt(hex, 16));
    }
    if (c === 'u') {
      if (s[this.pos] === '{') {
        const end = s.indexOf('}', this.pos);
        const hex = s.slice(this.pos + 1, end);
        if (end < 0 || !/^[0-9a-fA-F]{1,6}$/.test(hex)) this.fail('bad \\u escape');
        this.pos = end + 1;
        return String.fromCodePoint(parseInt(hex, 16));
      }
      const hex = s.slice(this.pos, this.pos + 4);
      if (!/^[0-9a-fA-F]{4}$/.test(hex)) this.fail('bad \\u escape');
      this.pos += 4;
      return String.fromCharCode(parseInt(hex, 16));
    }
    return c;
  }

  string(quote: string): string {
    const start = this.pos;
    this.pos++;
    let out = '';
    const s = this.src;
    for (;;) {
      const c = s[this.pos];
      if (c === undefined || c === '\n') this.fail('unterminated string', start);
      this.pos++;
      if (c === quote) return out;
      out += c === '\\' ? this.escape() : c;
    }
  }

  template(): Value {
    const start = this.pos;
    this.pos++;
    let out = '';
    const s = this.src;
    for (;;) {
      const c = s[this.pos];
      if (c === undefined) this.fail('unterminated template string', start);
      this.pos++;
      if (c === '`') return out;
      if (c === '$' && s[this.pos] === '{') {
        this.pos = start;
        this.skipBalanced();
        return new Unsupported('template string with ${…}', this.lineAt(start));
      }
      out += c === '\\' ? this.escape() : c;
    }
  }

  regex(): RegExp {
    const start = this.pos;
    this.pos++;
    const s = this.src;
    let inClass = false;
    let body = '';
    for (;;) {
      const c = s[this.pos];
      if (c === undefined || c === '\n') this.fail('unterminated regular expression', start);
      this.pos++;
      if (c === '\\') {
        body += c + (s[this.pos++] ?? '');
        continue;
      }
      if (c === '[') inClass = true;
      else if (c === ']') inClass = false;
      else if (c === '/' && !inClass) break;
      body += c;
    }
    const flags = /^[a-z]*/.exec(s.slice(this.pos))![0];
    this.pos += flags.length;
    try {
      return new RegExp(body, flags);
    } catch (e) {
      return this.fail(`invalid regular expression: ${(e as Error).message}`, start);
    }
  }

  number(): number {
    const m = /^[-+]?(0[xX][0-9a-fA-F]+|(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?|Infinity)/.exec(this.src.slice(this.pos));
    if (!m) this.fail('bad number');
    this.pos += m[0].length;
    return Number(m[0]);
  }

  /**
   * Skip a function (`function (…) {…}`, `async …`, `(…) => …` or `name(…) {…}` in an object)
   * up to the `,` / `]` / `}` that ends the value.
   */
  skipFunction(start = this.pos): Unsupported {
    this.pos = start;
    const s = this.src;
    while (this.pos < s.length) {
      this.skipSpace();
      const c = this.peek();
      if (c === ',' || c === ']' || c === '}' || c === '') break;
      if (c === '(' || c === '[' || c === '{') this.skipBalanced();
      else if (c === '"' || c === "'") this.string(c);
      else if (c === '`') this.skipBalanced();
      else this.pos++;
    }
    return new Unsupported('function', this.lineAt(start));
  }

  /** Skip a bracketed group or template string, honouring strings, comments and nested templates. */
  skipBalanced() {
    const s = this.src;
    const stack: string[] = [];
    const close: Record<string, string> = { '(': ')', '[': ']', '{': '}' };
    do {
      const top = stack[stack.length - 1];
      if (top === '`') {
        const c = s[this.pos];
        if (c === undefined) this.fail('unterminated template string');
        if (c === '\\') this.pos += 2;
        else if (c === '`') (stack.pop(), this.pos++);
        else if (c === '$' && s[this.pos + 1] === '{') (stack.push('}'), (this.pos += 2));
        else this.pos++;
        continue;
      }
      this.skipSpace();
      const c = s[this.pos];
      if (c === undefined) this.fail('unbalanced brackets');
      if (c in close) (stack.push(close[c]!), this.pos++);
      else if (c === '`') (stack.push('`'), this.pos++);
      else if (c === '"' || c === "'") this.string(c);
      else if (c === top) (stack.pop(), this.pos++);
      else if (c === ')' || c === ']' || c === '}') this.fail(`unexpected '${c}'`);
      else this.pos++;
    } while (stack.length > 0);
  }

  document(): Value {
    this.skipSpace();
    if (this.src.startsWith('export default', this.pos)) this.pos += 'export default'.length;
    const v = this.value();
    this.skipSpace();
    if (this.peek() === ';') this.pos++;
    this.skipSpace();
    if (this.pos < this.src.length) this.fail('unexpected text after the value');
    return v;
  }
}

/** Parse a snippet or variable file. Throws {@link ParseError} on a syntax error. */
export function parseLiteral(src: string): Value {
  return new Parser(src).document();
}
