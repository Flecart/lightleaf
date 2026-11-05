// The AI review dialog: start a review or a reference check, follow progress, see what was
// written, and remove AI comments. Loaded on demand by ReviewEntry.
import { useEffect, useRef, useState } from 'preact/hooks';
import { get, post } from '../api';
import { Modal, Spinner, toast } from '../ui';
import type { ReviewProps } from './ReviewEntry';

type Severity = 'critical' | 'warning' | 'suggestion';
type Kind = 'review' | 'references' | 'clear';

interface ReviewComment {
  agent: string;
  severity: Severity;
  text: string;
  quote: string;
  file: string;
}

interface ReferenceProblem {
  key: string;
  verdict: string;
  severity: Severity;
  message: string;
  bibFile: string | null;
}

interface ReferenceReport {
  bibFiles: string[];
  cited: number;
  checked: number;
  uncited: number;
  truncated: boolean;
  byVerdict: Record<string, number>;
  problems: ReferenceProblem[];
}

interface Job {
  id?: string;
  kind?: Kind;
  status: 'none' | 'queued' | 'running' | 'done' | 'failed';
  stage?: string;
  error?: string | null;
  result?: {
    paperType?: string;
    summary?: string;
    written?: number;
    skipped?: number;
    unanchored?: number;
    removed?: number;
    bySeverity?: Record<string, number>;
    failedAgents?: { agent: string; error: string }[];
    comments?: ReviewComment[];
    references?: ReferenceReport;
    usage?: { inputTokens: number; outputTokens: number };
  } | null;
}

interface Health {
  ok: boolean;
  reviewer?: { venues?: { id: string; label: string }[]; llmConfigured?: boolean; pack?: string };
}

const SEVERITY_ORDER: Severity[] = ['critical', 'warning', 'suggestion'];
const VERDICT_LABELS: Record<string, string> = {
  verified: 'verified',
  unverified: 'not confirmed',
  mismatch: 'details differ',
  fabricated: 'not found',
  doi_not_found: 'DOI not found',
  doi_mismatch: 'DOI points elsewhere',
  no_title: 'no title',
  skipped_web: 'web/software (skipped)',
};

const DONE_MESSAGES: Record<Kind, (j: Job) => string> = {
  review: (j) => `Review finished: ${j.result?.written ?? 0} comments added`,
  references: (j) => `Reference check finished: ${j.result?.references?.problems.length ?? 0} problems`,
  clear: (j) => `Removed ${j.result?.removed ?? 0} AI comments`,
};

function readPref(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function writePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* private mode: preferences are a convenience only */
  }
}

function CommentList(props: { comments: ReviewComment[]; onOpen: (file: string) => void }) {
  const sorted = [...props.comments].sort((a, b) => SEVERITY_ORDER.indexOf(a.severity) - SEVERITY_ORDER.indexOf(b.severity));
  return (
    <ul class="review-list">
      {sorted.map((c) => (
        <li class={`review-item sev-${c.severity}`} onClick={() => props.onOpen(c.file)}>
          <div class="review-meta"><span class="badge">{c.severity}</span> {c.agent} · <code>{c.file}</code></div>
          <div>{c.text}</div>
          <div class="muted small review-quote">“{c.quote}”</div>
        </li>
      ))}
    </ul>
  );
}

