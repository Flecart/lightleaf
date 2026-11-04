import { useEffect, useRef, useState } from 'preact/hooks';
import { del, get, Issue, post, Project, timeAgo, User } from '../api';
import { Avatar, Icon, Modal, Spinner, toast } from '../ui';
import { ChatMessage, ProjectSocket } from './socket';

// ---------------------------------------------------------------------------------------------
// Compile logs
// ---------------------------------------------------------------------------------------------
export function LogsPanel(props: { issues: Issue[]; log: string; onJump: (file: string | null, line: number) => void }) {
  const [raw, setRaw] = useState(false);
  const order = { error: 0, warning: 1, typesetting: 2 } as const;
  const sorted = [...props.issues].sort((a, b) => order[a.level] - order[b.level]);
  return (
    <div class="logs">
      <div class="logs-head">
        <strong>Logs</strong>
        <div class="spacer" />
        <button class="btn small" onClick={() => setRaw(!raw)}>{raw ? 'Show issues' : 'Raw log'}</button>
      </div>
      {raw ? (
        <pre class="raw-log">{props.log || 'No log yet.'}</pre>
      ) : sorted.length === 0 ? (
        <div class="pad muted">No errors or warnings. 🎉</div>
      ) : (
        <ul class="issues">
          {sorted.map((is, i) => (
            <li key={i} class={`issue ${is.level}`} onClick={() => is.line && props.onJump(is.file, is.line)}>
              <div class="issue-head">
                <span class="issue-level">{is.level === 'typesetting' ? 'Badbox' : is.level}</span>
                <span class="issue-loc">{is.file ?? ''}{is.line ? `:${is.line}` : ''}</span>
              </div>
              <div class="issue-msg">{is.message}</div>
              {is.context && <pre class="issue-ctx">{is.context}</pre>}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------------------------
interface Commit {
  sha: string;
  author: string;
  email: string;
  time: number;
  message: string;
  stat: string;
}

interface CommitDetail {
  sha: string;
  message: string;
  files: { path: string; added: number | null; removed: number | null }[];
  patch: string;
}

function DiffView({ patch }: { patch: string }) {
  const files = patch.split(/^(?=diff --git )/m).filter(Boolean);
  return (
    <div class="diff">
      {files.map((f, i) => {
        const lines = f.split('\n');
        const name = lines[0]!.replace(/^diff --git a\/(.*) b\/.*$/, '$1');
        const body = lines.slice(lines.findIndex((l) => l.startsWith('@@')));
        return (
          <div class="diff-file" key={i}>
            <div class="diff-name">{name}</div>
            <pre>
              {body.map((l) => (
                <div class={l.startsWith('+') ? 'add' : l.startsWith('-') ? 'del' : l.startsWith('@@') ? 'hunk' : ''}>{l || ' '}</div>
              ))}
            </pre>
          </div>
        );
      })}
    </div>
  );
}

export function HistoryPanel(props: { projectId: string; canWrite: boolean; onClose: () => void }) {
  const [commits, setCommits] = useState<Commit[] | null>(null);
  const [pending, setPending] = useState(false);
  const [sel, setSel] = useState<CommitDetail | null>(null);
  const [label, setLabel] = useState('');

  const load = async () => {
    const r = await get<{ commits: Commit[]; pending: boolean }>(`/api/projects/${props.projectId}/history`);
    setCommits(r.commits);
    setPending(r.pending);
    if (r.commits[0] && !sel) show(r.commits[0].sha);
  };
  useEffect(() => {
    load();
  }, []);

  const show = async (sha: string) => setSel(await get(`/api/projects/${props.projectId}/history/${sha}`));

  const snapshot = async (e: Event) => {
    e.preventDefault();
    const r = await post<{ created: boolean }>(`/api/projects/${props.projectId}/history`, { message: label });
    toast(r.created ? 'Version saved' : 'No changes since the last version', r.created ? 'success' : 'info');
    setLabel('');
    setSel(null);
    load();
  };

  const restore = async (path?: string) => {
    if (!sel) return;
    const what = path ? `“${path}”` : 'the whole project';
    if (!confirm(`Restore ${what} to version ${sel.sha.slice(0, 8)}? Current state stays in history.`)) return;
    await post(`/api/projects/${props.projectId}/history/${sel.sha}/restore`, { path });
    toast('Restored', 'success');
    load();
  };

  return (
    <div class="history">
      <div class="history-list">
        <div class="history-head">
          <strong>History</strong>
          <button class="icon-btn" onClick={props.onClose} title="Back to editor"><Icon name="x" /></button>
        </div>
        {props.canWrite && (
          <form class="snapshot" onSubmit={snapshot}>
            <input placeholder={pending ? 'Describe current changes…' : 'Label this version…'} value={label} onInput={(e) => setLabel(e.currentTarget.value)} />
            <button class="btn small primary" title="Commit current state">Save</button>
          </form>
        )}
        {pending && <div class="pending-note">Unsaved web edits will be committed automatically on the next git pull.</div>}
        {commits === null ? (
          <div class="pad center"><Spinner /></div>
        ) : (
          <ul>
            {commits.map((c) => (
              <li key={c.sha} class={sel?.sha === c.sha ? 'selected' : ''} onClick={() => show(c.sha)}>
                <div class="commit-msg">{c.message}</div>
                <div class="commit-meta">
                  <Avatar name={c.author} /> {c.author} · {timeAgo(c.time)}
                </div>
                {c.stat && <div class="commit-stat">{c.stat}</div>}
              </li>
            ))}
          </ul>
        )}
      </div>
      <div class="history-detail">
        {sel ? (
          <>
            <div class="history-detail-head">
              <div>
                <code>{sel.sha.slice(0, 10)}</code> <span class="pre-wrap">{sel.message}</span>
              </div>
              {props.canWrite && <button class="btn small" onClick={() => restore()}>Restore project to this version</button>}
            </div>
            <div class="history-files">
              {sel.files.map((f) => (
                <div class="history-file" key={f.path}>
                  <span>{f.path}</span>
                  <span class="add">+{f.added ?? 'bin'}</span>
                  <span class="del">−{f.removed ?? 'bin'}</span>
                  {props.canWrite && <button class="link" onClick={() => restore(f.path)}>restore file</button>}
                </div>
              ))}
            </div>
            <DiffView patch={sel.patch} />
          </>
        ) : (
          <div class="pad muted">Select a version to see its changes.</div>
        )}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------------------------
// Git
// ---------------------------------------------------------------------------------------------
export function GitModal(props: { project: Project; onClose: () => void }) {
  const url = `${location.origin}/git/${props.project.id}`;
  const [remote, setRemote] = useState<{ url: string | null; branch: string } | null>(null);
  const [newUrl, setNewUrl] = useState('');
  const [branch, setBranch] = useState('main');
  const [busy, setBusy] = useState('');

  const load = () =>
    get(`/api/projects/${props.project.id}/remote`).then((r) => {
      setRemote(r);
      setBranch(r.branch);
    });
  useEffect(() => {
    load();
  }, []);

  const run = async (what: 'pull' | 'push') => {
    setBusy(what);
    try {
      await post(`/api/projects/${props.project.id}/remote/${what}`);
      toast(what === 'pull' ? 'Pulled changes from remote' : 'Pushed to remote', 'success');
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      setBusy('');
    }
  };

  const save = async (e: Event) => {
    e.preventDefault();
    try {
      await post(`/api/projects/${props.project.id}/remote`, { url: newUrl, branch });
      setNewUrl('');
      load();
      toast('Remote saved', 'success');
    } catch (err: any) {
      toast(err.message, 'error');
    }
  };

  return (
    <Modal title="Git" onClose={props.onClose} wide>
      <h3>Clone with git</h3>
      <p class="muted">
        Every project is a git repository. Clone it, work locally, and push — collaborators see your changes instantly. Use a{' '}
        <a href="#/settings">git token</a> as the password.
      </p>
      <div class="row">
        <pre class="code grow">git clone {url} "{props.project.name}"</pre>
        <button class="btn small" onClick={() => navigator.clipboard.writeText(`git clone ${url}`)}>Copy</button>
      </div>
      <p class="muted small">Web edits are committed automatically when you pull. The default branch is <code>main</code>.</p>

      <h3>Sync with GitHub / GitLab / Gitea</h3>
      {remote?.url ? (
        <div class="row wrap">
          <code class="grow">{remote.url}</code>
          <span class="muted">branch <code>{remote.branch}</code></span>
          <button class="btn small" disabled={!!busy} onClick={() => run('pull')}>{busy === 'pull' ? <Spinner /> : 'Pull'}</button>
          <button class="btn small primary" disabled={!!busy} onClick={() => run('push')}>{busy === 'push' ? <Spinner /> : 'Push'}</button>
        </div>
      ) : (
        <p class="muted">No remote configured.</p>
      )}
      {props.project.role === 'owner' && (
        <form class="stack" onSubmit={save}>
          <div class="row">
            <input class="grow" placeholder="https://<token>@github.com/you/paper.git" value={newUrl} onInput={(e) => setNewUrl(e.currentTarget.value)} />
            <input style={{ width: '110px' }} placeholder="branch" value={branch} onInput={(e) => setBranch(e.currentTarget.value)} />
            <button class="btn">{remote?.url ? 'Change' : 'Set remote'}</button>
          </div>
          <p class="muted small">Include an access token in the URL for private repositories. Leave empty and save to remove the remote.</p>
        </form>
      )}
    </Modal>
  );
}

// ---------------------------------------------------------------------------------------------
// Share
// ---------------------------------------------------------------------------------------------
export function ShareModal(props: { project: Project; onClose: () => void; onChange: () => void }) {
  const [email, setEmail] = useState('');
  const [role, setRole] = useState('editor');
  const isOwner = props.project.role === 'owner';
  const add = async (e: Event) => {
    e.preventDefault();
    try {
      await post(`/api/projects/${props.project.id}/members`, { email, role });
      setEmail('');
      props.onChange();
    } catch (err: any) {
      toast(err.message, 'error');
    }
  };
  return (
    <Modal title="Share project" onClose={props.onClose}>
      {isOwner && (
        <form class="row" onSubmit={add}>
          <input class="grow" type="email" required placeholder="Collaborator's email" value={email} onInput={(e) => setEmail(e.currentTarget.value)} />
          <select value={role} onChange={(e) => setRole(e.currentTarget.value)}>
            <option value="editor">Can edit</option>
            <option value="viewer">Can view</option>
          </select>
          <button class="btn primary">Share</button>
        </form>
      )}
      <ul class="members">
        <li>
          <Avatar name={props.project.owner.name} />
          <div class="grow">
            <div>{props.project.owner.name}</div>
            <div class="muted small">{props.project.owner.email}</div>
          </div>
          <span class="muted">Owner</span>
        </li>
        {(props.project.members ?? []).map((m) => (
          <li key={m.id}>
            <Avatar name={m.name} />
            <div class="grow">
              <div>{m.name}</div>
              <div class="muted small">{m.email}</div>
            </div>
            {isOwner ? (
              <>
                <select value={m.role} onChange={async (e) => {
                  await post(`/api/projects/${props.project.id}/members`, { email: m.email, role: e.currentTarget.value });
                  props.onChange();
                }}>
                  <option value="editor">Can edit</option>
                  <option value="viewer">Can view</option>
                </select>
                <button class="icon-btn danger" title="Remove" onClick={async () => {
                  await del(`/api/projects/${props.project.id}/members/${m.id}`);
                  props.onChange();
                }}><Icon name="x" /></button>
              </>
            ) : (
              <span class="muted">{m.role === 'editor' ? 'Can edit' : 'Can view'}</span>
            )}
          </li>
        ))}
      </ul>
    </Modal>
  );
}

// ---------------------------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------------------------
export function ChatPanel(props: { projectId: string; socket: ProjectSocket; user: User; onClose: () => void; onUnread?: () => void }) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [text, setText] = useState('');
  const list = useRef<HTMLDivElement>(null);
  useEffect(() => {
    get<ChatMessage[]>(`/api/projects/${props.projectId}/chat`).then(setMessages);
    return props.socket.on((m) => {
      if (m.type === 'chat') setMessages((old) => [...old, m.message]);
    });
  }, []);
  useEffect(() => {
    list.current?.scrollTo({ top: list.current.scrollHeight });
  }, [messages]);
  const send = (e: Event) => {
    e.preventDefault();
    if (!text.trim()) return;
    props.socket.sendJson({ type: 'chat', body: text });
    setText('');
  };
  return (
    <div class="chat">
      <div class="chat-head">
        <strong>Chat</strong>
        <button class="icon-btn" onClick={props.onClose}><Icon name="x" /></button>
      </div>
      <div class="chat-list" ref={list}>
        {messages.length === 0 && <div class="muted pad center">No messages yet. Say hi 👋</div>}
        {messages.map((m) => (
          <div key={m.id} class={`chat-msg ${m.userId === props.user.id ? 'mine' : ''}`}>
            <div class="chat-meta">{m.userId === props.user.id ? 'You' : m.name} · {timeAgo(m.createdAt)}</div>
            <div class="chat-body">{m.body}</div>
          </div>
        ))}
      </div>
      <form class="chat-input" onSubmit={send}>
        <input placeholder="Send a message…" value={text} onInput={(e) => setText(e.currentTarget.value)} />
      </form>
    </div>
  );
}
