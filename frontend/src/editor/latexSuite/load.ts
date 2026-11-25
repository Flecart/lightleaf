import { get, put } from '../../api';
import { compileConfig, CompiledConfig } from './config';

/** The user's stored Latex Suite settings, or null when they use the defaults. */
export async function fetchLatexSuite(): Promise<Record<string, unknown> | null> {
  const r = await get<{ settings: Record<string, unknown> | null }>('/api/me/latex-suite');
  return r.settings;
}

export function saveLatexSuite(settings: Record<string, unknown> | null): Promise<unknown> {
  return put('/api/me/latex-suite', { settings });
}

/** The compiled configuration for the editor; the defaults if the stored one cannot be loaded. */
export async function loadLatexSuite(): Promise<CompiledConfig> {
  try {
    return compileConfig(await fetchLatexSuite());
  } catch {
    return compileConfig(null);
  }
}
