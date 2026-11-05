import { useCallback, useEffect, useMemo, useRef, useState } from 'preact/hooks';
import { api, CompileResult, del, Entry, get, patch, post, Project, User } from '../api';
import { Avatar, Icon, Logo, Modal, Spinner, toast } from '../ui';
import { DocBinding, PresenceUser, ProjectSocket } from './socket';
import { CodeEditor, EditorHandle } from './CodeEditor';
import { PdfHighlight, PdfViewer } from './PdfViewer';
import { FileTree, TreeActions } from './FileTree';
import { ChatPanel, GitModal, HistoryPanel, LogsPanel, ShareModal } from './Panels';
import { ReviewEntry } from '../review/ReviewEntry';
import { outline, OutlineItem } from './lang';

const COLORS = ['#e6194b', '#3cb44b', '#4363d8', '#f58231', '#911eb4', '#46a0a0', '#f032e6', '#808000', '#9a6324', '#000075'];

type Layout = 'split' | 'editor' | 'pdf';

function usePref<T>(key: string, init: T): [T, (v: T) => void] {
  const [v, setV] = useState<T>(() => {
    try {
      const s = localStorage.getItem(key);
      return s === null ? init : (JSON.parse(s) as T);
    } catch {
      return init;
    }
  });
  return [
    v,
    (nv: T) => {
      setV(nv);
      localStorage.setItem(key, JSON.stringify(nv));
    },
  ];
}

interface PromptState {
  title: string;
  value: string;
  action: string;
  onSubmit: (v: string) => Promise<void> | void;
}

