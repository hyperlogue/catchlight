/** Browser camera lifetime and demo-specific mapping to the bundled Lan rig.
 * Tracking only poses the replica: no authored keys or saved model changes.
 * Each run owns its stream, detector and frame loop, including late async arrivals.
 * Mirroring is read per frame, so toggling it never restarts the camera.
 * The face detector drives the head and expression; a second pose detector
 * drives the torso from the shoulders, eased because pose depth is noisy.
 * A torso param eases back to rest whenever its landmarks are out of view.
 */
import type { Camera, Session } from "@catchlight/core";
import { useEditor, Viewport } from "@catchlight/react";
import type { FaceLandmarker, PoseLandmarker, PoseLandmarkerResult } from "@mediapipe/tasks-vision";
import { useEffect, useRef, useState } from "react";
import { StatsForNerds, TierBadge, trackingStats, useGpuTier } from "./face-stats.js";

/** Lan's head with room for a turn and a tilt. */
const FACE_CAMERA: Camera = { center: [-3, 770], height: 520 };

const MODELS = "https://storage.googleapis.com/mediapipe-models";
/** MediaPipe pose landmark indices; "left" is the subject's own left. */
const LEFT_SHOULDER = 11, RIGHT_SHOULDER = 12, LEFT_HIP = 23, RIGHT_HIP = 24;
/** Share of the gap to the target closed per detection. */
const BODY_EASE = 0.35;

/** Torso targets in rig units, unmirrored and before clamping. A param is
 * absent while the landmarks it needs are out of view. */
export function torsoTargets(result: PoseLandmarkerResult): Map<string, number> {
  const targets = new Map<string, number>();
  const image = result.landmarks[0], world = result.worldLandmarks[0];
  if (!image || !world) return targets;
  const seen = (...indices: number[]) => indices.every(i => (image[i]?.visibility ?? 0) > 0.5);
  if (!seen(LEFT_SHOULDER, RIGHT_SHOULDER)) return targets;
  const degrees = 180 / Math.PI;
  // World landmarks are metric: x toward image right, y down, z away from the camera.
  const l = world[LEFT_SHOULDER]!, r = world[RIGHT_SHOULDER]!;
  // The rig's torso range is a few degrees, so a real turn is scaled down to fit.
  targets.set("body_yaw", Math.atan2(r.z - l.z, l.x - r.x) * degrees * 0.5);
  // Lan's body_roll runs opposite to head_roll.
  targets.set("body_roll", Math.atan2(l.y - r.y, l.x - r.x) * degrees);
  // Sway: the shoulder midpoint reaches the full range a quarter frame off centre.
  targets.set("body_move_x", (0.5 - (image[LEFT_SHOULDER]!.x + image[RIGHT_SHOULDER]!.x) / 2) * 40);
  if (seen(LEFT_HIP, RIGHT_HIP)) {
    const lh = world[LEFT_HIP]!, rh = world[RIGHT_HIP]!;
    // Lan's body_pitch is positive leaning toward the viewer.
    const up = (lh.y + rh.y - l.y - r.y) / 2, forward = (lh.z + rh.z - l.z - r.z) / 2;
    targets.set("body_pitch", Math.atan2(forward, up) * degrees * 0.5);
  }
  return targets;
}

