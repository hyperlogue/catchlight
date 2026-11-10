/** The entry page stays lightweight; a demo loads only after selection. */
import { createRoot } from "react-dom/client";
import logo from "../../../assets/logo-transparent.svg";
import "./site.css";

const query = new URLSearchParams(location.search);
const mode = query.get("demo");
// Existing connected and probe URLs remain direct editor entry points.
if (mode === "face" || mode === "editor" || query.has("server") || query.has("probe")) {
  void import("./demo.js").then(({ start }) => start(mode === "face" ? "face" : "editor")).catch(cause => {
    const failure = document.getElementById("failure")!;
    failure.hidden = false;
    failure.textContent = `Could not load the demo. Reload to try again. ${String(cause)}`;
  });
} else {
  createRoot(document.getElementById("editor")!).render(
    <main className="chooser">
      <div className="demo-brand"><img src={logo} alt="" width="40" height="40" />catchlight</div>
      <p className="eyebrow">CHARACTERS IN MOTION</p>
      <h1>Bring a character to life.</h1>
      <p className="intro">Two ways to explore Catchlight’s 2.5D animation. Pick a demo to begin.</p>
      <div className="demo-cards">
        <a className="demo-card" href="?demo=editor">
          <span className="card-number">01 / CREATE</span><h2>Editor demo</h2>
          <p>Explore Lan’s rig, pose the character, and shape your own animation in the studio.</p>
          <strong>Open the editor <span aria-hidden="true">↗</span></strong>
        </a>
        <a className="demo-card" href="?demo=face">
          <span className="card-number">02 / PERFORM</span><h2>Face tracking</h2>
          <p>Turn your head, blink, and speak. Watch Lan follow your expressions through your camera.</p>
          <strong>Try face tracking <span aria-hidden="true">↗</span></strong>
        </a>
      </div>
      <p className="chooser-note">Runs in your browser. Face tracking needs a camera; the editor does not.</p>
    </main>,
  );
}