export function EditorPage(props: { projectId: string; user: User; reviewer?: boolean }) {
  const pid = props.projectId;
  const [project, setProject] = useState<Project | null>(null);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [openPath, setOpenPath] = useState<string | null>(null);
  const [binding, setBinding] = useState<DocBinding | null>(null);
  const [synced, setSynced] = useState(false);
  const [presence, setPresence] = useState<PresenceUser[]>([]);
  const [connected, setConnected] = useState(true);
  const [compiling, setCompiling] = useState(false);
  const [result, setResult] = useState<CompileResult | null>(null);
  const [pdfUrl, setPdfUrl] = useState<string | null>(null);
  const [showLogs, setShowLogs] = useState(false);
  const [highlight, setHighlight] = useState<PdfHighlight | null>(null);
  const [panel, setPanel] = useState<null | 'history'>(null);
  const [chatOpen, setChatOpen] = useState(false);
  const [modal, setModal] = useState<null | 'git' | 'share' | 'menu'>(null);
  const [prompt, setPrompt] = useState<PromptState | null>(null);
  const [outlineItems, setOutline] = useState<OutlineItem[]>([]);
  const [cursorLine, setCursorLine] = useState(1);

  const [layout, setLayout] = usePref<Layout>('ll-layout', 'split');
  const [split, setSplit] = usePref('ll-split', 50);
  const [sideW, setSideW] = usePref('ll-side', 220);
  const [autoCompile, setAutoCompile] = usePref('ll-autocompile', true);
  const [editorDark, setEditorDark] = usePref('ll-editor-dark', document.documentElement.dataset.theme === 'dark');
  const [fontSize, setFontSize] = usePref('ll-font', 14);
  const [outlineOpen, setOutlineOpen] = usePref('ll-outline', true);

  const editorHandle = useRef<EditorHandle | null>(null);
  const socketRef = useRef<ProjectSocket | null>(null);
  const compilingRef = useRef(false);
  const queued = useRef(false);
  const autoTimer = useRef<number | undefined>();
  const bibCache = useRef<{ keys: string[]; at: number }>({ keys: [], at: 0 });
  const entriesRef = useRef<Entry[]>([]);
  entriesRef.current = entries;
  const openPathRef = useRef<string | null>(null);
  openPathRef.current = openPath;
  const autoRef = useRef(autoCompile);
  autoRef.current = autoCompile;

  const canWrite = project?.role === 'owner' || project?.role === 'editor';

  const loadProject = () =>
    get<Project>(`/api/projects/${pid}`).then((p) => {
      setProject(p);
      return p;
    });
  const loadTree = () => get<Entry[]>(`/api/projects/${pid}/tree`).then(setEntries);

  // ---- compile -------------------------------------------------------------------------------
  const compile = useCallback(async () => {
    if (compilingRef.current) {
      queued.current = true;
      return;
    }
    compilingRef.current = true;
    setCompiling(true);
    try {
      const r = await post<CompileResult>(`/api/projects/${pid}/compile`, {});
      setResult(r);
      if (r.pdf) setPdfUrl(r.pdf);
      if (r.status === 'failure' || r.status === 'timeout') setShowLogs(true);
    } catch (e: any) {
      toast(e.message, 'error');
    } finally {
      compilingRef.current = false;
      setCompiling(false);
      if (queued.current) {
        queued.current = false;
        compile();
      }
    }
  }, [pid]);

  const scheduleAuto = () => {
    if (!autoRef.current) return;
    clearTimeout(autoTimer.current);
    autoTimer.current = window.setTimeout(() => compile(), 2200);
  };

  // ---- file opening ---------------------------------------------------------------------------
  const isText = (path: string) => entriesRef.current.find((e) => e.path === path)?.text ?? false;

  const openFile = (path: string | null) => {
    const sock = socketRef.current;
    if (!sock) return;
    const prev = openPathRef.current;
    if (prev && prev !== path) sock.close(prev);
    setOpenPath(path);
    localStorage.setItem(`ll-open-${pid}`, path ?? '');
    if (!path || !isText(path)) {
      setBinding(null);
      return;
    }
    const b = sock.open(path);
    setSynced(b.synced);
    setBinding(b);
    b.whenSynced().then(() => {
      if (openPathRef.current === path) {
        setSynced(true);
        setOutline(outline(path, b.text.toString()));
      }
    });
    let t: number | undefined;
    b.onChange = () => {
      scheduleAuto();
      clearTimeout(t);
      t = window.setTimeout(() => setOutline(outline(path, b.text.toString())), 600);
    };
  };

  // ---- mount ---------------------------------------------------------------------------------
  useEffect(() => {
    const sock = new ProjectSocket(pid);
    sock.user = { name: props.user.name, color: COLORS[props.user.id % COLORS.length]! };
    socketRef.current = sock;
    const off = sock.on((m) => {
      switch (m.type) {
        case 'connection':
          setConnected(m.connected);
          break;
        case 'presence':
          setPresence(m.users);
          break;
        case 'tree':
          loadTree();
          break;
        case 'project':
          loadProject();
          break;
        case 'renamed': {
          const cur = openPathRef.current;
          loadTree().then(() => {
            if (cur && (cur === m.from || cur.startsWith(m.from + '/'))) {
              sock.drop(cur);
              openPathRef.current = null;
              openFile(m.to + cur.slice(m.from.length));
            }
          });
          break;
        }
        case 'deleted': {
          const cur = openPathRef.current;
          if (cur && (cur === m.path || cur.startsWith(m.path + '/'))) {
            sock.drop(cur);
            openPathRef.current = null;
            setBinding(null);
            setOpenPath(null);
          }
          break;
        }
        case 'project-deleted':
          toast('This project was deleted', 'error');
          location.hash = '#/';
          break;
        case 'error':
          if (m.error === 'read-only access') toast('You have read-only access to this project', 'error');
          break;
      }
    });
    Promise.all([loadProject(), loadTree()]).then(([p]) => {
      const last = localStorage.getItem(`ll-open-${pid}`);
      const first = last && entriesRef.current.some((e) => e.path === last) ? last : p.mainFile;
      openFile(entriesRef.current.some((e) => e.path === first) ? first : null);
      // Show the last successful PDF immediately, then compile fresh.
      fetch(`/api/projects/${pid}/output/output.pdf`, { method: 'HEAD' }).then((r) => {
        if (r.ok) setPdfUrl(`/api/projects/${pid}/output/output.pdf?t=${Date.now()}`);
      });
      compile();
    });
    const keys = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 's') {
        e.preventDefault();
        compile();
      }
    };
    window.addEventListener('keydown', keys);
    return () => {
      off();
      sock.destroy();
      window.removeEventListener('keydown', keys);
      clearTimeout(autoTimer.current);
    };
  }, [pid]);

  // ---- references for autocomplete ------------------------------------------------------------
  const refs = useMemo(
    () => ({
      labels: () => {
        const t = socketRef.current?.get(openPathRef.current ?? '')?.text.toString() ?? '';
        return [...new Set([...t.matchAll(/\\label\{([^}]+)\}/g)].map((m) => m[1]!))];
      },
      citations: () => {
        const c = bibCache.current;
        if (Date.now() - c.at > 30000) {
          c.at = Date.now();
          const bibs = entriesRef.current.filter((e) => e.path.endsWith('.bib'));
          Promise.all(bibs.map((b) => fetch(`/api/projects/${pid}/file?path=${encodeURIComponent(b.path)}`).then((r) => r.text()))).then(
            (texts) => {
              c.keys = [...new Set(texts.flatMap((t) => [...t.matchAll(/@\w+\s*\{\s*([^,\s]+)\s*,/g)].map((m) => m[1]!)))];
            },
          );
        }
        return c.keys;
      },
    }),
    [pid],
  );

  // ---- tree actions --------------------------------------------------------------------------
  const join = (dir: string, name: string) => (dir ? `${dir}/${name}` : name);
  const actions: TreeActions = {
    open: openFile,
    newFile: (dir) =>
      setPrompt({
        title: 'New file',
        value: project?.compiler === 'typst' ? 'chapter.typ' : 'chapter.tex',
        action: 'Create',
        onSubmit: async (name) => {
          const r = await post<{ path: string }>(`/api/projects/${pid}/files`, { path: join(dir, name), kind: 'file', content: '' });
          await loadTree();
          openFile(r.path);
        },
      }),
    newFolder: (dir) =>
      setPrompt({
        title: 'New folder',
        value: '',
        action: 'Create',
        onSubmit: async (name) => {
          await post(`/api/projects/${pid}/files`, { path: join(dir, name), kind: 'dir' });
          loadTree();
        },
      }),
    rename: (path) =>
      setPrompt({
        title: 'Rename or move',
        value: path,
        action: 'Rename',
        onSubmit: async (to) => {
          if (to !== path) await post(`/api/projects/${pid}/rename`, { from: path, to });
        },
      }),
    remove: async (path) => {
      if (!confirm(`Delete “${path}”?`)) return;
      await del(`/api/projects/${pid}/file?path=${encodeURIComponent(path)}`);
    },
    setMain: async (path) => {
      await patch(`/api/projects/${pid}`, { mainFile: path, compiler: path.endsWith('.typ') ? 'typst' : project?.compiler === 'typst' ? 'pdflatex' : undefined });
      await loadProject();
      toast(`Main document: ${path}`, 'success');
    },
    move: async (from, toDir) => {
      const name = from.split('/').pop()!;
      const to = join(toDir, name);
      if (to !== from) await post(`/api/projects/${pid}/rename`, { from, to }).catch((e) => toast(e.message, 'error'));
    },
    upload: async (files, dir) => {
      const fd = new FormData();
      fd.append('folder', dir);
      for (const f of Array.from(files)) fd.append('file', f, f.name);
      try {
        await api('POST', `/api/projects/${pid}/upload`, fd);
        toast(`Uploaded ${files.length} file(s)`, 'success');
        loadTree();
      } catch (e: any) {
        toast(e.message, 'error');
      }
    },
  };

  // ---- synctex -------------------------------------------------------------------------------
  const jumpTo = (file: string | null, line: number) => {
    const target = file ?? project?.mainFile ?? null;
    if (!target) return;
    if (target !== openPathRef.current) {
      openFile(target);
      setTimeout(() => socketRef.current?.get(target)?.whenSynced().then(() => setTimeout(() => editorHandle.current?.gotoLine(line), 50)), 0);
    } else editorHandle.current?.gotoLine(line);
  };

  const syncToCode = async (page: number, x: number, y: number) => {
    if (!result?.synctex) return;
    try {
      const r = await get<{ file: string; line: number }>(`/api/projects/${pid}/synctex/pdf?page=${page}&h=${x}&v=${y}`);
      if (entriesRef.current.some((e) => e.path === r.file)) jumpTo(r.file, r.line);
    } catch {
      /* no mapping */
    }
  };

  const syncToPdf = async () => {
    if (!openPath || !result?.synctex) return;
    try {
      const line = editorHandle.current?.cursorLine() ?? cursorLine;
      const r = await get<any>(`/api/projects/${pid}/synctex/code?file=${encodeURIComponent(openPath)}&line=${line}`);
      setHighlight({ page: r.page, h: r.h, v: r.v, w: r.width ?? 100, hh: r.height ?? 12, nonce: Date.now() });
    } catch {
      toast('No PDF location for this line', 'info');
    }
  };

  // ---- splitters -----------------------------------------------------------------------------
  const dragSplit = (e: PointerEvent, kind: 'split' | 'side') => {
    e.preventDefault();
    const container = (e.currentTarget as HTMLElement).parentElement!;
    const rect = container.getBoundingClientRect();
    document.body.classList.add('dragging');
    const move = (ev: PointerEvent) => {
      if (kind === 'side') setSideW(Math.max(140, Math.min(480, ev.clientX - rect.left)));
      else setSplit(Math.max(15, Math.min(85, ((ev.clientX - rect.left) / rect.width) * 100)));
    };
    const up = () => {
      document.body.classList.remove('dragging');
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  if (!project) return <div class="splash"><div class="spinner" /></div>;

  const errors = result?.issues.filter((i) => i.level === 'error').length ?? 0;
  const warnings = result?.issues.filter((i) => i.level !== 'error').length ?? 0;
  const others = presence.filter((p) => p.clientId !== socketRef.current?.clientId);
  const uniqueOthers = [...new Map(others.map((p) => [p.userId, p])).values()];
  const presenceByPath: Record<string, string[]> = {};
  for (const p of others) if (p.path) (presenceByPath[p.path] ??= []).push(p.color);
  const mainCandidates = entries.filter((e) => e.kind === 'file' && /\.(tex|typ)$/.test(e.path));
  const isImage = openPath && /\.(png|jpe?g|gif|svg|webp)$/i.test(openPath);
  const isPdf = openPath && /\.pdf$/i.test(openPath);
  const fileUrl = openPath ? `/api/projects/${pid}/file?path=${encodeURIComponent(openPath)}` : '';

  return (
    <div class="editor-page">
      <header class="ed-toolbar">
        <div class="ed-left">
          <button class="tb-btn" onClick={() => setModal('menu')} title="Menu"><Icon name="menu" /> <span class="hide-sm">Menu</span></button>
          <a class="tb-btn" href="#/" title="Back to projects"><Logo size={20} /></a>
        </div>
        <div class="ed-title" title={project.name}>
          {project.name}
          {!connected && <span class="offline">Reconnecting…</span>}
        </div>
        <div class="ed-right">
          <div class="presence">
            {uniqueOthers.slice(0, 5).map((p) => (
              <span onClick={() => p.path && openFile(p.path)} style={{ cursor: p.path ? 'pointer' : 'default' }}>
                <Avatar name={p.name} color={p.color} title={`${p.name}${p.path ? ` — ${p.path}` : ''}`} />
              </span>
            ))}
            {uniqueOthers.length > 5 && <span class="avatar more">+{uniqueOthers.length - 5}</span>}
          </div>
          <button class="tb-btn" onClick={() => setModal('share')}><Icon name="users" /> <span class="hide-sm">Share</span></button>
          <button class={`tb-btn ${panel === 'history' ? 'active' : ''}`} onClick={() => setPanel(panel === 'history' ? null : 'history')}><Icon name="history" /> <span class="hide-sm">History</span></button>
          <button class="tb-btn" onClick={() => setModal('git')}><Icon name="git" /> <span class="hide-sm">Git</span></button>
          {props.reviewer && canWrite && <ReviewEntry projectId={pid} onFinished={compile} onOpenFile={openFile} />}
          <button class={`tb-btn ${chatOpen ? 'active' : ''}`} onClick={() => setChatOpen(!chatOpen)}><Icon name="chat" /> <span class="hide-sm">Chat</span></button>
          <div class="segmented dark">
            <button class={layout === 'editor' ? 'active' : ''} title="Editor only" onClick={() => setLayout('editor')}><Icon name="edit" /></button>
            <button class={layout === 'split' ? 'active' : ''} title="Editor & PDF" onClick={() => setLayout('split')}><Icon name="layout" /></button>
            <button class={layout === 'pdf' ? 'active' : ''} title="PDF only" onClick={() => setLayout('pdf')}><Icon name="file" /></button>
          </div>
        </div>
      </header>

      <div class="ed-body">
        <aside class="ed-side" style={{ width: `${sideW}px` }}>
          <FileTree projectId={pid} entries={entries} selected={openPath} mainFile={project.mainFile} canWrite={canWrite} presence={presenceByPath} actions={actions} />
          {outlineItems.length > 0 && (
            <div class={`outline ${outlineOpen ? 'open' : ''}`}>
              <button class="outline-head" onClick={() => setOutlineOpen(!outlineOpen)}>
                <Icon name={outlineOpen ? 'chevron-down' : 'chevron'} size={12} /> File outline
              </button>
              {outlineOpen && (
                <ul>
                  {outlineItems.map((o) => {
                    const active = outlineItems.filter((x) => x.line <= cursorLine).pop() === o;
                    return (
                      <li class={active ? 'active' : ''} style={{ paddingLeft: `${10 + o.level * 10}px` }} onClick={() => editorHandle.current?.gotoLine(o.line)}>
                        {o.title}
                      </li>
                    );
                  })}
                </ul>
              )}
            </div>
          )}
        </aside>
        <div class="splitter" onPointerDown={(e) => dragSplit(e as PointerEvent, 'side')} />

        <div class="ed-main">
          {panel === 'history' ? (
            <HistoryPanel projectId={pid} canWrite={canWrite} onClose={() => setPanel(null)} />
          ) : (
            <div class="panes">
              {layout !== 'pdf' && (
                <section class="pane editor-pane" style={layout === 'split' ? { width: `${split}%` } : { flex: 1 }}>
                  <div class="pane-head">
                    <span class="crumb">{openPath ?? 'No file open'}</span>
                    {!canWrite && <span class="badge">read only</span>}
                  </div>
                  {binding && synced ? (
                    <CodeEditor
                      binding={binding}
                      readOnly={!canWrite}
                      dark={editorDark}
                      fontSize={fontSize}
                      refs={refs}
                      issues={result?.issues ?? []}
                      onCompile={compile}
                      onCursor={setCursorLine}
                      handle={editorHandle}
                    />
                  ) : binding ? (
                    <div class="pad center"><Spinner /></div>
                  ) : isImage ? (
                    <div class="preview"><img src={fileUrl} alt={openPath ?? ''} /></div>
                  ) : isPdf ? (
                    <iframe class="preview-frame" src={fileUrl} />
                  ) : openPath ? (
                    <div class="pad muted">Binary file. <a href={fileUrl} download>Download</a></div>
                  ) : (
                    <div class="pad muted">Select a file from the tree.</div>
                  )}
                </section>
              )}
              {layout === 'split' && <div class="splitter" onPointerDown={(e) => dragSplit(e as PointerEvent, 'split')} />}
              {layout !== 'editor' && (
                <section class="pane pdf-pane" style={{ flex: 1 }}>
                  <div class="pane-head pdf-head">
                    <div class="recompile">
                      <button class={`btn recompile-btn ${compiling ? 'busy' : ''}`} onClick={compile} disabled={compiling} title="Recompile (Ctrl+S)">
                        {compiling ? <Spinner /> : <Icon name="refresh" />} {compiling ? 'Compiling…' : 'Recompile'}
                      </button>
                      <label class="auto-toggle" title="Recompile automatically after edits">
                        <input type="checkbox" checked={autoCompile} onChange={(e) => setAutoCompile(e.currentTarget.checked)} /> Auto
                      </label>
                    </div>
                    {result?.synctex && openPath && binding && (
                      <button class="icon-btn" title="Jump to location in PDF (SyncTeX)" onClick={syncToPdf}><Icon name="arrow-right" /></button>
                    )}
                    <button class={`btn small logs-btn ${showLogs ? 'active' : ''}`} onClick={() => setShowLogs(!showLogs)}>
                      <Icon name="log" /> Logs
                      {errors > 0 && <span class="count error">{errors}</span>}
                      {warnings > 0 && <span class="count warn">{warnings}</span>}
                    </button>
                    <div class="spacer" />
                    {result && <span class="muted small hide-sm">{(result.durationMs / 1000).toFixed(1)}s</span>}
                    {pdfUrl && (
                      <a class="icon-btn" title="Download PDF" href={`/api/projects/${pid}/output/output.pdf`} download={`${project.name}.pdf`}><Icon name="download" /></a>
                    )}
                  </div>
                  {showLogs ? (
                    <LogsPanel issues={result?.issues ?? []} log={result?.log ?? ''} onJump={(f, l) => { jumpTo(f, l); }} />
                  ) : (
                    <>
                      {result && (result.status === 'failure' || result.status === 'timeout') && (
                        <div class="compile-banner" onClick={() => setShowLogs(true)}>
                          <Icon name="alert" /> {result.status === 'timeout' ? 'Compilation timed out.' : 'Compilation failed.'} Click to see the logs.
                        </div>
                      )}
                      <PdfViewer url={pdfUrl} highlight={highlight} onDoubleClick={syncToCode} />
                    </>
                  )}
                </section>
              )}
            </div>
          )}
        </div>
        {chatOpen && socketRef.current && (
          <aside class="ed-chat">
            <ChatPanel projectId={pid} socket={socketRef.current} user={props.user} onClose={() => setChatOpen(false)} />
          </aside>
        )}
      </div>

      {modal === 'git' && <GitModal project={project} onClose={() => setModal(null)} />}
      {modal === 'share' && <ShareModal project={project} onClose={() => setModal(null)} onChange={loadProject} />}
      {modal === 'menu' && (
        <div class="drawer-backdrop" onClick={(e) => e.target === e.currentTarget && setModal(null)}>
          <aside class="drawer">
            <h3>Download</h3>
            <p class="muted small">The source zip has the same layout as Overleaf's, so it can be uploaded there directly.</p>
            <div class="drawer-row">
              <a class="btn" href={`/api/projects/${pid}/download`}><Icon name="download" /> Source (.zip)</a>
              {pdfUrl && <a class="btn" href={`/api/projects/${pid}/output/output.pdf`} download={`${project.name}.pdf`}><Icon name="download" /> PDF</a>}
            </div>
            <h3>Actions</h3>
            <div class="drawer-col">
              <button class="btn" onClick={async () => {
                const r = await post<{ id: string }>(`/api/projects/${pid}/copy`, {});
                location.hash = `#/project/${r.id}`;
              }}><Icon name="copy" /> Copy project</button>
              {project.role === 'owner' && (
                <button class="btn" onClick={() => setPrompt({
                  title: 'Rename project', value: project.name, action: 'Rename',
                  onSubmit: async (name) => { await patch(`/api/projects/${pid}`, { name }); loadProject(); },
                })}><Icon name="edit" /> Rename project</button>
              )}
            </div>
            <h3>Settings</h3>
            <div class="settings-grid">
              <label>Compiler</label>
              <select disabled={!canWrite} value={project.compiler} onChange={async (e) => { await patch(`/api/projects/${pid}`, { compiler: e.currentTarget.value }); loadProject(); }}>
                {['pdflatex', 'xelatex', 'lualatex', 'tectonic', 'folio', 'typst'].map((c) => (
                  <option value={c} disabled={!project.compilers?.includes(c)}>{c}{project.compilers?.includes(c) ? '' : ' (not installed)'}</option>
                ))}
              </select>
              <label>Main document</label>
              <select disabled={!canWrite} value={project.mainFile} onChange={(e) => actions.setMain(e.currentTarget.value)}>
                {mainCandidates.map((f) => <option value={f.path}>{f.path}</option>)}
              </select>
              <label>Editor theme</label>
              <select value={editorDark ? 'dark' : 'light'} onChange={(e) => setEditorDark(e.currentTarget.value === 'dark')}>
                <option value="light">Light</option>
                <option value="dark">Dark</option>
              </select>
              <label>Font size</label>
              <select value={fontSize} onChange={(e) => setFontSize(Number(e.currentTarget.value))}>
                {[11, 12, 13, 14, 15, 16, 18, 20, 24].map((s) => <option value={s}>{s}px</option>)}
              </select>
              <label>Auto compile</label>
              <select value={autoCompile ? 'on' : 'off'} onChange={(e) => setAutoCompile(e.currentTarget.value === 'on')}>
                <option value="on">On</option>
                <option value="off">Off</option>
              </select>
            </div>
            <p class="muted small">Shortcuts: <kbd>Ctrl</kbd>+<kbd>S</kbd> / <kbd>Ctrl</kbd>+<kbd>Enter</kbd> recompile · <kbd>Ctrl</kbd>+<kbd>F</kbd> search · double-click the PDF to jump to the source.</p>
          </aside>
        </div>
      )}
      {prompt && <PromptModal state={prompt} onClose={() => setPrompt(null)} />}
    </div>
  );
}

function PromptModal(props: { state: PromptState; onClose: () => void }) {
  const [v, setV] = useState(props.state.value);
  const [busy, setBusy] = useState(false);
  const submit = async (e: Event) => {
    e.preventDefault();
    if (!v.trim()) return;
    setBusy(true);
    try {
      await props.state.onSubmit(v.trim());
      props.onClose();
    } catch (err: any) {
      toast(err.message, 'error');
      setBusy(false);
    }
  };
  return (
    <Modal title={props.state.title} onClose={props.onClose}>
      <form class="stack" onSubmit={submit}>
        <input autoFocus value={v} onInput={(e) => setV(e.currentTarget.value)} onFocus={(e) => {
          const el = e.currentTarget;
          const dot = el.value.lastIndexOf('.');
          el.setSelectionRange(el.value.lastIndexOf('/') + 1, dot > 0 ? dot : el.value.length);
        }} />
        <div class="row end">
          <button type="button" class="btn" onClick={props.onClose}>Cancel</button>
          <button class="btn primary" disabled={busy}>{props.state.action}</button>
        </div>
      </form>
    </Modal>
  );
}
