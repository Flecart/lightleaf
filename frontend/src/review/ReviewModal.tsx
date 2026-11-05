// The AI review dialog: start a review, follow its progress, see what it wrote, remove AI comments.
import { useEffect, useRef, useState } from 'preact/hooks';
import { get, post } from '../api';
import { Modal, Spinner, toast } from '../ui';
import type { ReviewProps } from './ReviewEntry';

interface ReviewComment {
  agent: string;
  severity: 'critical' | 'warning' | 'suggestion';
  text: string;
  quote: string;
  file: string;
}

interface Job {
  id?: string;
  kind?: 'review' | 'clear';
  status: 'none' | 'queued' | 'running' | 'done' | 'failed';
  stage?: string;
  error?: string | null;
  createdAt?: number;
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
    commit?: string | null;
    usage?: { inputTokens: number; outputTokens: number; cacheReadTokens: number };
  } | null;
}

const VENUES = ['NeurIPS', 'ICML', 'ICLR', 'ACL', 'EMNLP', 'NAACL', 'CVPR', 'ICCV', 'AAAI', 'IJCAI', 'KDD', 'CHI', 'arXiv preprint', 'Journal article', 'Thesis'];
const SEVERITY_ORDER = ['critical', 'warning', 'suggestion'];

export function ReviewModal(props: ReviewProps) {
  const [job, setJob] = useState<Job | null>(null);
  const [venue, setVenue] = useState(localStorage.getItem('ll-review-venue') ?? '');
  const [focus, setFocus] = useState('');
  const [busy, setBusy] = useState(false);
  const [unavailable, setUnavailable] = useState<string | null>(null);
  const timer = useRef<number>();
  const notified = useRef<string | null>(null);

  const active = job?.status === 'queued' || job?.status === 'running';

  const poll = async () => {
    try {
      const j = await get<Job>(`/api/projects/${props.projectId}/review`);
      setJob(j);
      setUnavailable(null);
      if (j.status === 'done' && j.id && notified.current === 'pending') {
        notified.current = j.id;
        if (j.kind === 'clear') toast(`Removed ${j.result?.removed ?? 0} AI comments`, 'success');
        else toast(`Review finished: ${j.result?.written ?? 0} comments added`, 'success');
        props.onFinished();
      }
    } catch (e: any) {
      setUnavailable(e.message);
    }
  };

  useEffect(() => {
    poll();
    return () => clearTimeout(timer.current);
  }, []);
  useEffect(() => {
    clearTimeout(timer.current);
    if (active) timer.current = window.setTimeout(poll, 2500);
  }, [job]);

  const start = async (kind: 'review' | 'clear') => {
    if (kind === 'clear' && !confirm('Remove every AI comment (\\ANG[AI: …]{…}) from the project? Your own comments are kept.')) return;
    setBusy(true);
    try {
      localStorage.setItem('ll-review-venue', venue);
      const url = kind === 'review' ? `/api/projects/${props.projectId}/review` : `/api/projects/${props.projectId}/review/clear`;
      const j = await post<Job>(url, kind === 'review' ? { venue, focus } : {});
      notified.current = 'pending';
      setJob(j);
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      setBusy(false);
    }
  };

  const r = job?.result;
  const comments = [...(r?.comments ?? [])].sort((a, b) => SEVERITY_ORDER.indexOf(a.severity) - SEVERITY_ORDER.indexOf(b.severity));

  return (
    <Modal title="AI review" onClose={props.onClose} wide>
      <p class="muted">
        A team of AI reviewers reads the whole paper and writes its comments straight into the LaTeX source as{' '}
        <code>\ANG[AI: …]{'{…}'}</code>, so they appear in the PDF next to the text they are about. They arrive as one commit
        in <strong>History</strong>: revert it, remove them below, or hide them all with{' '}
        <code>\usepackage[hide]{'{lightleaf-comments}'}</code>. You can write your own comments the same way: <code>\ANG{'{…}'}</code>.
      </p>
      {unavailable && <div class="alert error">{unavailable}</div>}

      {active ? (
        <div class="alert">
          <Spinner /> {job?.kind === 'clear' ? 'Removing AI comments' : 'Reviewing'}: {job?.stage ?? 'starting'}…
          <div class="muted small">This takes a few minutes. You can close this dialog and keep working; the comments will appear when it is done.</div>
        </div>
      ) : (
        <form class="stack" onSubmit={(e) => { e.preventDefault(); start('review'); }}>
          <label>
            Target venue (optional)
            <input list="ll-venues" placeholder="e.g. NeurIPS" value={venue} onInput={(e) => setVenue(e.currentTarget.value)} />
            <datalist id="ll-venues">{VENUES.map((v) => <option value={v} />)}</datalist>
          </label>
          <label>
            Anything the reviewers should focus on? (optional)
            <textarea rows={2} placeholder="e.g. Is the evaluation convincing? Is section 3 too long?" value={focus} onInput={(e) => setFocus(e.currentTarget.value)} />
          </label>
          <div class="row">
            <button class="btn primary" disabled={busy || !!unavailable}>Start review</button>
            <button type="button" class="btn" disabled={busy || !!unavailable} onClick={() => start('clear')}>Remove AI comments</button>
          </div>
        </form>
      )}

      {job?.status === 'failed' && <div class="alert error">The last {job.kind === 'clear' ? 'removal' : 'review'} failed: {job.error}</div>}

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
          {!!r.failedAgents?.length && (
            <div class="alert">Some reviewers did not finish: {r.failedAgents.map((f) => f.agent).join(', ')}.</div>
          )}
          <ul class="review-list">
            {comments.map((c) => (
              <li class={`review-item sev-${c.severity}`} onClick={() => { props.onOpenFile(c.file); props.onClose(); }}>
                <div class="review-meta"><span class="badge">{c.severity}</span> {c.agent} · <code>{c.file}</code></div>
                <div>{c.text}</div>
                <div class="muted small review-quote">“{c.quote}”</div>
              </li>
            ))}
          </ul>
        </>
      )}
      {job?.status === 'done' && job.kind === 'clear' && <p class="muted">Removed {r?.removed ?? 0} AI comments.</p>}
    </Modal>
  );
}
