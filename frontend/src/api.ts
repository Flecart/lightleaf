export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message);
  }
}

export async function api<T = any>(method: string, url: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, headers: {}, credentials: 'same-origin' };
  if (body instanceof FormData) {
    init.body = body;
  } else if (body !== undefined) {
    (init.headers as Record<string, string>)['Content-Type'] = 'application/json';
    init.body = JSON.stringify(body);
  }
  const res = await fetch(url, init);
  const type = res.headers.get('content-type') ?? '';
  const data = type.includes('application/json') ? await res.json() : await res.text();
  if (!res.ok) {
    const msg = typeof data === 'object' && data && 'error' in data ? (data as any).error : String(data || res.statusText);
    if (res.status === 401 && !url.startsWith('/api/auth')) {
      location.hash = '#/login';
    }
    throw new ApiError(res.status, msg);
  }
  return data as T;
}

export const get = <T = any>(url: string) => api<T>('GET', url);
export const post = <T = any>(url: string, body?: unknown) => api<T>('POST', url, body ?? {});
export const patch = <T = any>(url: string, body?: unknown) => api<T>('PATCH', url, body ?? {});
export const del = <T = any>(url: string) => api<T>('DELETE', url);

export interface User {
  id: number;
  email: string;
  name: string;
  is_admin: boolean;
}

export interface Project {
  id: string;
  name: string;
  compiler: string;
  mainFile: string;
  createdAt: number;
  updatedAt: number;
  owner: { id: number; name: string; email: string };
  role: 'owner' | 'editor' | 'viewer';
  members?: { id: number; name: string; email: string; role: string }[];
  compilers?: string[];
}

export interface Entry {
  path: string;
  kind: 'file' | 'dir';
  size: number;
  text: boolean;
}

export interface Issue {
  level: 'error' | 'warning' | 'typesetting';
  file: string | null;
  line: number | null;
  message: string;
  context?: string;
}

export interface CompileResult {
  status: 'success' | 'errors' | 'failure' | 'timeout';
  durationMs: number;
  pdf: string | null;
  issues: Issue[];
  log: string;
  synctex: boolean;
  mainFile: string;
}

export interface PublicConfig {
  siteName: string;
  signup: boolean;
  signupMessage: string | null;
  firstUser: boolean;
  compilers: string[];
  sourceUrl: string;
  /** The optional AI review service is configured. */
  reviewer: boolean;
  version: string;
}

export function timeAgo(secs: number): string {
  const d = Date.now() / 1000 - secs;
  if (d < 60) return 'just now';
  if (d < 3600) return `${Math.floor(d / 60)} min ago`;
  if (d < 86400) return `${Math.floor(d / 3600)} h ago`;
  if (d < 86400 * 30) return `${Math.floor(d / 86400)} days ago`;
  return new Date(secs * 1000).toLocaleDateString();
}

export function initials(name: string): string {
  return name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((p) => p[0]!.toUpperCase())
    .join('');
}
