/** Browser camera lifetime and demo-specific mapping to the bundled Lan rig.
 * Tracking only poses the replica: no authored keys or saved model changes.
 * Each run owns its stream, detector and frame loop, including late async arrivals.
 */
import type { Session } from "@catchlight/core";
import { Viewport } from "@catchlight/react";
import type { FaceLandmarker } from "@mediapipe/tasks-vision";
import { useEffect, useRef, useState } from "react";

export function FaceDemo({ session }: { session: Session }) {
  const video = useRef<HTMLVideoElement>(null);
  const [active, setActive] = useState(false);
  const [status, setStatus] = useState("Ready when you are. Place your face in view of the camera.");
  const [error, setError] = useState("");

  useEffect(() => {
    if (!active) return;
    let disposed = false;
    let stream: MediaStream | undefined;
    let detector: FaceLandmarker | undefined;
    let frame = 0;
    const params = session.params();
    const pose = (name: string, value: number) => {
      const param = params.find(p => p.name === name || p.id === name);
      if (param) session.setParam(param.id, Math.max(param.min, Math.min(param.max, value)));
    };
    const reset = () => { for (const p of params) session.setParam(p.id, p.default); };
    const cleanup = () => {
      disposed = true;
      cancelAnimationFrame(frame);
      stream?.getTracks().forEach(track => track.stop());
      detector?.close();
      detector = undefined;
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
      const { FaceLandmarker, FilesetResolver } = await import("@mediapipe/tasks-vision");
      const files = await FilesetResolver.forVisionTasks("https://cdn.jsdelivr.net/npm/@mediapipe/tasks-vision@0.10.32/wasm");
      if (disposed) return;
      const loaded = await FaceLandmarker.createFromOptions(files, {
        baseOptions: { modelAssetPath: "https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task" },
        runningMode: "VIDEO", numFaces: 1, outputFaceBlendshapes: true,
        outputFacialTransformationMatrixes: true,
      });
      if (disposed) { loaded.close(); return; }
      detector = loaded;
      let lastTime = -1;
      let lastFrame = 0;
      const tick = (now: number) => {
        if (disposed) return;
        try {
          // Limit synchronous inference to 30 fps and never process a frame twice.
          if (element.readyState >= 2 && element.currentTime !== lastTime && now - lastFrame >= 33) {
            lastTime = element.currentTime;
            lastFrame = now;
            const result = loaded.detectForVideo(element, now);
            if (result.faceLandmarks.length === 0) {
              reset();
              setStatus("No face in view. Face the camera in good light.");
            } else {
              setStatus("Tracking live — turn your head, blink, or open your mouth.");
              const scores = new Map(result.faceBlendshapes[0]?.categories.map(c => [c.categoryName, c.score]));
              const score = (name: string) => scores.get(name) ?? 0;
              pose("ParamEyeLOpen", 1 - score("eyeBlinkLeft"));
              pose("ParamEyeROpen", 1 - score("eyeBlinkRight"));
              pose("ParamMouthOpenY", score("jawOpen") * 1.5);
              pose("ParamMouthForm", (score("mouthSmileLeft") + score("mouthSmileRight")) / 2);
              // MediaPipe's column-major rotation matrix, converted to degrees.
              const m = result.facialTransformationMatrixes[0]?.data;
              if (m) {
                const degrees = 180 / Math.PI;
                pose("ParamAngleX", Math.atan2(m[8]!, m[10]!) * degrees);
                pose("ParamAngleY", -Math.atan2(-m[9]!, Math.hypot(m[8]!, m[10]!)) * degrees);
                pose("ParamAngleZ", -Math.atan2(m[1]!, m[5]!) * degrees);
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
  }, [active, session]);

  return <main className="face-demo">
    <div className="face-stage"><Viewport.Root session={session} aria-label="Lan following your face" onError={cause => setError(String(cause))} /></div>
    <aside className="face-controls">
      <p className="eyebrow">LIVE PERFORMANCE</p><h1>Your expressions.<br />Lan’s animation.</h1>
      <p>Turn your head, blink, and open your mouth to bring Lan to life.</p>
      <video ref={video} muted playsInline aria-label="Mirrored camera preview" />
      <p role="status">{status}</p>
      {error && <p role="alert" className="camera-error">{error}</p>}
      <button type="button" onClick={() => { setActive(!active); if (active) setStatus("Camera stopped."); }}>{active ? "Stop camera" : "Start camera"}</button>
      <p className="privacy-note">Camera frames stay in your browser. Tracking assets download from Google and jsDelivr when you start. No audio is captured.</p>
    </aside>
  </main>;
}
