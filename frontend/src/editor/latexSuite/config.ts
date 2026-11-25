/**
 * Latex Suite configuration: the stored settings object, its defaults, and compilation of the
 * snippet and variable files into matchable {@link Snippet}s.
 *
 * The stored format is the same object as Obsidian's `.obsidian/plugins/obsidian-latex-suite/data.json`
 * (snippets and variables as source text, plus the plain settings), so that file can be imported
 * as is. Keys that are missing fall back to {@link DEFAULT_SETTINGS}; keys for features that are
 * not ported (conceal, math preview, vim, …) are kept but ignored.
 */
import defaultSnippets from './default-snippets.js?raw';
import defaultVariables from './default-variables.js?raw';
import { parseLiteral, ParseError, Unsupported, Value } from './parse';

export interface LatexSuiteSettings {
  snippets: string;
  snippetVariables: string;
  snippetsEnabled: boolean;
  removeSnippetWhitespace: boolean;
  autofractionEnabled: boolean;
  autofractionSymbol: string;
  autofractionBreakingChars: string;
  autofractionExcludedEnvs: string;
  matrixShortcutsEnabled: boolean;
  matrixShortcutsEnvNames: string;
  taboutEnabled: boolean;
  taboutClosingSymbols: string;
  autoEnlargeBrackets: boolean;
  autoEnlargeBracketsTriggers: string;
  wordDelimiters: string;
  [other: string]: unknown;
}

export const DEFAULT_SETTINGS: LatexSuiteSettings = {
  snippets: defaultSnippets,
  snippetVariables: defaultVariables,
  snippetsEnabled: true,
  removeSnippetWhitespace: true,
  autofractionEnabled: true,
  autofractionSymbol: '\\frac',
  autofractionBreakingChars: '+-=\t',
  autofractionExcludedEnvs: '[\n\t["^{", "}"],\n\t["\\\\pu{", "}"]\n]',
  matrixShortcutsEnabled: true,
  matrixShortcutsEnvNames: 'pmatrix, cases, align, bmatrix, Bmatrix, vmatrix, Vmatrix, array, matrix',
  taboutEnabled: true,
  taboutClosingSymbols: '), ], \\rbrack, \\}, \\rbrace, \\rangle, \\rvert, \\rVert, \\rfloor, \\rceil, \\urcorner, }',
  autoEnlargeBrackets: true,
  autoEnlargeBracketsTriggers: 'sum, int, frac, prod',
  wordDelimiters: '., +-\\n\t:;!?\\/{}[]()=~$',
};

/** Settings as stored (any subset of keys) merged over the defaults. */
export function withDefaults(stored: Record<string, unknown> | null): LatexSuiteSettings {
  const out: LatexSuiteSettings = { ...DEFAULT_SETTINGS };
  if (stored) {
    for (const [k, v] of Object.entries(stored)) {
      const def = (DEFAULT_SETTINGS as Record<string, unknown>)[k];
      if (def === undefined || typeof v === typeof def) out[k] = v;
    }
  }
  return out;
}

export interface SnippetOptions {
  automatic: boolean;
  regex: boolean;
  wordBoundary: boolean;
  /** Mode flags; all false means "any mode". */
  text: boolean;
  inlineMath: boolean;
  blockMath: boolean;
  code: boolean;
}

export interface Snippet {
  kind: 'string' | 'regex' | 'visual';
  /** Literal trigger (string/visual) or a regex anchored at the end, `(?:…)$`. */
  trigger: string | RegExp;
  replacement: string;
  options: SnippetOptions;
  priority: number;
  /** `{open, close}` pairs inside which this snippet does not fire. */
  excluded: Env[];
}

export interface Env {
  open: string;
  close: string;
}

