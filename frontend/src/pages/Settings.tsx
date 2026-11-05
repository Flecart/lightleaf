import { useEffect, useState } from 'preact/hooks';
import { del, get, post, PublicConfig, timeAgo, User } from '../api';
import { Icon, toast, TopBar } from '../ui';
import { UserMenu } from './UserMenu';

interface Session {
  id: number;
  createdAt: number;
  lastSeen: number;
  ip: string;
  userAgent: string;
  current: boolean;
}

/** A short, readable description of a browser from its user-agent string. */
function describeAgent(ua: string): string {
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox' : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : '';
  const os = /Windows/.test(ua) ? 'Windows' : /Android/.test(ua) ? 'Android' : /iPhone|iPad/.test(ua) ? 'iOS' : /Mac OS X/.test(ua) ? 'macOS' : /Linux/.test(ua) ? 'Linux' : '';
  return [browser, os].filter(Boolean).join(' on ') || ua || 'Unknown browser';
}

interface Token {
  id: number;
  name: string;
  createdAt: number;
  lastUsed: number | null;
}

export function SettingsPage(props: { user: User; config: PublicConfig; onLogout: () => void }) {
  const [tokens, setTokens] = useState<Token[]>([]);
  const [newToken, setNewToken] = useState<string | null>(null);
  const [tokenName, setTokenName] = useState('');
  const [cur, setCur] = useState('');
  const [pw, setPw] = useState('');
  const [theme, setTheme] = useState(localStorage.getItem('ll-theme') ?? 'auto');

  const [sessions, setSessions] = useState<Session[]>([]);
  const load = () => get<Token[]>('/api/me/tokens').then(setTokens);
  const loadSessions = () => get<Session[]>('/api/me/sessions').then(setSessions);
  useEffect(() => {
    load();
    loadSessions();
  }, []);

  const create = async (e: Event) => {
    e.preventDefault();
    const r = await post<{ token: string }>('/api/me/tokens', { name: tokenName || 'git' });
    setNewToken(r.token);
    setTokenName('');
    load();
  };

  const changePw = async (e: Event) => {
    e.preventDefault();
    try {
      await post('/api/me/password', { current: cur, new: pw });
      toast('Password updated — other browsers were logged out', 'success');
      setCur('');
      setPw('');
      loadSessions();
    } catch (err: any) {
      toast(err.message, 'error');
    }
  };

  const setThemePref = (t: string) => {
    setTheme(t);
    localStorage.setItem('ll-theme', t);
    (window as any).__applyTheme();
  };

  return (
    <div class="page">
      <TopBar title="Account settings">
        <UserMenu user={props.user} sourceUrl={props.config.sourceUrl} onLogout={props.onLogout} />
      </TopBar>
      <div class="settings">
        <section class="card">
          <h2><Icon name="git" /> Git authentication tokens</h2>
          <p class="muted">
            Use a token as the password when cloning, pulling or pushing projects with git. The username can be anything
            (for example <code>git</code>).
          </p>
          <pre class="code">git clone {location.origin}/git/&lt;project-id&gt;</pre>
          {newToken && (
            <div class="alert success">
              <strong>Copy your new token now — it won't be shown again:</strong>
              <div class="row">
                <code class="token">{newToken}</code>
                <button class="btn small" onClick={() => navigator.clipboard.writeText(newToken)}>Copy</button>
              </div>
            </div>
          )}
          <form class="row" onSubmit={create}>
            <input placeholder="Token name (e.g. laptop)" value={tokenName} onInput={(e) => setTokenName(e.currentTarget.value)} />
            <button class="btn primary">Generate token</button>
          </form>
          {tokens.length > 0 && (
            <table class="simple-table">
              <thead><tr><th>Name</th><th>Created</th><th>Last used</th><th /></tr></thead>
              <tbody>
                {tokens.map((t) => (
                  <tr key={t.id}>
                    <td>{t.name}</td>
                    <td class="muted">{timeAgo(t.createdAt)}</td>
                    <td class="muted">{t.lastUsed ? timeAgo(t.lastUsed) : 'never'}</td>
                    <td class="right">
                      <button class="icon-btn danger" title="Revoke" onClick={async () => { await del(`/api/me/tokens/${t.id}`); load(); }}>
                        <Icon name="trash" />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <section class="card">
          <h2><Icon name="settings" /> Appearance</h2>
          <div class="segmented">
            {['auto', 'light', 'dark'].map((t) => (
              <button class={theme === t ? 'active' : ''} onClick={() => setThemePref(t)}>{t[0]!.toUpperCase() + t.slice(1)}</button>
            ))}
          </div>
        </section>

        <section class="card">
          <h2><Icon name="shield" /> Active sessions</h2>
          <p class="muted">Browsers where you are logged in. Inactive sessions are logged out automatically.</p>
          <table class="simple-table">
            <thead><tr><th>Browser</th><th>IP address</th><th>Signed in</th><th>Last active</th><th /></tr></thead>
            <tbody>
              {sessions.map((s) => (
                <tr key={s.id}>
                  <td title={s.userAgent}>{describeAgent(s.userAgent)} {s.current && <span class="badge">this browser</span>}</td>
                  <td class="muted">{s.ip || '–'}</td>
                  <td class="muted">{timeAgo(s.createdAt)}</td>
                  <td class="muted">{timeAgo(s.lastSeen)}</td>
                  <td class="right">
                    {!s.current && (
                      <button class="icon-btn danger" title="Log out this session" onClick={async () => { await del(`/api/me/sessions/${s.id}`); loadSessions(); }}>
                        <Icon name="logout" />
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          {sessions.length > 1 && (
            <div>
              <button class="btn" onClick={async () => { await del('/api/me/sessions'); toast('Logged out other sessions', 'success'); loadSessions(); }}>
                Log out all other sessions
              </button>
            </div>
          )}
        </section>

        <section class="card">
          <h2><Icon name="key" /> Change password</h2>
          <form class="stack narrow" onSubmit={changePw}>
            <input type="password" autoComplete="current-password" placeholder="Current password" required value={cur} onInput={(e) => setCur(e.currentTarget.value)} />
            <input type="password" autoComplete="new-password" placeholder="New password (min. 8 characters)" minLength={8} required value={pw} onInput={(e) => setPw(e.currentTarget.value)} />
            <div><button class="btn primary">Update password</button></div>
          </form>
        </section>
      </div>
    </div>
  );
}
