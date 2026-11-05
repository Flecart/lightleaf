import { useEffect, useState } from 'preact/hooks';
import { del, get, patch, post, PublicConfig, timeAgo, User } from '../api';
import { Icon, toast, TopBar } from '../ui';
import { UserMenu } from './UserMenu';

interface AdminUser {
  id: number;
  email: string;
  name: string;
  isAdmin: boolean;
  createdAt: number;
  projects: number;
}

export function AdminPage(props: { user: User; config: PublicConfig }) {
  const [users, setUsers] = useState<AdminUser[]>([]);
  const [form, setForm] = useState({ email: '', name: '', password: '', is_admin: false });
  const [reset, setReset] = useState<{ email: string; password: string } | null>(null);
  const load = () => get<AdminUser[]>('/api/admin/users').then(setUsers);
  useEffect(() => {
    load();
  }, []);

  const create = async (e: Event) => {
    e.preventDefault();
    try {
      await post('/api/admin/users', form);
      setForm({ email: '', name: '', password: '', is_admin: false });
      toast('User created', 'success');
      load();
    } catch (err: any) {
      toast(err.message, 'error');
    }
  };

  return (
    <div class="page">
      <TopBar title="Administration">
        <UserMenu user={props.user} sourceUrl={props.config.sourceUrl} onLogout={() => (location.hash = '#/login')} />
      </TopBar>
      <div class="settings">
        <section class="card">
          <h2><Icon name="users" /> Users</h2>
          <p class="muted">
            Public sign-up is <strong>{props.config.signup ? 'enabled' : 'disabled'}</strong> (<code>LIGHTLEAF_SIGNUP</code> is{' '}
            <code>open</code>, <code>local</code> or <code>closed</code>). Accounts can also be managed on the server with{' '}
            <code>lightleaf user …</code>.
            Installed compilers: {props.config.compilers.join(', ') || 'none'}.
          </p>
          {reset && (
            <div class="alert success">
              <strong>New password for {reset.email}</strong> — share it securely; it won't be shown again. They were logged out everywhere.
              <div class="row">
                <code class="token">{reset.password}</code>
                <button class="btn small" onClick={() => navigator.clipboard.writeText(reset.password)}>Copy</button>
                <button class="btn small" onClick={() => setReset(null)}>Dismiss</button>
              </div>
            </div>
          )}
          <table class="simple-table">
            <thead><tr><th>Name</th><th>Email</th><th>Projects</th><th>Joined</th><th /></tr></thead>
            <tbody>
              {users.map((u) => (
                <tr key={u.id}>
                  <td>{u.name} {u.isAdmin && <span class="badge">admin</span>}</td>
                  <td>{u.email}</td>
                  <td>{u.projects}</td>
                  <td class="muted">{timeAgo(u.createdAt)}</td>
                  <td class="right">
                    {u.id !== props.user.id && (
                      <button class="btn small" title={u.isAdmin ? 'Remove admin rights' : 'Make administrator'} onClick={async () => {
                        try {
                          await patch(`/api/admin/users/${u.id}`, { is_admin: !u.isAdmin });
                          load();
                        } catch (err: any) {
                          toast(err.message, 'error');
                        }
                      }}>{u.isAdmin ? 'Revoke admin' : 'Make admin'}</button>
                    )}
                    {u.id !== props.user.id && (
                      <button class="icon-btn" title="Reset password" onClick={async () => {
                        if (!confirm(`Reset the password of ${u.email}? They will be logged out everywhere.`)) return;
                        try {
                          const r = await post<{ password: string }>(`/api/admin/users/${u.id}/password`);
                          setReset({ email: u.email, password: r.password });
                        } catch (err: any) {
                          toast(err.message, 'error');
                        }
                      }}><Icon name="key" /></button>
                    )}
                    {u.id !== props.user.id && (
                      <button class="icon-btn danger" title="Delete user and their projects" onClick={async () => {
                        if (!confirm(`Delete ${u.email} and all projects they own?`)) return;
                        await del(`/api/admin/users/${u.id}`);
                        load();
                      }}><Icon name="trash" /></button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
        <section class="card">
          <h2>Create user</h2>
          <form class="stack narrow" onSubmit={create}>
            <input type="email" required placeholder="Email" value={form.email} onInput={(e) => setForm({ ...form, email: e.currentTarget.value })} />
            <input required placeholder="Name" value={form.name} onInput={(e) => setForm({ ...form, name: e.currentTarget.value })} />
            <input type="password" required minLength={8} placeholder="Initial password" value={form.password} onInput={(e) => setForm({ ...form, password: e.currentTarget.value })} />
            <label class="check"><input type="checkbox" checked={form.is_admin} onChange={(e) => setForm({ ...form, is_admin: e.currentTarget.checked })} /> Administrator</label>
            <div><button class="btn primary">Create user</button></div>
          </form>
        </section>
      </div>
    </div>
  );
}
