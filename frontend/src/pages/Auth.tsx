import { useState } from 'preact/hooks';
import { post, PublicConfig } from '../api';
import { Logo } from '../ui';

export function AuthPage(props: { config: PublicConfig; mode: 'login' | 'register'; onAuth: () => void }) {
  const [mode, setMode] = useState(props.mode);
  const [email, setEmail] = useState('');
  const [name, setName] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);

  const submit = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError('');
    try {
      if (mode === 'register') await post('/api/auth/register', { email, name, password });
      else await post('/api/auth/login', { email, password });
      location.hash = '#/';
      props.onAuth();
    } catch (err: any) {
      setError(err.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div class="auth-page">
      <form class="auth-card" onSubmit={submit}>
        <div class="auth-logo">
          <Logo size={40} />
          <span>{props.config.siteName}</span>
        </div>
        <h1>{mode === 'register' ? (props.config.firstUser ? 'Create the admin account' : 'Create an account') : 'Log in'}</h1>
        {props.config.firstUser && props.config.signup && (
          <p class="muted">This is a fresh install. The first account becomes the administrator.</p>
        )}
        {mode === 'register' && !props.config.signup && props.config.signupMessage && (
          <div class="alert">{props.config.signupMessage}</div>
        )}
        {mode === 'login' && props.config.firstUser && !props.config.signup && props.config.signupMessage && (
          <div class="alert">{props.config.signupMessage}</div>
        )}
        <label>
          Email
          <input type="email" required autoFocus value={email} onInput={(e) => setEmail(e.currentTarget.value)} />
        </label>
        {mode === 'register' && (
          <label>
            Name
            <input required value={name} onInput={(e) => setName(e.currentTarget.value)} />
          </label>
        )}
        <label>
          Password
          <input type="password" required autoComplete={mode === 'register' ? 'new-password' : 'current-password'} minLength={mode === 'register' ? 8 : 1} value={password} onInput={(e) => setPassword(e.currentTarget.value)} />
        </label>
        {error && <div class="alert error">{error}</div>}
        <button class="btn primary block" disabled={busy || (mode === 'register' && !props.config.signup)}>
          {mode === 'register' ? 'Register' : 'Log in'}
        </button>
        {props.config.signup && !props.config.firstUser && (
          <p class="muted center">
            {mode === 'login' ? (
              <>
                No account? <a href="#/register" onClick={() => setMode('register')}>Register</a>
              </>
            ) : (
              <>
                Already registered? <a href="#/login" onClick={() => setMode('login')}>Log in</a>
              </>
            )}
          </p>
        )}
        <p class="muted center small">
          <a href={props.config.sourceUrl} target="_blank" rel="noopener noreferrer">Source code</a> · AGPL-3.0
        </p>
      </form>
    </div>
  );
}
