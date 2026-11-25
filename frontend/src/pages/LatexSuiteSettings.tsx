import { Fragment } from 'preact';
import { useEffect, useMemo, useState } from 'preact/hooks';
import { Icon, toast } from '../ui';
import { compileConfig, DEFAULT_SETTINGS, LatexSuiteSettings, withDefaults } from '../editor/latexSuite/config';
import { fetchLatexSuite, saveLatexSuite } from '../editor/latexSuite/load';

type BoolKey = 'snippetsEnabled' | 'autofractionEnabled' | 'matrixShortcutsEnabled' | 'taboutEnabled' | 'autoEnlargeBrackets' | 'removeSnippetWhitespace';
type TextKey =
  | 'wordDelimiters'
  | 'autofractionSymbol'
  | 'autofractionBreakingChars'
  | 'matrixShortcutsEnvNames'
  | 'taboutClosingSymbols'
  | 'autoEnlargeBracketsTriggers';

const TOGGLES: [BoolKey, string][] = [
  ['snippetsEnabled', 'Snippets'],
  ['autofractionEnabled', 'Autofraction (x/ → \\frac{x}{})'],
  ['matrixShortcutsEnabled', 'Matrix shortcuts (Tab → &, Enter → \\\\)'],
  ['taboutEnabled', 'Tabout (Tab leaves brackets and math)'],
  ['autoEnlargeBrackets', 'Auto-enlarge brackets around \\sum, \\frac, …'],
  ['removeSnippetWhitespace', 'Trim trailing spaces of snippets in inline math'],
];

const TEXTS: [TextKey, string][] = [
  ['wordDelimiters', 'Word delimiters (for the w option)'],
  ['autofractionSymbol', 'Autofraction command'],
  ['autofractionBreakingChars', 'Autofraction breaking characters'],
  ['matrixShortcutsEnvNames', 'Matrix shortcut environments'],
  ['taboutClosingSymbols', 'Tabout closing symbols'],
  ['autoEnlargeBracketsTriggers', 'Auto-enlarge triggers'],
];

/** The keys of `s` that differ from the defaults: stored that way, untouched parts keep following new defaults. */
function changedKeys(s: LatexSuiteSettings): Record<string, unknown> | null {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(s)) {
    if ((DEFAULT_SETTINGS as Record<string, unknown>)[k] !== v) out[k] = v;
  }
  return Object.keys(out).length > 0 ? out : null;
}

function download(name: string, text: string) {
  const a = document.createElement('a');
  a.href = URL.createObjectURL(new Blob([text], { type: 'application/json' }));
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}

