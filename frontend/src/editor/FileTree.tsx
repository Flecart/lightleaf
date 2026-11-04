import { useMemo, useState } from 'preact/hooks';
import { Entry } from '../api';
import { Icon } from '../ui';

interface Node {
  name: string;
  path: string;
  kind: 'file' | 'dir';
  children: Node[];
}

function buildTree(entries: Entry[]): Node[] {
  const root: Node = { name: '', path: '', kind: 'dir', children: [] };
  const dirs = new Map<string, Node>([['', root]]);
  const ensureDir = (path: string): Node => {
    if (dirs.has(path)) return dirs.get(path)!;
    const i = path.lastIndexOf('/');
    const parent = ensureDir(i < 0 ? '' : path.slice(0, i));
    const n: Node = { name: path.slice(i + 1), path, kind: 'dir', children: [] };
    parent.children.push(n);
    dirs.set(path, n);
    return n;
  };
  for (const e of entries) {
    if (e.kind === 'dir') ensureDir(e.path);
    else {
      const i = e.path.lastIndexOf('/');
      const parent = ensureDir(i < 0 ? '' : e.path.slice(0, i));
      parent.children.push({ name: e.path.slice(i + 1), path: e.path, kind: 'file', children: [] });
    }
  }
  const sort = (n: Node) => {
    n.children.sort((a, b) => (a.kind !== b.kind ? (a.kind === 'dir' ? -1 : 1) : a.name.localeCompare(b.name)));
    n.children.forEach(sort);
  };
  sort(root);
  return root.children;
}

function fileIcon(name: string) {
  return /\.(png|jpe?g|gif|svg|webp|pdf|eps)$/i.test(name) ? 'image' : 'file';
}

export interface TreeActions {
  open(path: string): void;
  newFile(dir: string): void;
  newFolder(dir: string): void;
  rename(path: string): void;
  remove(path: string): void;
  setMain(path: string): void;
  move(from: string, toDir: string): void;
  upload(files: FileList, dir: string): void;
}

interface Props {
  projectId: string;
  entries: Entry[];
  selected: string | null;
  mainFile: string;
  canWrite: boolean;
  presence: Record<string, string[]>; // path -> colors
  actions: TreeActions;
}

export function FileTree(props: Props) {
  const tree = useMemo(() => buildTree(props.entries), [props.entries]);
  const key = `ll-tree-${props.projectId}`;
  const [collapsed, setCollapsed] = useState<Set<string>>(() => new Set(JSON.parse(localStorage.getItem(key) ?? '[]')));
  const [menu, setMenu] = useState<{ x: number; y: number; node: Node } | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [selectedDir, setSelectedDir] = useState('');

  const toggle = (p: string) => {
    const s = new Set(collapsed);
    s.has(p) ? s.delete(p) : s.add(p);
    setCollapsed(s);
    localStorage.setItem(key, JSON.stringify([...s]));
  };

  const currentDir = () => {
    if (selectedDir) return selectedDir;
    const s = props.selected;
    return s && s.includes('/') ? s.slice(0, s.lastIndexOf('/')) : '';
  };

  const onDrop = (e: DragEvent, dir: string) => {
    e.preventDefault();
    e.stopPropagation();
    setDropTarget(null);
    if (!props.canWrite) return;
    const from = e.dataTransfer?.getData('text/x-lightleaf-path');
    if (from) {
      if (from !== dir && !dir.startsWith(from + '/')) props.actions.move(from, dir);
    } else if (e.dataTransfer?.files.length) {
      props.actions.upload(e.dataTransfer.files, dir);
    }
  };

  const renderNode = (n: Node, depth: number) => {
    const isDir = n.kind === 'dir';
    const open = isDir && !collapsed.has(n.path);
    const colors = props.presence[n.path] ?? [];
    return (
      <li key={n.path}>
        <div
          class={`tree-row ${props.selected === n.path ? 'selected' : ''} ${dropTarget === n.path ? 'drop' : ''}`}
          style={{ paddingLeft: `${8 + depth * 14}px` }}
          draggable={props.canWrite}
          onDragStart={(e) => e.dataTransfer?.setData('text/x-lightleaf-path', n.path)}
          onDragOver={(e) => {
            if (!isDir) return;
            e.preventDefault();
            e.stopPropagation();
            setDropTarget(n.path);
          }}
          onDragLeave={() => setDropTarget(null)}
          onDrop={(e) => isDir && onDrop(e, n.path)}
          onClick={() => {
            if (isDir) {
              toggle(n.path);
              setSelectedDir(n.path);
            } else {
              setSelectedDir('');
              props.actions.open(n.path);
            }
          }}
          onContextMenu={(e) => {
            e.preventDefault();
            setMenu({ x: e.clientX, y: e.clientY, node: n });
          }}
        >
          {isDir ? <Icon name={open ? 'chevron-down' : 'chevron'} size={12} /> : <span class="tree-spacer" />}
          <Icon name={isDir ? (open ? 'folder-open' : 'folder') : fileIcon(n.name)} size={15} />
          <span class="tree-name">{n.name}</span>
          {n.path === props.mainFile && <span class="main-badge" title="Main document">main</span>}
          <span class="tree-presence">
            {colors.slice(0, 3).map((c) => (
              <i style={{ background: c }} />
            ))}
          </span>
        </div>
        {isDir && open && <ul>{n.children.map((c) => renderNode(c, depth + 1))}</ul>}
      </li>
    );
  };

  return (
    <div
      class={`file-tree ${dropTarget === '' ? 'drop' : ''}`}
      onDragOver={(e) => {
        e.preventDefault();
        setDropTarget('');
      }}
      onDrop={(e) => onDrop(e, '')}
      onClick={() => setMenu(null)}
    >
      {props.canWrite && (
        <div class="tree-toolbar">
          <button class="icon-btn" title="New file" onClick={() => props.actions.newFile(currentDir())}><Icon name="file-plus" /></button>
          <button class="icon-btn" title="New folder" onClick={() => props.actions.newFolder(currentDir())}><Icon name="folder-plus" /></button>
          <label class="icon-btn" title="Upload files">
            <Icon name="upload" />
            <input type="file" multiple hidden onChange={(e) => {
              const f = e.currentTarget.files;
              if (f?.length) props.actions.upload(f, currentDir());
              e.currentTarget.value = '';
            }} />
          </label>
        </div>
      )}
      <ul class="tree-root">{tree.map((n) => renderNode(n, 0))}</ul>
      {menu && (
        <div class="menu context" style={{ left: `${menu.x}px`, top: `${menu.y}px` }} onMouseLeave={() => setMenu(null)}>
          {menu.node.kind === 'file' && <button onClick={() => props.actions.open(menu.node.path)}>Open</button>}
          {menu.node.kind === 'file' && /\.(tex|typ)$/.test(menu.node.path) && props.canWrite && (
            <button onClick={() => props.actions.setMain(menu.node.path)}>Set as main document</button>
          )}
          {menu.node.kind === 'file' && (
            <a href={`/api/projects/${props.projectId}/file?path=${encodeURIComponent(menu.node.path)}`} download={menu.node.name}>Download</a>
          )}
          {props.canWrite && (
            <>
              {menu.node.kind === 'dir' && <button onClick={() => props.actions.newFile(menu.node.path)}>New file here</button>}
              <button onClick={() => props.actions.rename(menu.node.path)}>Rename / move</button>
              <button class="danger" onClick={() => props.actions.remove(menu.node.path)}>Delete</button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
