/** Startup-only feedback; after React mounts, these elements no longer exist. */
export function showLoading(label: string, detail: string, fraction?: number): void {
  const title = document.getElementById("loading-label");
  const description = document.getElementById("loading-detail");
  const bar = document.getElementById("loading-progress") as HTMLProgressElement | null;
  if (title) title.textContent = label;
  if (description) description.textContent = detail;
  if (bar) {
    bar.setAttribute("aria-label", label);
    if (fraction === undefined) bar.removeAttribute("value");
    else bar.value = fraction;
  }
}

/** Fetch counts decoded bytes, so a compressed Content-Length is not a total. */
export async function downloadModel(url: string, name: string): Promise<Uint8Array> {
  showLoading("Downloading model…", name);
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`fetching ${url} failed: ${response.status} ${response.statusText}`);
  }
  const length = Number(response.headers.get("content-length"));
  const encoding = response.headers.get("content-encoding");
  const total = (!encoding || encoding === "identity") && Number.isFinite(length) && length > 0 ? length : undefined;
  const describe = (received: number) => {
    const size = `${(received / 1048576).toFixed(1)} MB`;
    showLoading("Downloading model…", total !== undefined && received <= total
      ? `${name} · ${size} / ${(total / 1048576).toFixed(1)} MB · ${Math.floor(received / total * 100)}%`
      : `${name} · ${size} received`, total !== undefined && received <= total ? received / total : undefined);
  };
  describe(0);
  if (!response.body) return new Uint8Array(await response.arrayBuffer());
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let received = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      received += value.length;
      describe(received);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(received);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}

/** Let the browser paint the processing state before synchronous wasm work. */
export async function showProcessing(name: string): Promise<void> {
  showLoading("Processing model…", `${name} · Preparing the model and textures`);
  await new Promise<void>(resolve => requestAnimationFrame(() => setTimeout(resolve, 0)));
}