export function FaceDemo({ session }: { session: Session }) {
  const editor = useEditor();
  const tier = useGpuTier(editor);
  const video = useRef<HTMLVideoElement>(null);
  const stage = useRef<HTMLDivElement>(null);
  const [stats] = useState(trackingStats);
  const [mirrored, setMirrored] = useState(true);
  const mirror = useRef(mirrored);
  mirror.current = mirrored;
  const [nerdy, setNerdy] = useState(false);
  const [active, setActive] = useState(false);
  const [status, setStatus] = useState("Ready when you are. Place your face in view of the camera.");
  const [error, setError] = useState("");

  useEffect(() => {
    if (!active) return;
    let disposed = false;
    let stream: MediaStream | undefined;
    let detector: FaceLandmarker | undefined;
    let poser: PoseLandmarker | undefined;
    /** Eased torso values, by param name. */
    const body = new Map<string, number>();
    let frame = 0;
    const params = session.params();
    const pose = (name: string, value: number) => {
      const param = params.find(p => p.name === name || p.id === name);
      if (!param) return;
      session.setParam(param.id, Math.max(param.min, Math.min(param.max, value)));
      stats.driven.set(param.name, value);
    };
    const reset = () => {
      for (const p of params) session.setParam(p.id, p.default);
      stats.driven.clear();
      stats.shapes = [];
      body.clear();
    };
    const cleanup = () => {
      disposed = true;
      cancelAnimationFrame(frame);
      stream?.getTracks().forEach(track => track.stop());
      detector?.close();
      detector = undefined;
      poser?.close();
      poser = undefined;
      if (video.current) video.current.srcObject = null;
      reset();
    };
    const fail = (cause: unknown) => {
      cleanup();
      setActive(false);
      setError(cause instanceof Error ? cause.message : String(cause));
      setStatus("Tracking stopped. Check camera permissions and your connection, then try again.");
    };
    async function start() {
      setError("");
      setStatus("Waiting for camera permission…");
      if (!navigator.mediaDevices?.getUserMedia) throw new Error("Camera access requires HTTPS and a supported browser.");
      const acquired = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "user", width: 640, height: 480 }, audio: false });
      if (disposed) { acquired.getTracks().forEach(track => track.stop()); return; }
      stream = acquired;
      stream.getVideoTracks()[0]?.addEventListener("ended", () => { if (!disposed) fail(new Error("The camera disconnected.")); });
      const element = video.current!;
      element.srcObject = stream;
      await element.play();
      if (disposed) return;
      setStatus("Loading face tracking…");
      const { FaceLandmarker, FilesetResolver, PoseLandmarker } = await import("@mediapipe/tasks-vision");
      const files = await FilesetResolver.forVisionTasks("https://cdn.jsdelivr.net/npm/@mediapipe/tasks-vision@0.10.32/wasm");
      if (disposed) return;
      const [loaded, torso] = await Promise.all([
        FaceLandmarker.createFromOptions(files, {
          baseOptions: { modelAssetPath: `${MODELS}/face_landmarker/face_landmarker/float16/1/face_landmarker.task` },
          runningMode: "VIDEO", numFaces: 1, outputFaceBlendshapes: true,
          outputFacialTransformationMatrixes: true,
        }),
        PoseLandmarker.createFromOptions(files, {
          baseOptions: { modelAssetPath: `${MODELS}/pose_landmarker/pose_landmarker_lite/float16/1/pose_landmarker_lite.task` },
          runningMode: "VIDEO", numPoses: 1,
        }),
      ]);
      if (disposed) { loaded.close(); torso.close(); return; }
      detector = loaded;
      poser = torso;
      let lastTime = -1;
      let lastFrame = 0;
      const tick = (now: number) => {
        if (disposed) return;
        try {
          // Limit synchronous inference to 30 fps and never process a frame twice.
          if (element.readyState >= 2 && element.currentTime !== lastTime && now - lastFrame >= 33) {
            lastTime = element.currentTime;
            lastFrame = now;
            const started = performance.now();
            const result = loaded.detectForVideo(element, now);
            const posture = torso.detectForVideo(element, now);
            stats.inferenceMs = performance.now() - started;
            stats.detections++;
            if (result.faceLandmarks.length === 0) {
              reset();
              setStatus("No face in view. Face the camera in good light.");
            } else {
              setStatus("Tracking live — turn your head, blink, open your mouth, or lean.");
              stats.faces++;
              const categories = result.faceBlendshapes[0]?.categories ?? [];
              stats.shapes = categories.filter(c => c.categoryName !== "_neutral")
                .sort((a, b) => b.score - a.score).slice(0, 6).map(c => [c.categoryName, c.score]);
              const scores = new Map(categories.map(c => [c.categoryName, c.score]));
              const score = (name: string) => scores.get(name) ?? 0;
              // Mirrored, Lan moves like your reflection: your left eye drives
              // the eye on the same side of the screen, and turns follow suit.
              const flip = mirror.current;
              const side = flip ? -1 : 1;
              pose(flip ? "eye_right_open" : "eye_left_open", 1 - score("eyeBlinkLeft"));
              pose(flip ? "eye_left_open" : "eye_right_open", 1 - score("eyeBlinkRight"));
              pose("mouth_open", score("jawOpen") * 1.5);
              pose("mouth_expression", (score("mouthSmileLeft") + score("mouthSmileRight")) / 2);
              // MediaPipe's column-major rotation matrix, converted to degrees.
              const m = result.facialTransformationMatrixes[0]?.data;
              if (m) {
                const degrees = 180 / Math.PI;
                pose("head_yaw", -side * Math.atan2(m[8]!, m[10]!) * degrees);
                pose("head_pitch", -Math.atan2(-m[9]!, Math.hypot(m[8]!, m[10]!)) * degrees);
                pose("head_roll", -side * Math.atan2(m[1]!, m[5]!) * degrees);
              }
              const targets = torsoTargets(posture);
              for (const name of ["body_yaw", "body_roll", "body_move_x", "body_pitch"]) {
                // Like the head, turns and sway flip with the mirror; a lean does not.
                const target = (targets.get(name) ?? 0) * (name === "body_pitch" ? 1 : -side);
                const eased = (body.get(name) ?? 0) + (target - (body.get(name) ?? 0)) * BODY_EASE;
                body.set(name, eased);
                pose(name, eased);
              }
            }
          }
          frame = requestAnimationFrame(tick);
        } catch (cause) { fail(cause); }
      };
      frame = requestAnimationFrame(tick);
    }
    void start().catch(cause => { if (!disposed) fail(cause); });
    // Page navigation and backgrounding must release the camera as well as unmount.
    const stop = () => { cleanup(); setActive(false); setStatus("Camera stopped."); };
    const hidden = () => { if (document.hidden) stop(); };
    window.addEventListener("pagehide", stop);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      window.removeEventListener("pagehide", stop);
      document.removeEventListener("visibilitychange", hidden);
      cleanup();
    };
  }, [active, session, stats]);

  return <main className="face-demo">
    <div className="face-stage" ref={stage}>
      <Viewport.Root session={session} defaultCamera={FACE_CAMERA} aria-label="Lan following your face" onError={cause => setError(String(cause))} />
      <TierBadge tier={tier} />
      {nerdy && <StatsForNerds editor={editor} session={session} stats={stats} video={video.current} canvasHost={stage.current}
        tier={tier} mirrored={mirrored} active={active} onClose={() => setNerdy(false)} />}
    </div>
    <aside className="face-controls">
      <p className="eyebrow">LIVE PERFORMANCE</p><h1>Your expressions.<br />Lan’s animation.</h1>
      <p>Turn your head, blink, open your mouth, and lean or turn your shoulders to bring Lan to life.</p>
      <video ref={video} muted playsInline className={mirrored ? "mirrored" : undefined}
        aria-label={mirrored ? "Mirrored camera preview" : "Camera preview"} />
      <p role="status">{status}</p>
      {error && <p role="alert" className="camera-error">{error}</p>}
      <button type="button" onClick={() => { setActive(!active); if (active) setStatus("Camera stopped."); }}>{active ? "Stop camera" : "Start camera"}</button>
      <div className="face-toggles">
        <label><input type="checkbox" role="switch" checked={mirrored} onChange={e => setMirrored(e.target.checked)} />Mirror left and right</label>
        <label><input type="checkbox" role="switch" checked={nerdy} onChange={e => setNerdy(e.target.checked)} />Stats for nerds</label>
      </div>
      <p className="privacy-note">Camera frames stay in your browser. Tracking assets download from Google and jsDelivr when you start. No audio is captured.</p>
    </aside>
  </main>;
}