/** Settings card for the Latex Suite snippets and shortcuts (stored per account). */
export function LatexSuiteCard() {
  const [saved, setSaved] = useState<LatexSuiteSettings | null>(null);
  const [draft, setDraft] = useState<LatexSuiteSettings>(DEFAULT_SETTINGS);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    fetchLatexSuite()
      .then((s) => {
        const full = withDefaults(s);
        setSaved(full);
        setDraft(full);
      })
      .catch((e) => toast(`Could not load snippet settings: ${e.message}`, 'error'));
  }, []);

  const compiled = useMemo(() => compileConfig(changedKeys(draft)), [draft]);
  const dirty = saved !== null && JSON.stringify(changedKeys(saved)) !== JSON.stringify(changedKeys(draft));
  const set = (patch: Partial<LatexSuiteSettings>) => setDraft((d) => ({ ...d, ...patch }));

  const save = async (next: LatexSuiteSettings) => {
    setBusy(true);
    try {
      await saveLatexSuite(changedKeys(next));
      setSaved(next);
      setDraft(next);
      toast('Snippet settings saved; reopen a project to use them', 'success');
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const importFile = async (file: File) => {
    const text = await file.text();
    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch {
      set({ snippets: text });
      toast(`Loaded snippets from ${file.name}; review and save`, 'info');
      return;
    }
    if (!json || typeof json !== 'object' || Array.isArray(json)) {
      toast('Expected a Latex Suite data.json object or a snippets file', 'error');
      return;
    }
    const obj = json as Record<string, unknown>;
    if (obj.loadSnippetsFromFile === true || obj.loadSnippetVariablesFromFile === true) {
      toast('That data.json loads snippets from vault files: import those files as well', 'info');
    }
    setDraft(withDefaults(obj));
    toast(`Loaded ${file.name}; review and save`, 'info');
  };

  if (!saved) return null;
  const snippetWarnings = compiled.warnings;

  return (
    <section class="card latex-suite">
      <h2><Icon name="edit" /> LaTeX snippets</h2>
      <p class="muted">
        Snippets and math shortcuts for <code>.tex</code> files, compatible with the Obsidian{' '}
        <a href="https://github.com/artisticat1/obsidian-latex-suite" target="_blank" rel="noopener noreferrer">Latex Suite</a>{' '}
        plugin: same snippet syntax (<code>trigger</code>, <code>replacement</code>, <code>options</code> such as
        <code>mA</code>, <code>rmA</code>, <code>tAw</code>; tabstops <code>$0</code>, <code>${'{'}1:default{'}'}</code>; <code>${'{'}VISUAL{'}'}</code>;
        regex groups <code>[[0]]</code>) and the same <code>data.json</code>. Function replacements are not supported.
      </p>

      <div class="ls-toggles">
        {TOGGLES.map(([k, label]) => (
          <label class="check" key={k}>
            <input type="checkbox" checked={draft[k]} onChange={(e) => set({ [k]: e.currentTarget.checked })} />
            {label}
          </label>
        ))}
      </div>

      <label class="ls-label" for="ls-snippets">Snippets</label>
      <textarea
        id="ls-snippets"
        class="ls-source"
        rows={18}
        spellcheck={false}
        value={draft.snippets}
        onInput={(e) => set({ snippets: e.currentTarget.value })}
      />
      <label class="ls-label" for="ls-vars">Snippet variables</label>
      <textarea
        id="ls-vars"
        class="ls-source"
        rows={5}
        spellcheck={false}
        value={draft.snippetVariables}
        onInput={(e) => set({ snippetVariables: e.currentTarget.value })}
      />

      <details class="ls-advanced">
        <summary>Advanced</summary>
        <div class="ls-grid">
          {TEXTS.map(([k, label]) => (
            <Fragment key={k}>
              <label for={`ls-${k}`}>{label}</label>
              <input id={`ls-${k}`} value={draft[k]} spellcheck={false} onInput={(e) => set({ [k]: e.currentTarget.value })} />
            </Fragment>
          ))}
          <label for="ls-excluded">Autofraction excluded environments</label>
          <textarea
            id="ls-excluded"
            class="ls-source"
            rows={4}
            spellcheck={false}
            value={draft.autofractionExcludedEnvs}
            onInput={(e) => set({ autofractionExcludedEnvs: e.currentTarget.value })}
          />
        </div>
      </details>

      <div class={`alert ${snippetWarnings.length ? 'error' : ''}`}>
        {compiled.snippets.length} snippets loaded
        {snippetWarnings.length > 0 && (
          <ul class="ls-warnings">
            {snippetWarnings.map((w) => (
              <li>{w}</li>
            ))}
          </ul>
        )}
      </div>

      <div class="row wrap">
        <button class="btn primary" disabled={!dirty || busy} onClick={() => save(draft)}>Save</button>
        <button class="btn" disabled={!dirty || busy} onClick={() => setDraft(saved)}>Discard changes</button>
        <label class="btn">
          <Icon name="upload" /> Import data.json or snippets file
          <input
            type="file"
            accept=".json,.js,.txt,application/json,text/javascript,text/plain"
            hidden
            onChange={(e) => {
              const f = e.currentTarget.files?.[0];
              e.currentTarget.value = '';
              if (f) importFile(f);
            }}
          />
        </label>
        <button class="btn" onClick={() => download('data.json', JSON.stringify(draft, null, '\t'))}>
          <Icon name="download" /> Export data.json
        </button>
        <button
          class="btn danger"
          disabled={busy}
          onClick={() => confirm('Replace your snippets and settings with the defaults?') && save(withDefaults(null))}
        >
          Reset to defaults
        </button>
      </div>
    </section>
  );
}
