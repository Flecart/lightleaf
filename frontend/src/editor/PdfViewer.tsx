import { useEffect, useRef, useState } from 'preact/hooks';
import * as pdfjs from 'pdfjs-dist';
import type { PDFDocumentProxy, RenderTask } from 'pdfjs-dist';
import workerUrl from 'pdfjs-dist/build/pdf.worker.min.mjs?url';
import { Icon } from '../ui';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

export interface PdfHighlight {
  page: number;
  h: number;
  v: number;
  w: number;
  hh: number;
  nonce: number;
}

interface Props {
  url: string | null;
  highlight: PdfHighlight | null;
  onDoubleClick?: (page: number, x: number, y: number) => void;
  /** The loaded document, e.g. for text-based sync of Typst output. */
  onDocument?: (doc: PDFDocumentProxy | null) => void;
}

type Zoom = 'fit' | 'page' | number;

export function PdfViewer(props: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const [doc, setDoc] = useState<PDFDocumentProxy | null>(null);
  const [sizes, setSizes] = useState<{ w: number; h: number }[]>([]);
  const [zoom, setZoom] = useState<Zoom>(() => {
    const z = localStorage.getItem('ll-pdf-zoom');
    return z === 'fit' || z === 'page' || !z ? ((z as Zoom) ?? 'fit') : Number(z);
  });
  const [width, setWidth] = useState(600);
  const [height, setHeight] = useState(800);
  const [page, setPage] = useState(1);
  const [error, setError] = useState('');
  const rendered = useRef(new Map<number, { scale: number; task?: RenderTask }>());
  const pending = useRef<{ top: number; ratio: number } | null>(null);

  // Load document
  useEffect(() => {
    if (!props.url) return;
    let cancelled = false;
    const el = scroller.current;
    if (el && doc) pending.current = { top: el.scrollTop, ratio: el.scrollTop / Math.max(1, el.scrollHeight) };
    const task = pdfjs.getDocument({ url: props.url, isEvalSupported: false });
    task.promise
      .then(async (d) => {
        const s: { w: number; h: number }[] = [];
        for (let i = 1; i <= d.numPages; i++) {
          const p = await d.getPage(i);
          const vp = p.getViewport({ scale: 1 });
          s.push({ w: vp.width, h: vp.height });
        }
        if (cancelled) return d.destroy();
        setError('');
        rendered.current.clear();
        setSizes(s);
        setDoc((old) => {
          old?.destroy();
          return d;
        });
      })
      .catch((e) => !cancelled && setError(String(e?.message ?? e)));
    return () => {
      cancelled = true;
    };
  }, [props.url]);

  useEffect(() => props.onDocument?.(doc), [doc]);

  // Track container size
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setWidth(el.clientWidth);
      setHeight(el.clientHeight);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const maxW = Math.max(1, ...sizes.map((s) => s.w));
  const firstH = sizes[0]?.h ?? 842;
  const scale =
    zoom === 'fit' ? Math.max(0.2, (width - 32) / maxW) : zoom === 'page' ? Math.max(0.2, Math.min((width - 32) / maxW, (height - 24) / firstH)) : zoom;

  // Render visible pages lazily
  useEffect(() => {
    const el = scroller.current;
    if (!doc || !el) return;
    rendered.current.forEach((r) => r.task?.cancel());
    rendered.current.clear();
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const renderPage = async (idx: number, holder: HTMLElement) => {
      const prev = rendered.current.get(idx);
      if (prev && prev.scale === scale) return;
      prev?.task?.cancel();
      const entry: { scale: number; task?: RenderTask } = { scale };
      rendered.current.set(idx, entry);
      try {
        const p = await doc.getPage(idx + 1);
        const vp = p.getViewport({ scale: scale * dpr });
        const canvas = document.createElement('canvas');
        canvas.width = Math.floor(vp.width);
        canvas.height = Math.floor(vp.height);
        canvas.style.width = '100%';
        canvas.style.height = '100%';
        entry.task = p.render({ canvasContext: canvas.getContext('2d')!, viewport: vp });
        await entry.task.promise;
        holder.querySelector('canvas')?.remove();
        holder.prepend(canvas);
      } catch {
        /* cancelled */
      }
    };
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) renderPage(Number((e.target as HTMLElement).dataset.idx), e.target as HTMLElement);
        }
      },
      { root: el, rootMargin: '600px 0px' },
    );
    el.querySelectorAll<HTMLElement>('.pdf-page').forEach((p) => io.observe(p));
    if (pending.current) {
      const { top, ratio } = pending.current;
      el.scrollTop = zoom === 'fit' ? ratio * el.scrollHeight : top;
      pending.current = null;
    }
    return () => io.disconnect();
  }, [doc, scale]);

  // Scroll to SyncTeX highlight
  useEffect(() => {
    const h = props.highlight;
    const el = scroller.current;
    if (!h || !el) return;
    const pageEl = el.querySelector<HTMLElement>(`.pdf-page[data-idx="${h.page - 1}"]`);
    if (!pageEl) return;
    el.scrollTo({ top: pageEl.offsetTop + (h.v - h.hh) * scale - el.clientHeight / 3, behavior: 'smooth' });
    const box = document.createElement('div');
    box.className = 'synctex-box';
    Object.assign(box.style, {
      left: `${h.h * scale - 4}px`,
      top: `${(h.v - h.hh) * scale - 4}px`,
      width: `${Math.max(h.w, 20) * scale + 8}px`,
      height: `${Math.max(h.hh, 10) * scale + 8}px`,
    });
    pageEl.appendChild(box);
    const t = setTimeout(() => box.remove(), 2000);
    return () => {
      clearTimeout(t);
      box.remove();
    };
  }, [props.highlight]);

  const setZ = (z: Zoom) => {
    setZoom(z);
    localStorage.setItem('ll-pdf-zoom', String(z));
  };

  const onScroll = () => {
    const el = scroller.current;
    if (!el) return;
    const pages = el.querySelectorAll<HTMLElement>('.pdf-page');
    const mid = el.scrollTop + el.clientHeight / 3;
    for (let i = 0; i < pages.length; i++) {
      const p = pages[i]!;
      if (p.offsetTop + p.offsetHeight > mid) {
        setPage(i + 1);
        break;
      }
    }
  };

  const onDbl = (e: MouseEvent) => {
    const target = (e.target as HTMLElement).closest<HTMLElement>('.pdf-page');
    if (!target || !props.onDoubleClick) return;
    const rect = target.getBoundingClientRect();
    props.onDoubleClick(Number(target.dataset.idx) + 1, (e.clientX - rect.left) / scale, (e.clientY - rect.top) / scale);
  };

  return (
    <div class="pdf-viewer">
      <div class="pdf-toolbar">
        <span class="muted small">{doc ? `${page} / ${doc.numPages}` : ''}</span>
        <div class="spacer" />
        <button class="icon-btn" title="Zoom out" onClick={() => setZ(Math.max(0.25, +(scale / 1.2).toFixed(2)))}><Icon name="zoomout" /></button>
        <span class="small zoom-label">{Math.round(scale * 100)}%</span>
        <button class="icon-btn" title="Zoom in" onClick={() => setZ(Math.min(5, +(scale * 1.2).toFixed(2)))}><Icon name="zoomin" /></button>
        <button class={`icon-btn ${zoom === 'fit' ? 'active' : ''}`} title="Fit width" onClick={() => setZ('fit')}><Icon name="fit" /></button>
        <button class={`icon-btn ${zoom === 'page' ? 'active' : ''}`} title="Fit page" onClick={() => setZ('page')}><Icon name="file" /></button>
      </div>
      <div class="pdf-scroll" ref={scroller} onScroll={onScroll} onDblClick={onDbl}>
        {error && <div class="alert error">Could not load PDF: {error}</div>}
        {!props.url && !error && <div class="pdf-empty muted">Compile the project to see the PDF preview.</div>}
        {sizes.map((s, i) => (
          <div class="pdf-page" data-idx={i} key={i} style={{ width: `${s.w * scale}px`, height: `${s.h * scale}px` }} />
        ))}
      </div>
    </div>
  );
}
