/** The face demo's graphics badge and its "stats for nerds" overlay.
 * The tracking loop writes `TrackingStats` in place every frame; the overlay
 * samples it a few times a second, so an open panel never re-renders the demo.
 */
import type { Editor, Session } from "@catchlight/core";
import { useEffect, useMemo, useRef, useState } from "react";

/** Samples kept for each sparkline. */
const HISTORY = 60;
const SAMPLE_MS = 250;

export interface TrackingStats {
  /** Detector calls, and how many of those found a face. */
  detections: number;
  faces: number;
  /** Last detector call, in milliseconds. */
  inferenceMs: number;
  /** The params this frame drove, by name. */
  driven: Map<string, number>;
  /** The strongest blendshapes this frame, highest first. */
  shapes: [string, number][];
}

export function trackingStats(): TrackingStats {
  return { detections: 0, faces: 0, inferenceMs: 0, driven: new Map(), shapes: [] };
}

const TIER_LABEL: Record<string, string> = { webgpu: "WebGPU", webgl2: "WebGL2" };

/** The graphics tier the editor's device came up on, once there is one. */
export function useGpuTier(editor: Editor): string | undefined {
  const [tier, setTier] = useState(editor.gpuTier());
  useEffect(() => editor.onGpuChanged(() => setTier(editor.gpuTier())), [editor]);
  return tier;
}

export function TierBadge({ tier }: { tier: string | undefined }) {
  return <span className="tier-badge" data-tier={tier ?? "pending"} title="Graphics backend">
    {tier ? TIER_LABEL[tier] ?? tier : "Starting graphics…"}
  </span>;
}

/** Adapter details for the tier in use, from a probe separate from the editor's device. */
async function adapterInfo(tier: string | undefined): Promise<string> {
  if (tier === "webgpu") {
    const gpu = (navigator as unknown as { gpu?: { requestAdapter(): Promise<{ info?: Record<string, string> } | null> } }).gpu;
    const info = (await gpu?.requestAdapter())?.info;
    if (!info) return "unknown";
    return [info.vendor, info.architecture, info.device, info.description].filter(Boolean).join(" · ") || "unknown";
  }
  if (tier === "webgl2") {
    const gl = document.createElement("canvas").getContext("webgl2");
    if (!gl) return "unknown";
    const debug = gl.getExtension("WEBGL_debug_renderer_info");
    const renderer = gl.getParameter(debug ? debug.UNMASKED_RENDERER_WEBGL : gl.RENDERER) as string;
    gl.getExtension("WEBGL_lose_context")?.loseContext();
    return renderer;
  }
  return "—";
}

function Sparkline({ values, max, label }: { values: number[]; max: number; label: string }) {
  // Newest sample on the right edge, the way a scrolling trace reads.
  const offset = HISTORY - values.length;
  const points = values.map((v, i) => `${((offset + i) / (HISTORY - 1)) * 100},${20 - Math.min(1, v / max) * 18}`).join(" ");
  return <svg className="sparkline" viewBox="0 0 100 20" preserveAspectRatio="none" role="img" aria-label={label}>
    <polyline points={points} />
  </svg>;
}

function Bar({ value, min, max }: { value: number; min: number; max: number }) {
  const span = max - min || 1;
  const zero = Math.min(1, Math.max(0, -min / span));
  const at = Math.min(1, Math.max(0, (value - min) / span));
  return <span className="bar" aria-hidden="true">
    <i style={{ left: `${Math.min(zero, at) * 100}%`, width: `${Math.abs(at - zero) * 100}%` }} />
  </span>;
}

const clip = (text: string, length: number) => (text.length > length ? `${text.slice(0, length - 1)}…` : text);
const fixed = (n: number, digits = 1) => (Number.isFinite(n) ? n.toFixed(digits) : "—");

