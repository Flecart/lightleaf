import { render } from 'preact';
import { useEffect, useState } from 'preact/hooks';
import { get, PublicConfig, User } from './api';
import { AuthPage } from './pages/Auth';
import { ProjectsPage } from './pages/Projects';
import { SettingsPage } from './pages/Settings';
import { AdminPage } from './pages/Admin';
import { EditorPage } from './editor/EditorPage';
import { registerToastHost } from './ui';
import './styles.css';

function useHash() {
  const [hash, setHash] = useState(location.hash.slice(1) || '/');
  useEffect(() => {
    const on = () => setHash(location.hash.slice(1) || '/');
    window.addEventListener('hashchange', on);
    return () => window.removeEventListener('hashchange', on);
  }, []);
  return hash;
}

function applyTheme() {
  const t = localStorage.getItem('ll-theme') ?? 'auto';
  const dark = t === 'dark' || (t === 'auto' && matchMedia('(prefers-color-scheme: dark)').matches);
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
}
applyTheme();
matchMedia('(prefers-color-scheme: dark)').addEventListener('change', applyTheme);
(window as any).__applyTheme = applyTheme;

function App() {
  const hash = useHash();
  const [user, setUser] = useState<User | null | undefined>(undefined);
  const [config, setConfig] = useState<PublicConfig | null>(null);

  const refresh = () =>
    get<User>('/api/me')
      .then(setUser)
      .catch(() => setUser(null));

  useEffect(() => {
    get<PublicConfig>('/api/config').then((c) => {
      setConfig(c);
      document.title = c.siteName;
    });
    refresh();
  }, []);

  if (user === undefined || !config) return <div class="splash"><div class="spinner" /></div>;

  if (!user || hash === '/login' || hash === '/register') {
    if (user && (hash === '/login' || hash === '/register')) {
      location.hash = '#/';
      return null;
    }
    return <AuthPage config={config} mode={hash === '/register' || (config.firstUser && config.signup) ? 'register' : 'login'} onAuth={refresh} />;
  }

  const m = hash.match(/^\/project\/([0-9a-f]+)/);
  if (m) return <EditorPage key={m[1]} projectId={m[1]!} user={user} reviewer={config.reviewer} />;
  if (hash === '/settings') return <SettingsPage user={user} config={config} onLogout={() => setUser(null)} />;
  if (hash === '/admin' && user.is_admin) return <AdminPage user={user} config={config} />;
  return <ProjectsPage user={user} config={config} onLogout={() => setUser(null)} />;
}

function Toaster() {
  const [items, setItems] = useState<{ id: number; msg: string; kind: string }[]>([]);
  useEffect(() => {
    let n = 0;
    registerToastHost((msg, kind = 'info') => {
      const id = ++n;
      setItems((old) => [...old.slice(-3), { id, msg, kind }]);
      setTimeout(() => setItems((old) => old.filter((t) => t.id !== id)), kind === 'error' ? 6000 : 3000);
    });
  }, []);
  return (
    <div class="toasts">
      {items.map((t) => (
        <div key={t.id} class={`toast ${t.kind}`} onClick={() => setItems((o) => o.filter((x) => x.id !== t.id))}>{t.msg}</div>
      ))}
    </div>
  );
}

render(
  <>
    <App />
    <Toaster />
  </>,
  document.getElementById('app')!,
);