export interface CompiledConfig {
  settings: LatexSuiteSettings;
  snippets: Snippet[];
  wordDelimiters: string;
  autofractionExcluded: Env[];
  matrixEnvs: string[];
  taboutClosing: string[];
  enlargeTriggers: string[];
  /** Problems found while compiling; the rest of the configuration still works. */
  warnings: string[];
}

/** Environments in which some default triggers never fire (as in Latex Suite). */
const BUILTIN_EXCLUSIONS: Record<string, Env[]> = {
  '([A-Za-z])(\\d)': [
    { open: '\\pu{', close: '}' },
    { open: '\\ce{', close: '}' },
  ],
  '->': [{ open: '\\ce{', close: '}' }],
};

function parseOptions(src: string): SnippetOptions {
  const o: SnippetOptions = {
    automatic: false,
    regex: false,
    wordBoundary: false,
    text: false,
    inlineMath: false,
    blockMath: false,
    code: false,
  };
  for (const c of src) {
    if (c === 'A') o.automatic = true;
    else if (c === 'r') o.regex = true;
    else if (c === 'w') o.wordBoundary = true;
    else if (c === 'm') o.inlineMath = o.blockMath = true;
    else if (c === 'n') o.inlineMath = true;
    else if (c === 'M') o.blockMath = true;
    else if (c === 't') o.text = true;
    else if (c === 'c') o.code = true;
  }
  if (!o.text && !o.inlineMath && !o.blockMath && !o.code) o.text = o.inlineMath = o.blockMath = o.code = true;
  return o;
}

function describe(e: unknown): string {
  return e instanceof ParseError || e instanceof Error ? e.message : String(e);
}

/** Parse the variables file: `{ "${GREEK}": "alpha|beta", NAME: "…" }`. */
export function compileVariables(src: string, warnings: string[]): Record<string, string> {
  const out: Record<string, string> = {};
  let v: Value;
  try {
    v = parseLiteral(src);
  } catch (e) {
    warnings.push(`Snippet variables: ${describe(e)}`);
    return out;
  }
  if (!v || typeof v !== 'object' || Array.isArray(v) || v instanceof RegExp || v instanceof Unsupported) {
    warnings.push('Snippet variables: expected an object');
    return out;
  }
  for (const [k, val] of Object.entries(v)) {
    if (typeof val !== 'string') {
      warnings.push(`Snippet variable ${k}: value must be a string`);
      continue;
    }
    if (k.startsWith('${') !== k.endsWith('}')) {
      warnings.push(`Snippet variable '${k}': write it as \${NAME} or NAME`);
      continue;
    }
    out[k.startsWith('${') ? k : '${' + k + '}'] = val;
  }
  return out;
}

function substitute(s: string, vars: Record<string, string>): string {
  for (const [k, v] of Object.entries(vars)) s = s.split(k).join(v);
  return s;
}

function compileSnippet(raw: Value, vars: Record<string, string>): Snippet | string {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw) || raw instanceof RegExp || raw instanceof Unsupported) {
    return 'not a snippet object';
  }
  const { trigger, replacement, options, priority, flags } = raw;
  if (replacement instanceof Unsupported) {
    return `function replacements are not supported (line ${replacement.line})`;
  }
  if (typeof replacement !== 'string') return 'replacement must be a string';
  if (typeof options !== 'string') return 'options must be a string';
  if (raw.triggerKey !== undefined && raw.triggerKey !== '') return 'custom trigger keys are not supported';
  if (priority !== undefined && typeof priority !== 'number') return 'priority must be a number';
  const opts = parseOptions(options);
  const base = { replacement, options: opts, priority: (priority as number | undefined) ?? 0 };

  if (opts.regex || trigger instanceof RegExp) {
    let source: string;
    let fl = typeof flags === 'string' ? flags : '';
    if (trigger instanceof RegExp) {
      source = trigger.source;
      fl = trigger.flags + fl;
    } else if (typeof trigger === 'string') source = trigger;
    else return 'trigger must be a string or a regular expression';
    source = substitute(source, vars);
    // keep: Latex Suite keeps only these flags; 'g'/'y' would make exec() stateful.
    fl = [...new Set(fl)].filter((f) => 'imsuv'.includes(f)).join('');
    let re: RegExp;
    try {
      re = new RegExp(`(?:${source})$`, fl);
    } catch (e) {
      return `invalid regex: ${describe(e)}`;
    }
    return { ...base, kind: 'regex', trigger: re, excluded: BUILTIN_EXCLUSIONS[source] ?? [] };
  }
  if (typeof trigger !== 'string' || trigger === '') return 'trigger must be a non-empty string';
  const t = substitute(trigger, vars);
  return {
    ...base,
    kind: replacement.includes('${VISUAL}') ? 'visual' : 'string',
    trigger: t,
    excluded: BUILTIN_EXCLUSIONS[t] ?? [],
  };
}

