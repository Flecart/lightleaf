import { useState } from 'preact/hooks';
import { post, User } from '../api';
import { Avatar, Icon } from '../ui';

export function UserMenu(props: { user: User; onLogout: () => void; sourceUrl?: string }) {
  const [open, setOpen] = useState(false);
  const logout = async () => {
    await post('/api/auth/logout');
    props.onLogout();
    location.hash = '#/login';
  };
  return (
    <div class="dropdown right">
      <button class="user-btn" onClick={() => setOpen(!open)}>
        <Avatar name={props.user.name} color="#138a36" />
        <span class="hide-sm">{props.user.name}</span>
        <Icon name="chevron-down" size={14} />
      </button>
      {open && (
        <div class="menu" onMouseLeave={() => setOpen(false)}>
          <div class="menu-label">{props.user.email}</div>
          <a href="#/">
            <Icon name="home" /> Projects
          </a>
          <a href="#/settings">
            <Icon name="settings" /> Account &amp; git tokens
          </a>
          {props.user.is_admin && (
            <a href="#/admin">
              <Icon name="shield" /> Admin
            </a>
          )}
          <a href={props.sourceUrl ?? 'https://github.com/Flecart/lightleaf'} target="_blank" rel="noopener noreferrer">
            <Icon name="git" /> Source code (AGPL)
          </a>
          <hr />
          <button onClick={logout}>
            <Icon name="logout" /> Log out
          </button>
        </div>
      )}
    </div>
  );
}