export function StatsForNerds({ editor, session, stats, video, canvasHost, tier, mirrored, active, onClose }: {
  editor: Editor;
  session: Session;
  stats: TrackingStats;
  video: HTMLVideoElement | null;
  canvasHost: HTMLElement | null;
  tier: string | undefined;
  mirrored: boolean;
  active: boolean;
  onClose: () => void;
}) {
  const [, redraw] = useState(0);
  const [adapter, setAdapter] = useState("…");
  const history = useRef({ fps: [] as number[], tracking: [] as number[], inference: [] as number[] });
  const rates = useRef({ fps: 0, tracking: 0, faces: 0 });

  const model = useMemo(() => {
    let nodes = 0;
    const walk = (node: ReturnType<Session["tree"]>) => { nodes++; node.children.forEach(walk); };
    walk(session.tree());
    const textures = session.textures();
    const texels = textures.reduce((sum, t) => sum + t.width * t.height, 0);
    const bounds = session.bounds();
    return { nodes, textures: textures.length, texels, params: session.params(), bounds };
  }, [session]);

  useEffect(() => {
    let live = true;
    adapterInfo(tier).then(info => { if (live) setAdapter(info); }, () => { if (live) setAdapter("unavailable"); });
    return () => { live = false; };
  }, [tier]);

  useEffect(() => {
    // Display frame rate from the page's own animation frames.
    let frames = 0, raf = 0;
    const count = () => { frames++; raf = requestAnimationFrame(count); };
    raf = requestAnimationFrame(count);
    let last = performance.now(), detections = stats.detections, faces = stats.faces;
    const push = (list: number[], v: number) => { list.push(v); if (list.length > HISTORY) list.shift(); };
    const timer = setInterval(() => {
      const now = performance.now(), seconds = (now - last) / 1000;
      rates.current = {
        fps: frames / seconds,
        tracking: (stats.detections - detections) / seconds,
        faces: (stats.faces - faces) / seconds,
      };
      frames = 0; last = now; detections = stats.detections; faces = stats.faces;
      push(history.current.fps, rates.current.fps);
      push(history.current.tracking, rates.current.tracking);
      push(history.current.inference, stats.inferenceMs);
      redraw(n => n + 1);
    }, SAMPLE_MS);
    return () => { cancelAnimationFrame(raf); clearInterval(timer); };
  }, [stats]);

  const canvas = canvasHost?.querySelector("canvas");
  const track = (video?.srcObject as MediaStream | null)?.getVideoTracks()[0];
  const settings = track?.getSettings();
  const heap = (performance as unknown as { memory?: { usedJSHeapSize: number; jsHeapSizeLimit: number } }).memory;
  const { fps, tracking, faces } = rates.current;
  const h = history.current;

  return <section className="nerd-stats" aria-label="Stats for nerds">
    <button type="button" className="nerd-close" onClick={onClose} aria-label="Close stats for nerds">×</button>
    <dl>
      <dt>Graphics</dt><dd>{tier ? TIER_LABEL[tier] ?? tier : "starting…"} / {editor.backendKind()}</dd>
      <dt>Adapter</dt><dd>{adapter}</dd>
      <dt>Viewport</dt><dd>{canvas ? `${canvas.clientWidth}×${canvas.clientHeight} css · ${canvas.width}×${canvas.height} px · dpr ${fixed(devicePixelRatio, 2)}` : "—"}</dd>
      <dt>Display</dt><dd className="with-graph"><span>{fixed(fps, 0)} fps</span><Sparkline values={h.fps} max={Math.max(60, ...h.fps)} label="Display frame rate" /></dd>
      <dt>Camera</dt><dd>{track ? `${settings?.width ?? video?.videoWidth}×${settings?.height ?? video?.videoHeight} @ ${fixed(settings?.frameRate ?? NaN, 0)} fps · ${clip(track.label || "camera", 40)}` : "off"}</dd>
      <dt>Tracking</dt><dd className="with-graph"><span>{fixed(tracking, 0)} Hz · face {tracking > 0 ? `${Math.round((faces / tracking) * 100)}%` : "—"}</span><Sparkline values={h.tracking} max={35} label="Tracking rate" /></dd>
      <dt>Inference</dt><dd className="with-graph"><span>{fixed(stats.inferenceMs)} ms</span><Sparkline values={h.inference} max={Math.max(33, ...h.inference)} label="Inference time" /></dd>
      <dt>Mapping</dt><dd>{mirrored ? "mirrored" : "direct"}</dd>
      <dt>Model</dt><dd>{model.nodes} nodes · {model.params.length} params · {model.textures} textures ({fixed(model.texels / 1e6, 2)} Mpx)</dd>
      {model.bounds && <><dt>Bounds</dt><dd>{model.bounds.map(v => Math.round(v)).join(", ")}</dd></>}
      {heap && <><dt>JS heap</dt><dd>{fixed(heap.usedJSHeapSize / 2 ** 20, 0)} / {fixed(heap.jsHeapSizeLimit / 2 ** 20, 0)} MiB</dd></>}
    </dl>
    <h2>Driven params</h2>
    {stats.driven.size === 0 && <p>{active ? "Waiting for a face." : "Start the camera to see live values."}</p>}
    <table>
      <tbody>{model.params.filter(p => stats.driven.has(p.name)).map(p => {
        const value = session.paramValue(p.id) ?? p.default;
        return <tr key={p.id}><th>{p.name}</th><td>{fixed(value, 2)}</td><td><Bar value={value} min={p.min} max={p.max} /></td></tr>;
      })}</tbody>
    </table>
    {stats.shapes.length > 0 && <>
      <h2>Top blendshapes</h2>
      <table>
        <tbody>{stats.shapes.map(([name, score]) => <tr key={name}><th>{name}</th><td>{fixed(score, 2)}</td><td><Bar value={score} min={0} max={1} /></td></tr>)}</tbody>
      </table>
    </>}
  </section>;
}
