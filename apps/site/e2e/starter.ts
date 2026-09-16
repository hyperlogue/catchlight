/** Regenerate the site's original, layered starter through the editor itself.
 * Run the dev host, then `bun run --filter catchlight-site sample [url]`.
 * SVG is the source art; Chromium rasterizes it, and the ordinary authoring
 * protocol creates the meshes, controls and .clm. No private model is used. */
import { chromium } from "playwright-core";
import type { Editor, ScalarTarget } from "@catchlight/core";

const browser = await chromium.launch({
  executablePath: Bun.which("chromium") ?? "chromium",
  headless: true,
  args: [
    "--no-sandbox",
    "--disable-features=WebGPU",
    "--use-angle=swiftshader",
    "--enable-unsafe-swiftshader",
  ],
});
try {
  const page = await browser.newPage();
  const url = new URL(process.argv[2] ?? "http://localhost:5173/");
  url.searchParams.set("probe", "1");
  await page.goto(url.href);
  await page.waitForFunction(() => "__catchlightProbe" in globalThis);
  const bytes = await page.evaluate(async () => {
    const { editor } = (globalThis as unknown as { __catchlightProbe: { editor: Editor } })
      .__catchlightProbe;
    const session = await editor.newSession();
    await session.send({ cmd: "node_set", node: "root", name: "Mica" });
    const defs = `<defs><linearGradient id="fur" x2=".7" y2="1"><stop stop-color="#f9bc79"/><stop offset="1" stop-color="#dc8251"/></linearGradient><linearGradient id="coat" x2=".5" y2="1"><stop stop-color="#83a695"/><stop offset="1" stop-color="#486c60"/></linearGradient><linearGradient id="cream" x2=".5" y2="1"><stop stop-color="#fff1da"/><stop offset="1" stop-color="#e9ceaa"/></linearGradient></defs>`;
    const headPivot: [number, number] = [300, 255];
    const bodyPivot: [number, number] = [300, 425];
    const world = ([x, y]: [number, number]): [number, number] => [x - 300, 330 - y];
    async function group(id: string, name: string, pivot: [number, number]) {
      await session.send({
        cmd: "node_add",
        parent: "root",
        kind: "group",
        name,
        node: id,
      });
      const [x, y] = world(pivot);
      await session.send({ cmd: "node_set", node: id, translate: [x, y, 0] });
    }
    await group("body", "Body", bodyPivot);
    await group("head", "Head", headPivot);
    async function part(
      id: string,
      name: string,
      svg: string,
      z: number,
      pivot: [number, number],
      parent = "root",
      parentPivot: [number, number] = [300, 330],
    ) {
      const image = new Image();
      image.src = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="600" height="660" viewBox="0 0 600 660">${defs}${svg}</svg>`)}`;
      await image.decode();
      const canvas = document.createElement("canvas");
      canvas.width = 600;
      canvas.height = 660;
      canvas.getContext("2d")!.drawImage(image, 0, 0);
      const blob = await new Promise<Blob>((resolve) =>
        canvas.toBlob((b) => resolve(b!), "image/png"),
      );
      await session.send({
        cmd: "node_add",
        node: id,
        parent,
        kind: "part",
        name,
      });
      await session.sendWith(
        { cmd: "texture_add", node: id, texture: `${id}-art`, encoding: "png" },
        [["texture", new Uint8Array(await blob.arrayBuffer())]],
      );
      await session.send({
        cmd: "mesh_generate",
        node: id,
        mode: { mode: "contour", spacing: 28, margin: 2, simplify: 2 },
      });
      const mesh = session.mesh(id);
      await session.send({
        cmd: "mesh_set",
        node: id,
        ...mesh,
        origin: world(pivot),
      });
      const p = world(pivot),
        pp = world(parentPivot);
      await session.send({
        cmd: "node_set",
        node: id,
        translate: [p[0] - pp[0], p[1] - pp[1], 0],
        z_order: z,
      });
    }
    await part(
      "tail",
      "Tail",
      `<path d="M357 477C409 470 471 438 476 383C484 332 459 303 464 259C527 311 552 383 520 450C495 501 437 527 370 520Z" fill="url(#fur)"/><path d="M476 383C484 332 459 303 464 259C500 289 523 324 530 361C511 358 496 363 476 383Z" fill="url(#cream)"/><path d="M399 501C452 492 489 460 503 424" fill="none" stroke="#bc6e49" stroke-width="3" opacity=".5"/>`,
      0,
      [370, 498],
    );
    await part(
      "feet",
      "Boots",
      `<path d="M242 489L239 551Q216 551 211 569Q211 582 233 583L273 583Q283 581 279 563L278 495Z" fill="#334c45"/><path d="M324 494L324 558Q321 581 335 583L375 583Q394 582 389 569Q383 554 361 551L358 491Z" fill="#334c45"/><path d="M217 575H276M328 575H386" stroke="#a6b3a0" stroke-width="3"/><path d="M244 554H267M335 554H356" stroke="#829787" stroke-width="3"/>`,
      1,
      [300, 510],
      "body",
      bodyPivot,
    );
    await part(
      "arm-left",
      "Left arm",
      `<path d="M234 358Q204 347 189 378L159 441Q155 465 174 471Q191 476 202 453L242 394Z" fill="url(#coat)"/><path d="M158 442Q149 465 162 478Q174 490 188 476L199 455Z" fill="url(#fur)"/><path d="M159 440L198 455" stroke="#b5c7ae" stroke-width="7"/>`,
      2,
      [233, 367],
      "body",
      bodyPivot,
    );
    await part(
      "torso",
      "Field jacket",
      `<path d="M241 333Q300 319 361 336L384 479Q384 511 357 518L243 518Q216 511 218 484Z" fill="url(#coat)"/><path d="M294 364V516" stroke="#b0c2a5" stroke-width="3"/><path d="M239 431H278V469Q258 480 239 469ZM311 431H360V469Q336 480 311 469Z" fill="#426358" stroke="#86a28a" stroke-width="2"/><circle cx="294" cy="400" r="3" fill="#e4d4a8"/><circle cx="294" cy="466" r="3" fill="#e4d4a8"/><path d="M249 337L273 377L293 355L316 377L349 337" fill="#a8b79b"/>`,
      3,
      bodyPivot,
      "body",
      bodyPivot,
    );
    await part(
      "arm-right",
      "Waving arm",
      `<path d="M352 353Q375 342 391 369L415 405L443 373L468 393Q439 460 407 452Q389 448 366 411L348 379Z" fill="url(#coat)"/><path d="M441 376L449 356Q450 349 456 352L458 364L475 344Q481 338 485 344L480 357Q495 347 498 357Q500 363 486 378Q498 374 500 382Q498 396 464 402Z" fill="url(#fur)"/><path d="M442 375L470 396" stroke="#b5c7ae" stroke-width="7"/>`,
      4,
      [362, 366],
      "body",
      bodyPivot,
    );
    await part(
      "ear-left",
      "Left ear",
      `<path d="M199 207Q164 145 179 81Q238 105 260 171Z" fill="url(#fur)"/><path d="M199 177Q184 142 190 107Q219 128 235 168Z" fill="#895248"/><path d="M189 102L200 116L207 105L215 123" fill="#fff0d6"/>`,
      5,
      headPivot,
      "head",
      headPivot,
    );
    await part(
      "ear-right",
      "Right ear",
      `<path d="M346 169Q368 109 426 79Q445 143 404 211Z" fill="url(#fur)"/><path d="M369 170Q385 132 413 108Q420 142 400 178Z" fill="#895248"/><path d="M399 110L408 118L419 103" fill="#fff0d6"/>`,
      5,
      headPivot,
      "head",
      headPivot,
    );
    await part(
      "face",
      "Face",
      `<path d="M192 188Q233 155 304 163Q375 154 408 190Q435 224 425 262L440 272L421 278L431 288L410 291Q387 336 306 350Q226 339 200 295L180 288L194 279L178 270L193 261Q177 224 192 188Z" fill="url(#fur)"/><path d="M192 230Q237 216 274 250L306 286L335 250Q374 218 426 232L425 262L440 272L421 278L431 288L410 291Q387 336 306 350Q226 339 200 295L180 288L194 279L178 270L193 261Z" fill="url(#cream)"/><path d="M287 165L305 188L317 166" fill="#d38251" opacity=".65"/>`,
      6,
      headPivot,
      "head",
      headPivot,
    );
    await part(
      "eyes",
      "Eyes",
      `<ellipse cx="254" cy="252" rx="8" ry="13" fill="#3a3c38"/><ellipse cx="354" cy="252" rx="8" ry="13" fill="#3a3c38"/><circle cx="257" cy="248" r="2.7" fill="#fff6e5"/><circle cx="357" cy="248" r="2.7" fill="#fff6e5"/>`,
      7,
      [304, 252],
      "head",
      headPivot,
    );
    await part(
      "expression",
      "Nose & smile",
      `<path d="M294 280Q306 275 318 280Q318 284 307 291Q295 287 294 280Z" fill="#514139"/><path d="M306 291V300M288 298Q298 310 306 300Q314 310 325 298" fill="none" stroke="#795c49" stroke-width="2.5" stroke-linecap="round"/><ellipse cx="226" cy="277" rx="15" ry="7" fill="#d89072" opacity=".48"/><ellipse cx="382" cy="277" rx="15" ry="7" fill="#d89072" opacity=".48"/><path d="M242 231Q252 225 264 228M344 228Q355 225 366 231" fill="none" stroke="#835b42" stroke-width="3" stroke-linecap="round"/>`,
      7,
      headPivot,
      "head",
      headPivot,
    );
    await part(
      "scarf-tail",
      "Scarf ribbon",
      `<path d="M318 343Q358 350 369 383L355 446L334 427L319 439L321 382L299 364Z" fill="#cc735c"/><path d="M324 364Q346 384 345 424" fill="none" stroke="#f3b596" stroke-width="4"/><path d="M335 430L343 413M352 441L357 424" stroke="#975243" stroke-width="2"/>`,
      8,
      [312, 350],
    );
    await part(
      "scarf",
      "Scarf collar",
      `<path d="M236 326Q298 352 370 326L367 349Q308 378 240 349Z" fill="#de9575"/><path d="M247 336Q308 357 357 337" stroke="#efb795" stroke-width="3" fill="none"/><path d="M321 342Q335 341 338 352Q336 365 322 365Q313 352 321 342Z" fill="#bd6855"/>`,
      9,
      headPivot,
      "head",
      headPivot,
    );
    await part(
      "badge",
      "Little explorer badge",
      `<circle cx="341" cy="401" r="13" fill="#e6c890"/><path d="M331 405L339 393L350 407H331Z" fill="#5d7967"/><path d="M337 396L339 393L342 397" fill="#fff0d0"/>`,
      10,
      bodyPivot,
      "body",
      bodyPivot,
    );
    async function control(
      id: string,
      name: string,
      node: string,
      target: ScalarTarget,
      values: [number, number, number],
    ) {
      await session.send({
        cmd: "param_add",
        param: id,
        name,
        min: -1,
        max: 1,
        default: 0,
      });
      await session.send({ cmd: "edit_apply", if_rev: session.getRevision(), edits: [
        { op: "binding_add", param: id, node, target, key_positions: [[0, 0.5, 1]] },
        { op: "binding_cells_set", param: id, node, target, cells: values.map((value, i) => ({ cell: [i, 0], value: { scalar: value } })) },
      ] });
    }
    await control("head-tilt", "Head tilt", "head", "rz", [-0.18, 0, 0.18]);
    await control("look", "Look around", "eyes", "tx", [-10, 0, 10]);
    await control("blink", "Blink", "eyes", "sy", [0.08, 1, 1.1]);
    await control("wave", "Say hello", "arm-right", "rz", [-0.28, 0, 0.28]);
    await control("tail-sway", "Tail sway", "tail", "rz", [-0.14, 0, 0.17]);
    await control("breathe", "Breathe", "body", "sy", [0.97, 1, 1.03]);
    await editor.saveSession(session, "Mica.clm");
    const saved = await editor.readFile("Mica.clm");
    if (!saved) throw new Error("The starter generator needs the in-tab backend.");
    return Array.from(saved);
  });
  await Bun.write(new URL("../public/sample.clm", import.meta.url), new Uint8Array(bytes));
  console.log(`Built Mica: ${bytes.length.toLocaleString()} bytes, 13 parts and 6 pose controls.`);
} finally {
  await browser.close();
}