function ReferenceResults(props: { report: ReferenceReport; written: number }) {
  const r = props.report;
  if (!r.bibFiles.length) return <p class="muted">No .bib file found in this project.</p>;
  return (
    <>
      <p class="muted">
        {r.checked} of {r.cited} cited references checked in {r.bibFiles.join(', ')}
        {Object.entries(r.byVerdict).map(([k, n]) => ` · ${n} ${VERDICT_LABELS[k] ?? k}`)}
        {r.truncated && ' · stopped at the limit'} · {props.written} comments written next to the citations.
      </p>
      {r.problems.length === 0 ? (
        <p>No problems found.</p>
      ) : (
        <ul class="review-list">
          {r.problems.map((p) => (
            <li class={`review-item sev-${p.severity}`}>
              <div class="review-meta"><span class="badge">{VERDICT_LABELS[p.verdict] ?? p.verdict}</span> <code>{p.key}</code>{p.bibFile && <> · {p.bibFile}</>}</div>
              <div style={{ whiteSpace: 'pre-line' }}>{p.message}</div>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

export function ReviewModal(props: ReviewProps) {
  const [job, setJob] = useState<Job | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [venue, setVenue] = useState(readPref('ll-review-venue', ''));
  const [strict, setStrict] = useState(readPref('ll-review-strict', '') === '1');
  const [focus, setFocus] = useState('');
  const [busy, setBusy] = useState(false);
  const [unavailable, setUnavailable] = useState<string | null>(null);
  const timer = useRef<number>();
  const pending = useRef(false);
  const base = `/api/projects/${props.projectId}/review`;

  const active = job?.status === 'queued' || job?.status === 'running';
  const venues = health?.reviewer?.venues ?? [];

  const poll = async () => {
    try {
      const j = await get<Job>(base);
      setJob(j);
      setUnavailable(null);
      if (j.status === 'done' && pending.current && j.kind) {
        pending.current = false;
        toast(DONE_MESSAGES[j.kind](j), 'success');
        props.onFinished();
      }
    } catch (e: any) {
      setUnavailable(e.message);
    }
  };

  useEffect(() => {
    poll();
    get<Health>('/api/reviewer/health').then(setHealth).catch(() => setHealth(null));
    return () => clearTimeout(timer.current);
  }, []);
  useEffect(() => {
    clearTimeout(timer.current);
    if (active) timer.current = window.setTimeout(poll, 2500);
  }, [job]);

  const start = async (kind: Kind) => {
    if (kind === 'clear' && !confirm('Remove every AI comment (\\AI[…]{…}) from the project?')) return;
    setBusy(true);
    try {
      writePref('ll-review-venue', venue);
      writePref('ll-review-strict', strict ? '1' : '');
      const url = { review: base, references: `${base}/references`, clear: `${base}/clear` }[kind];
      const j = await post<Job>(url, kind === 'review' ? { venue, focus, strict } : {});
      pending.current = true;
      setJob(j);
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const r = job?.result;
  const disabled = busy || !!unavailable;

  return (
    <Modal title="AI review" onClose={props.onClose} wide>
      <p class="muted">
        A team of AI reviewers reads the whole paper and writes its comments into the LaTeX source as{' '}
        <code>\AI[…]{'{…}'}</code>, so they appear in the PDF next to the text they are about. They arrive as one commit in{' '}
        <strong>History</strong>: revert it, remove them below, or hide them all with <code>\usepackage[hide]{'{lightleaf-comments}'}</code>.
        The <code>\AI</code> command is reserved for these comments; use another command for your own notes.
      </p>
      {unavailable && <div class="alert error">{unavailable}</div>}
      {health?.reviewer && health.reviewer.llmConfigured === false && (
        <div class="alert">The review service has no model configured: reviews are disabled, the reference check still works.</div>
      )}

      {active ? (
        <div class="alert">
          <Spinner /> {{ review: 'Reviewing', references: 'Checking references', clear: 'Removing AI comments' }[job?.kind ?? 'review']}: {job?.stage ?? 'starting'}…
          <div class="muted small">This can take a few minutes. You can close this dialog and keep working; the comments appear when it is done.</div>
        </div>
      ) : (
        <form class="stack" onSubmit={(e) => { e.preventDefault(); start('review'); }}>
          <label>
            Target venue
            {venues.length ? (
              <select value={venue} onChange={(e) => setVenue(e.currentTarget.value)}>
                <option value="">None / arXiv preprint</option>
                {venues.map((v) => <option value={v.id}>{v.label}</option>)}
              </select>
            ) : (
              <input placeholder="e.g. NeurIPS" value={venue} onInput={(e) => setVenue(e.currentTarget.value)} />
            )}
          </label>
          <label>
            Anything the reviewers should focus on? (optional)
            <textarea rows={2} placeholder="e.g. Is the evaluation convincing?" value={focus} onInput={(e) => setFocus(e.currentTarget.value)} />
          </label>
          <label class="check">
            <input type="checkbox" checked={strict} onChange={(e) => setStrict(e.currentTarget.checked)} /> Strict: only issues a
            reviewer would raise (no suggestions, at most 30 comments)
          </label>
          <div class="row wrap">
            <button class="btn primary" disabled={disabled}>Start review</button>
            <button type="button" class="btn" disabled={disabled} onClick={() => start('references')} title="Verify cited references against Semantic Scholar">
              Check references
            </button>
            <button type="button" class="btn" disabled={disabled} onClick={() => start('clear')}>Remove AI comments</button>
          </div>
        </form>
      )}

      {job?.status === 'failed' && <div class="alert error">The last job failed: {job.error}</div>}

      {job?.status === 'done' && job.kind === 'review' && r && (
        <>
          <h3>Last review</h3>
          {r.summary && <p>{r.paperType && <span class="badge">{r.paperType}</span>} {r.summary}</p>}
          <p class="muted">
            {r.written ?? 0} comments written
            {SEVERITY_ORDER.filter((s) => r.bySeverity?.[s]).map((s) => ` · ${r.bySeverity![s]} ${s}`)}
            {(r.skipped ?? 0) + (r.unanchored ?? 0) > 0 && ` · ${(r.skipped ?? 0) + (r.unanchored ?? 0)} could not be placed`}
            {r.usage && ` · ${Math.round((r.usage.inputTokens + r.usage.outputTokens) / 1000)}k tokens`}
          </p>
          {!!r.failedAgents?.length && <div class="alert">Some reviewers did not finish: {r.failedAgents.map((f) => f.agent).join(', ')}.</div>}
          <CommentList comments={r.comments ?? []} onOpen={(f) => { props.onOpenFile(f); props.onClose(); }} />
        </>
      )}
      {job?.status === 'done' && job.kind === 'references' && r?.references && (
        <>
          <h3>Last reference check</h3>
          <ReferenceResults report={r.references} written={r.written ?? 0} />
        </>
      )}
      {job?.status === 'done' && job.kind === 'clear' && <p class="muted">Removed {r?.removed ?? 0} AI comments.</p>}
    </Modal>
  );
}
