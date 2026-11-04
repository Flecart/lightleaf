import { useEffect, useMemo, useRef, useState } from 'preact/hooks';

interface ImportResult {
  id: string;
  mainFile: string;
  compiler: string;
  reason: string;
  note: string | null;
  files: number;
  skipped: number;
}
import { api, del, get, post, Project, PublicConfig, timeAgo, User } from '../api';
import { Icon, Modal, toast, TopBar } from '../ui';
import { UserMenu } from './UserMenu';

type Filter = 'all' | 'mine' | 'shared';

export function ProjectsPage(props: { user: User; config: PublicConfig; onLogout: () => void }) {
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [filter, setFilter] = useState<Filter>('all');
  const [query, setQuery] = useState('');
  const [newOpen, setNewOpen] = useState(false);
  const [creating, setCreating] = useState<null | 'latex' | 'typst' | 'blank'>(null);
  const [importing, setImporting] = useState(false);
  const [uploadOpen, setUploadOpen] = useState(false);
  const [dragging, setDragging] = useState(false);

  const load = () => get<Project[]>('/api/projects').then(setProjects);
  useEffect(() => {
    load();
  }, []);

  const shown = useMemo(() => {
    const q = query.toLowerCase();
    return (projects ?? []).filter(
      (p) =>
        (filter === 'all' || (filter === 'mine' ? p.role === 'owner' : p.role !== 'owner')) &&
        (!q || p.name.toLowerCase().includes(q) || p.owner.name.toLowerCase().includes(q)),
    );
  }, [projects, filter, query]);

  const importZip = async (f: File) => {
    if (!/\.zip$/i.test(f.name)) {
      toast('Please choose a .zip file', 'error');
      return;
    }
    setImporting(true);
    try {
      const fd = new FormData();
      // Overleaf names its exports `Project_Name.zip`.
      fd.append('name', f.name.replace(/\.zip$/i, '').replace(/_/g, ' '));
      fd.append('file', f);
      const r = await api<ImportResult>('POST', '/api/projects/import', fd);
      toast(
        `Imported ${r.files} files · main document ${r.mainFile} · ${r.compiler}` + (r.note ? ` (${r.note})` : ''),
        'success',
      );
      location.hash = `#/project/${r.id}`;
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      setImporting(false);
      setUploadOpen(false);
    }
  };

  const remove = async (p: Project) => {
    const owner = p.role === 'owner';
    if (!confirm(owner ? `Delete “${p.name}” permanently? This cannot be undone.` : `Leave “${p.name}”?`)) return;
    await del(`/api/projects/${p.id}`);
    load();
  };

  const copy = async (p: Project) => {
    const r = await post<{ id: string }>(`/api/projects/${p.id}/copy`, {});
    toast('Project copied', 'success');
    load();
    return r;
  };

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const f = e.dataTransfer?.files[0];
    if (f) importZip(f);
  };

  return (
    <div
      class="page"
      onDragOver={(e) => {
        if (e.dataTransfer?.types.includes('Files')) {
          e.preventDefault();
          setDragging(true);
        }
      }}
      onDragLeave={(e) => e.relatedTarget === null && setDragging(false)}
      onDrop={onDrop}
    >
      <TopBar>
        <UserMenu user={props.user} onLogout={props.onLogout} />
      </TopBar>
      <div class="dashboard">
        <aside class="dash-side">
          <div class="dropdown">
            <button class="btn primary block" onClick={() => setNewOpen(!newOpen)}>
              New Project <Icon name="chevron-down" size={14} />
            </button>
            {newOpen && (
              <div class="menu" onMouseLeave={() => setNewOpen(false)}>
                <button onClick={() => { setCreating('latex'); setNewOpen(false); }}>Example LaTeX project</button>
                <button onClick={() => { setCreating('blank'); setNewOpen(false); }}>Blank LaTeX project</button>
                <button onClick={() => { setCreating('typst'); setNewOpen(false); }}>Typst project</button>
                <hr />
                <button onClick={() => { setUploadOpen(true); setNewOpen(false); }}>Upload project (.zip)</button>
              </div>
            )}
          </div>
          <nav class="dash-filters">
            {(['all', 'mine', 'shared'] as Filter[]).map((f) => (
              <button class={filter === f ? 'active' : ''} onClick={() => setFilter(f)}>
                {f === 'all' ? 'All projects' : f === 'mine' ? 'Your projects' : 'Shared with you'}
              </button>
            ))}
          </nav>
          <div class="dash-foot muted">
            Self-hosted · open source
          </div>
        </aside>
        <main class="dash-main">
          <div class="dash-head">
            <h1>{filter === 'all' ? 'All projects' : filter === 'mine' ? 'Your projects' : 'Shared with you'}</h1>
            <div class="search">
              <Icon name="search" />
              <input placeholder="Search in your projects…" value={query} onInput={(e) => setQuery(e.currentTarget.value)} />
            </div>
          </div>
          {importing && <div class="alert">Importing archive…</div>}
          {projects === null ? (
            <div class="center pad"><span class="spinner" /></div>
          ) : shown.length === 0 ? (
            <div class="empty">
              <p>{projects.length === 0 ? 'You have no projects yet.' : 'No projects match.'}</p>
              {projects.length === 0 && (
                <button class="btn primary" onClick={() => setCreating('latex')}>Create your first project</button>
              )}
            </div>
          ) : (
            <table class="project-table">
              <thead>
                <tr>
                  <th>Title</th>
                  <th>Owner</th>
                  <th>Last modified</th>
                  <th class="right">Actions</th>
                </tr>
              </thead>
              <tbody>
                {shown.map((p) => (
                  <tr key={p.id}>
                    <td>
                      <a class="project-link" href={`#/project/${p.id}`}>{p.name}</a>
                      <span class={`badge ${p.compiler === 'typst' ? 'typst' : 'tex'}`}>{p.compiler === 'typst' ? 'Typst' : 'LaTeX'}</span>
                    </td>
                    <td class="muted">{p.role === 'owner' ? 'You' : p.owner.name}</td>
                    <td class="muted">{timeAgo(p.updatedAt)}</td>
                    <td class="right actions">
                      <button class="icon-btn" title="Copy" onClick={() => copy(p)}><Icon name="copy" /></button>
                      <a class="icon-btn" title="Download source (.zip, Overleaf-compatible)" href={`/api/projects/${p.id}/download`}><Icon name="download" /></a>
                      <button class="icon-btn danger" title={p.role === 'owner' ? 'Delete' : 'Leave'} onClick={() => remove(p)}><Icon name="trash" /></button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </main>
      </div>
      {uploadOpen && <UploadModal busy={importing} onFile={importZip} onClose={() => setUploadOpen(false)} />}
      {dragging && <div class="drop-overlay">Drop a .zip to import it as a new project</div>}
      {creating && <NewProjectModal template={creating} onClose={() => setCreating(null)} />}
    </div>
  );
}

function NewProjectModal(props: { template: string; onClose: () => void }) {
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const submit = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    try {
      const r = await post<{ id: string }>('/api/projects', { name: name || 'Untitled', template: props.template });
      location.hash = `#/project/${r.id}`;
    } catch (err: any) {
      toast(err.message, 'error');
      setBusy(false);
    }
  };
  return (
    <Modal title={props.template === 'typst' ? 'New Typst project' : 'New LaTeX project'} onClose={props.onClose}>
      <form onSubmit={submit} class="stack">
        <input autoFocus placeholder="Project name" value={name} onInput={(e) => setName(e.currentTarget.value)} />
        <div class="row end">
          <button type="button" class="btn" onClick={props.onClose}>Cancel</button>
          <button class="btn primary" disabled={busy}>Create</button>
        </div>
      </form>
    </Modal>
  );
}

function UploadModal(props: { busy: boolean; onFile: (f: File) => void; onClose: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  const [over, setOver] = useState(false);
  return (
    <Modal title="Upload project" onClose={props.onClose}>
      <div
        class={`dropzone ${over ? 'over' : ''}`}
        onClick={() => input.current?.click()}
        onDragOver={(e) => { e.preventDefault(); e.stopPropagation(); setOver(true); }}
        onDragLeave={() => setOver(false)}
        onDrop={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setOver(false);
          const f = e.dataTransfer?.files[0];
          if (f) props.onFile(f);
        }}
      >
        {props.busy ? <span class="spinner" /> : <Icon name="upload" size={28} />}
        <strong>{props.busy ? 'Importing…' : 'Drag a .zip here or click to choose'}</strong>
        <span class="muted small">
          Works with Overleaf “Download → Source” zips, arXiv sources, or any LaTeX/Typst folder. The main document and
          TeX engine are detected automatically.
        </span>
        <input ref={input} type="file" accept=".zip,application/zip" hidden onChange={(e) => e.currentTarget.files?.[0] && props.onFile(e.currentTarget.files[0])} />
      </div>
    </Modal>
  );
}