function triggerLength(s: Snippet): number {
  return typeof s.trigger === 'string' ? s.trigger.length : s.trigger.source.length;
}

function lineOfEntry(src: string, index: number): string {
  const m = [...src.matchAll(/\btrigger\s*:/g)][index];
  return m ? ` (line ${src.slice(0, m.index).split('\n').length})` : '';
}

/** Parse and compile the snippet file. Bad entries are skipped with a warning. */
export function compileSnippets(src: string, vars: Record<string, string>, warnings: string[]): Snippet[] {
  let v: Value;
  try {
    v = parseLiteral(src);
  } catch (e) {
    warnings.push(`Snippets: ${describe(e)}`);
    return [];
  }
  if (!Array.isArray(v)) {
    warnings.push('Snippets: expected an array');
    return [];
  }
  const out: Snippet[] = [];
  v.flat(Infinity as 1).forEach((raw, i) => {
    const s = compileSnippet(raw as Value, vars);
    if (typeof s === 'string') {
      const trig = raw && typeof raw === 'object' && 'trigger' in raw ? ` "${String((raw as { trigger: Value }).trigger)}"` : '';
      warnings.push(`Snippet${trig}${lineOfEntry(src, i)} skipped: ${s}`);
    } else out.push(s);
  });
  // keep: Latex Suite's order: priority first, then longer triggers (regex source length).
  return out
    .map((s, i) => ({ s, i }))
    .sort((a, b) => b.s.priority - a.s.priority || triggerLength(b.s) - triggerLength(a.s) || a.i - b.i)
    .map(({ s }) => s);
}

function list(s: string): string[] {
  return s
    .split(',')
    .map((x) => x.trim())
    .filter(Boolean);
}

function compileEnvs(src: string, warnings: string[]): Env[] {
  try {
    const v = parseLiteral(src);
    if (Array.isArray(v)) {
      return v.flatMap((p) =>
        Array.isArray(p) && typeof p[0] === 'string' && typeof p[1] === 'string' ? [{ open: p[0], close: p[1] }] : [],
      );
    }
  } catch (e) {
    warnings.push(`Autofraction excluded environments: ${describe(e)}`);
    return [];
  }
  warnings.push('Autofraction excluded environments: expected [["open", "close"], …]');
  return [];
}

export function compileConfig(stored: Record<string, unknown> | null): CompiledConfig {
  const settings = withDefaults(stored);
  const warnings: string[] = [];
  const vars = compileVariables(settings.snippetVariables, warnings);
  const snippets = compileSnippets(settings.snippets, vars, warnings);
  return {
    settings,
    snippets,
    wordDelimiters: settings.wordDelimiters.replace('\\n', '\n'),
    autofractionExcluded: compileEnvs(settings.autofractionExcludedEnvs, warnings),
    matrixEnvs: list(settings.matrixShortcutsEnvNames),
    taboutClosing: list(settings.taboutClosingSymbols),
    enlargeTriggers: list(settings.autoEnlargeBracketsTriggers),
    warnings,
  };
}
