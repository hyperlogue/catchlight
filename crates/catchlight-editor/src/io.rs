//! Where document bytes live. Native flows hand the server a filesystem path
//! (the OS dialog + the server's path commands own the bytes); web flows move
//! bytes through here — async picker in, blob download out, startup fetch of
//! the staged demo model. No other GUI module touches file bytes.

use std::sync::{Arc, Mutex};

use eframe::egui;

pub enum IoEvent {
    Opened {
        title: String,
        bytes: Vec<u8>,
    },
    /// The staged demo model — only adopted when nothing else is open.
    DemoLoaded {
        title: String,
        bytes: Vec<u8>,
    },
    PickedTexture {
        bytes: Vec<u8>,
        is_tga: bool,
    },
    /// A previous session's autosave exists — offer to restore it.
    AutosaveFound {
        bytes: Vec<u8>,
    },
    Status(String),
    Error(String),
}

/// Completed async IO lands here; the app drains it once per frame. Pushing
/// requests a repaint so results are picked up without polling.
pub struct IoQueue {
    events: Mutex<Vec<IoEvent>>,
    ctx: egui::Context,
}

impl IoQueue {
    pub fn new(ctx: egui::Context) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            ctx,
        })
    }

    pub fn push(&self, event: IoEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
        self.ctx.request_repaint();
    }

    pub fn drain(&self) -> Vec<IoEvent> {
        match self.events.lock() {
            Ok(mut events) => std::mem::take(&mut *events),
            Err(_) => Vec::new(),
        }
    }
}

/// Async `.clp` picker; the picked file's bytes arrive as [`IoEvent::Opened`].
#[cfg(target_arch = "wasm32")]
pub fn pick_clp(queue: Arc<IoQueue>) {
    wasm_bindgen_futures::spawn_local(async move {
        let Some(file) = rfd::AsyncFileDialog::new()
            .add_filter("catchlight puppet", &["clp"])
            .pick_file()
            .await
        else {
            return;
        };
        let title = file.file_name().trim_end_matches(".clp").to_string();
        let bytes = file.read().await;
        queue.push(IoEvent::Opened { title, bytes });
    });
}

/// Async PNG/TGA picker for the textures panel.
#[cfg(target_arch = "wasm32")]
pub fn pick_texture(queue: Arc<IoQueue>) {
    wasm_bindgen_futures::spawn_local(async move {
        let Some(file) = rfd::AsyncFileDialog::new()
            .add_filter("image", &["png", "tga"])
            .pick_file()
            .await
        else {
            return;
        };
        let is_tga = file.file_name().to_lowercase().ends_with(".tga");
        let bytes = file.read().await;
        queue.push(IoEvent::PickedTexture { bytes, is_tga });
    });
}

/// Fetch the demo model staged next to the page; silently absent when the
/// deployment ships no model.
#[cfg(target_arch = "wasm32")]
pub fn fetch_demo(queue: Arc<IoQueue>, url: &'static str) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(bytes) = fetch_bytes(url).await {
            let title = url
                .rsplit('/')
                .next()
                .unwrap_or(url)
                .trim_end_matches(".clp")
                .to_string();
            queue.push(IoEvent::DemoLoaded { title, bytes });
        }
    });
}

#[cfg(target_arch = "wasm32")]
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;

    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let resp = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await?;
    let resp: web_sys::Response = resp.dyn_into()?;
    if !resp.ok() {
        return Err(JsValue::from_str("fetch failed"));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer()?).await?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Single-slot autosave in the browser's origin-private file system. Fire and
/// forget; failures surface on the status line.
#[cfg(target_arch = "wasm32")]
pub fn autosave_write(queue: Arc<IoQueue>, bytes: Vec<u8>) {
    wasm_bindgen_futures::spawn_local(async move {
        match opfs_write("autosave.clp", &bytes).await {
            Ok(()) => queue.push(IoEvent::Status("autosaved".into())),
            Err(_) => queue.push(IoEvent::Error("autosave failed (no OPFS?)".into())),
        }
    });
}

/// Look for a previous session's autosave at startup.
#[cfg(target_arch = "wasm32")]
pub fn autosave_probe(queue: Arc<IoQueue>) {
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(bytes) = opfs_read("autosave.clp").await {
            if !bytes.is_empty() {
                queue.push(IoEvent::AutosaveFound { bytes });
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
async fn opfs_root() -> Result<web_sys::FileSystemDirectoryHandle, wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let dir =
        wasm_bindgen_futures::JsFuture::from(window.navigator().storage().get_directory()).await?;
    dir.dyn_into()
}

#[cfg(target_arch = "wasm32")]
async fn opfs_write(name: &str, bytes: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;
    let dir = opfs_root().await?;
    let opts = web_sys::FileSystemGetFileOptions::new();
    opts.set_create(true);
    let handle: web_sys::FileSystemFileHandle =
        wasm_bindgen_futures::JsFuture::from(dir.get_file_handle_with_options(name, &opts))
            .await?
            .dyn_into()?;
    let writable: web_sys::FileSystemWritableFileStream =
        wasm_bindgen_futures::JsFuture::from(handle.create_writable())
            .await?
            .dyn_into()?;
    wasm_bindgen_futures::JsFuture::from(writable.write_with_u8_array(bytes)?).await?;
    wasm_bindgen_futures::JsFuture::from(writable.close()).await?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
async fn opfs_read(name: &str) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast;
    let dir = opfs_root().await?;
    let handle: web_sys::FileSystemFileHandle =
        wasm_bindgen_futures::JsFuture::from(dir.get_file_handle(name))
            .await?
            .dyn_into()?;
    let file: web_sys::File = wasm_bindgen_futures::JsFuture::from(handle.get_file())
        .await?
        .dyn_into()?;
    let buf = wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// Hand `bytes` to the browser as a named download (blob + anchor click).
#[cfg(target_arch = "wasm32")]
pub fn download_bytes(file_name: &str, bytes: &[u8]) -> Result<(), String> {
    use wasm_bindgen::JsCast;

    let err = |what: &str| format!("download: {what}");
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::of1(&array);
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts).map_err(|_| err("blob"))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| err("object url"))?;
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| err("no document"))?;
    let anchor: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|_| err("anchor"))?
        .dyn_into()
        .map_err(|_| err("anchor cast"))?;
    anchor.set_href(&url);
    anchor.set_download(file_name);
    anchor.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}
