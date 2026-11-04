import { useEffect, useState } from 'preact/hooks';
import { del, get, post, PublicConfig, timeAgo, User } from '../api';
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
        <UserMenu user={props.user} onLogout={() => (location.hash = '#/login')} />
      </TopBar>
      <div class="settings">
        <section class="card">
          <h2><Icon name="users" /> Users</h2>
          <p class="muted">
            Public sign-up is <strong>{props.config.signup ? 'enabled' : 'disabled'}</strong> (set <code>LIGHTLEAF_SIGNUP=closed</code> to disable).
            Installed compilers: {props.config.compilers.join(', ') || 'none'}.
          </p>
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
