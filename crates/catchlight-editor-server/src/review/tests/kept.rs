use std::collections::BTreeMap;
use std::io;

use super::*;
use crate::kept_store::KeptStore;
use crate::review::document::DOCUMENT;

/// A store that is not the filesystem, as a cloud or browser one would be.
#[derive(Debug, Default)]
struct MemoryStore {
    documents: Mutex<BTreeMap<String, Vec<u8>>>,
    images: Mutex<BTreeMap<(String, String), Vec<u8>>>,
    saves: std::sync::atomic::AtomicUsize,
    refuse: std::sync::atomic::AtomicBool,
}

impl MemoryStore {
    fn check(&self) -> io::Result<()> {
        if self.refuse.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(io::Error::other("store is down"));
        }
        Ok(())
    }

    fn saves(&self) -> usize {
        self.saves.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn image_names(&self, model: &str) -> Vec<String> {
        self.images
            .lock()
            .unwrap()
            .keys()
            .filter(|(m, _)| m == model)
            .map(|(_, name)| name.clone())
            .collect()
    }
}

impl KeptStore for MemoryStore {
    fn read(&self, model: &str, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.check()?;
        Ok(if name == DOCUMENT {
            self.documents.lock().unwrap().get(model).cloned()
        } else {
            self.images
                .lock()
                .unwrap()
                .get(&(model.to_owned(), name.to_owned()))
                .cloned()
        })
    }

    fn write(&self, model: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.check()?;
        if name == DOCUMENT {
            self.saves
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.documents
                .lock()
                .unwrap()
                .insert(model.into(), bytes.to_vec());
        } else {
            self.images
                .lock()
                .unwrap()
                .insert((model.into(), name.into()), bytes.to_vec());
        }
        Ok(())
    }

    fn delete(&self, model: &str, name: &str) -> io::Result<()> {
        self.check()?;
        self.images
            .lock()
            .unwrap()
            .remove(&(model.to_owned(), name.to_owned()));
        Ok(())
    }
}

/// Model bytes in memory, so a test's sessions have keys without files.
#[derive(Debug, Default)]
struct MemoryModels(Mutex<BTreeMap<String, Vec<u8>>>);

impl crate::Storage for MemoryModels {
    fn read(&self, key: &str) -> io::Result<Vec<u8>> {
        self.0
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, key.to_owned()))
    }

    fn write(&self, key: &str, bytes: &[u8]) -> io::Result<()> {
        self.0.lock().unwrap().insert(key.into(), bytes.to_vec());
        Ok(())
    }
}

impl Fixture {
    fn on(editor: Editor, session: SessionId) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        editor
            .subscribe(Box::new(move |event| {
                sink.lock().unwrap().push(event.clone());
            }))
            .detach();
        Self {
            editor,
            session,
            events,
            next: 1,
        }
    }

    /// A new session on an editor that keeps reviews in `store`.
    fn keeping(models: &Arc<MemoryModels>, store: &Arc<MemoryStore>) -> Self {
        let editor = Editor::with_storage(models.clone()).with_kept_store(store.clone());
        let session = match editor.handle(Request {
            id: 1,
            command: Command::SessionNew {
                name: None,
                source: None,
            },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        Self::on(editor, session)
    }

    /// The model at `key`, opened by a fresh editor on the same stores.
    fn reopened(models: &Arc<MemoryModels>, store: &Arc<MemoryStore>, key: &str) -> Self {
        let editor = Editor::with_storage(models.clone()).with_kept_store(store.clone());
        let session = match editor.handle(Request {
            id: 1,
            command: Command::SessionOpen { path: key.into() },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        Self::on(editor, session)
    }

    fn save(&mut self, key: &str) -> Reply {
        let session = self.session;
        self.send(Command::Save {
            session,
            path: Some(key.into()),
        })
    }

    fn thread_with_image(&mut self, bytes: &[u8]) -> ReviewThread {
        let image = self.image(bytes).image;
        match self.say(vec![image], "see this") {
            Reply::Ok {
                body: ResponseBody::ReviewThread { thread },
                ..
            } => thread,
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_kept_review_is_there_when_another_editor_opens_the_model() {
    let (models, store) = Default::default();
    let mut f = Fixture::keeping(&models, &store);
    let session = f.session;
    assert_eq!(f.review().kept, Kept::Unsaved);
    assert!(matches!(f.save("m.clm"), Reply::Ok { .. }));
    assert_eq!(f.review().kept, Kept::Stored);

    let first = f.thread_with_image(PNG);
    let second = f.open(ReviewTarget::Model, human());
    f.take("claude");
    assert!(matches!(
        f.keyed(agent("claude"), second.thread.0, "on it", "retry"),
        Reply::Ok { .. }
    ));
    f.ok(Command::ReviewThreadSet {
        session,
        thread: second.thread,
        status: ThreadStatus::Resolved,
    });
    let before = f.review();

    let mut g = Fixture::reopened(&models, &store, "m.clm");
    let after = g.review();
    assert_eq!(after.kept, Kept::Stored);
    assert_eq!(
        after.agents,
        [],
        "an agent belongs to the session it joined"
    );
    assert_eq!(after.threads.len(), 2);
    for (was, now) in before.threads.iter().zip(&after.threads) {
        assert_eq!(was.thread, now.thread);
        assert_eq!(was.status, now.status);
        assert_eq!(was.comments, now.comments);
        assert_eq!(
            now.holder, None,
            "a claim belongs to the session that took it"
        );
    }
    let image = first.comments[0].images[0].image;
    assert_eq!(g.bytes(image).unwrap().bytes, PNG);
    assert!(
        matches!(
            g.keyed(agent("claude"), second.thread.0, "on it", "retry"),
            Reply::Ok { body: ResponseBody::ReviewThread { thread }, .. } if thread.comments.len() == 2
        ),
        "a retry key outlives the editor"
    );

    let next = g.thread_with_image(PNG);
    assert_eq!(next.thread, ThreadId(3), "thread Ids continue");
    assert_eq!(next.comments[0].comment, 4, "comment Ids continue");
    assert_eq!(
        next.comments[0].images[0].image,
        image + 1,
        "image Ids continue"
    );
}

#[test]
fn a_store_that_refuses_leaves_the_review_as_it_was() {
    let (models, store) = Default::default();
    let mut f = Fixture::keeping(&models, &store);
    let session = f.session;
    f.save("m.clm");
    let thread = f.thread_with_image(PNG);
    let held = f.image(PNG).image;
    let before = f.review();
    let events = f.review_events();

    store
        .refuse
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(
        f.say(vec![held], "lost?"),
        Reply::Err {
            code: ErrorCode::Io,
            ..
        }
    ));
    assert_eq!(
        f.refused(Command::ReviewThreadDelete {
            session,
            thread: thread.thread
        }),
        ErrorCode::Io
    );
    assert_eq!(f.review(), before);
    assert_eq!(f.review_events(), events);

    store
        .refuse
        .store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(
        matches!(f.say(vec![held], "kept"), Reply::Ok { .. }),
        "the refused comment's image is still held"
    );
    assert_eq!(
        Fixture::reopened(&models, &store, "m.clm")
            .review()
            .threads
            .len(),
        2
    );
}

#[test]
fn only_a_change_to_what_is_kept_is_written() {
    let (models, store) = Default::default();
    let mut f = Fixture::keeping(&models, &store);
    let session = f.session;
    f.save("m.clm");
    f.ok(Command::ReviewPoll {
        session,
        agent: who("claude"),
    });
    f.review();
    assert_eq!(store.saves(), 0, "an empty review is not written");

    f.open(ReviewTarget::Model, human());
    assert_eq!(store.saves(), 1);
    f.image(PNG);
    f.ok(Command::ReviewPoll {
        session,
        agent: who("claude"),
    });
    f.ok(Command::ReviewFetch {
        session,
        agent: who("claude"),
    });
    f.save("m.clm");
    assert_eq!(
        store.saves(),
        1,
        "presence, a held image and a re-save keep nothing new"
    );
    assert!(
        store.image_names("m.clm").is_empty(),
        "a held image is not kept"
    );

    f.ok(Command::ReviewSubmit { session });
    assert_eq!(store.saves(), 2);
}

#[test]
fn deleting_a_comment_drops_its_kept_images() {
    let (models, store) = Default::default();
    let mut f = Fixture::keeping(&models, &store);
    let session = f.session;
    f.save("m.clm");
    let thread = f.thread_with_image(PNG);
    f.thread_with_image(PNG);
    assert_eq!(store.image_names("m.clm"), ["1.png", "2.png"]);

    f.ok(Command::ReviewThreadDelete {
        session,
        thread: thread.thread,
    });
    assert_eq!(store.image_names("m.clm"), ["2.png"]);
}

#[test]
fn a_review_follows_its_model_to_a_new_key() {
    let (models, store) = Default::default();
    let mut f = Fixture::keeping(&models, &store);
    let thread = f.thread_with_image(PNG);
    assert_eq!(
        store.saves(),
        0,
        "a model with no file keeps its review in memory"
    );
    let events = f.review_events();

    assert!(matches!(f.save("a.clm"), Reply::Ok { .. }));
    assert_eq!(f.review_events(), events + 1, "how long it is kept changed");
    assert_eq!(store.image_names("a.clm"), ["1.png"]);

    // Reopened, the image is only in the store: saving elsewhere must
    // still carry it.
    let mut g = Fixture::reopened(&models, &store, "a.clm");
    assert!(matches!(g.save("b.clm"), Reply::Ok { .. }));
    let mut h = Fixture::reopened(&models, &store, "b.clm");
    assert_eq!(h.review().threads[0].comments, thread.comments);
    assert_eq!(
        h.bytes(thread.comments[0].images[0].image).unwrap().bytes,
        PNG
    );
    assert_eq!(
        Fixture::reopened(&models, &store, "a.clm")
            .review()
            .threads
            .len(),
        1,
        "the review left at the old key is untouched"
    );
}

#[test]
fn a_save_whose_review_is_refused_still_saved_the_model() {
    let (models, store) = Default::default();
    let mut f = Fixture::keeping(&models, &store);
    f.open(ReviewTarget::Model, human());
    store
        .refuse
        .store(true, std::sync::atomic::Ordering::Relaxed);
    match f.save("m.clm") {
        Reply::Err {
            code: ErrorCode::Io,
            message,
            ..
        } => assert!(message.contains("saved the model to m.clm"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert!(models.0.lock().unwrap().contains_key("m.clm"));

    store
        .refuse
        .store(false, std::sync::atomic::Ordering::Relaxed);
    f.open(ReviewTarget::Model, human());
    assert_eq!(
        Fixture::reopened(&models, &store, "m.clm")
            .review()
            .threads
            .len(),
        2,
        "the next change writes everything the refused save did not"
    );
}

#[test]
fn a_review_that_cannot_be_read_is_left_alone() {
    let (models, store): (Arc<MemoryModels>, Arc<MemoryStore>) = Default::default();
    Fixture::keeping(&models, &store).save("m.clm");
    for bad in [&b"{ not json"[..], br#"{"version": 99, "threads": []}"#] {
        store
            .documents
            .lock()
            .unwrap()
            .insert("m.clm".into(), bad.to_vec());
        let mut f = Fixture::reopened(&models, &store, "m.clm");
        let review = f.review();
        assert_eq!(review.kept, Kept::Unreadable);
        assert!(review.kept_problem.is_some());
        assert!(review.threads.is_empty());

        f.open(ReviewTarget::Model, human());
        f.save("m.clm");
        assert_eq!(
            f.review().threads.len(),
            1,
            "comments still work, in memory"
        );
        assert_eq!(store.documents.lock().unwrap()["m.clm"], bad);

        assert!(matches!(f.save("copy.clm"), Reply::Ok { .. }));
        assert_eq!(f.review().kept, Kept::Stored, "a new key has no bad file");
    }
}

#[test]
fn a_kept_thread_finds_a_target_renamed_after_the_last_save() {
    let (models, store) = Default::default();
    let mut f = Fixture::keeping(&models, &store);
    let session = f.session;
    let node = f.group("arm");
    f.save("m.clm");
    f.open(ReviewTarget::Node { node: node.clone() }, human());
    let renamed = NodeId::new("limb").unwrap();
    f.ok(Command::RenameId {
        session,
        rename: Rename::Node {
            from: node.clone(),
            to: renamed,
        },
    });

    // The file still calls it `arm`; the kept thread says `limb`.
    let mut g = Fixture::reopened(&models, &store, "m.clm");
    let thread = &g.review().threads[0];
    assert_eq!(thread.target, ReviewTarget::Node { node });
    assert!(!thread.target_missing);
}

#[test]
fn an_editor_without_a_review_store_says_reviews_last_for_the_session() {
    let models: Arc<MemoryModels> = Default::default();
    let editor = Editor::with_storage(models);
    let session = match editor.handle(Request {
        id: 1,
        command: Command::SessionNew {
            name: None,
            source: None,
        },
    }) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("{other:?}"),
    };
    let mut f = Fixture::on(editor, session);
    f.thread_with_image(PNG);
    assert!(matches!(f.save("m.clm"), Reply::Ok { .. }));
    assert_eq!(f.review().kept, Kept::Session);
}

#[test]
fn the_sidecar_is_a_directory_beside_the_model() {
    use crate::{FileStorage, SidecarKeptStore};
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("cl-review-sidecar-{}-{nth}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(dir.join("models")).unwrap();
    let editor = || {
        Editor::with_storage(Arc::new(FileStorage::new(&dir).unwrap()))
            .with_kept_store(Arc::new(SidecarKeptStore::new(&dir)))
    };
    let first = editor();
    let session = match first.handle(Request {
        id: 1,
        command: Command::SessionNew {
            name: None,
            source: None,
        },
    }) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("{other:?}"),
    };
    let mut f = Fixture::on(first, session);
    f.save("models/m.clm");
    let thread = f.thread_with_image(PNG);

    let sidecar = dir.join("models/m.clm.kept");
    let mut names: Vec<String> = std::fs::read_dir(&sidecar)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["1.png", "review.json"]);
    assert_eq!(std::fs::read(sidecar.join("1.png")).unwrap(), PNG);

    let second = editor();
    let session = match second.handle(Request {
        id: 1,
        command: Command::SessionOpen {
            path: "models/m.clm".into(),
        },
    }) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("{other:?}"),
    };
    let mut g = Fixture::on(second, session);
    assert_eq!(g.review().threads[0].comments, thread.comments);
    assert_eq!(g.bytes(1).unwrap().bytes, PNG);
    g.ok(Command::ReviewThreadDelete {
        session,
        thread: thread.thread,
    });
    assert!(!sidecar.join("1.png").exists());

    std::fs::write(sidecar.join("review.json"), b"torn").unwrap();
    let third = editor();
    let session = match third.handle(Request {
        id: 1,
        command: Command::SessionOpen {
            path: "models/m.clm".into(),
        },
    }) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("{other:?}"),
    };
    let mut h = Fixture::on(third, session);
    let review = h.review();
    assert_eq!(review.kept, Kept::Unreadable);
    assert!(
        review.kept_problem.unwrap().contains("review.json"),
        "the problem names the file to look at"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_user_store_keeps_one_directory_per_model_and_prunes_deleted_ones() {
    use crate::{FileStorage, UserKeptStore};
    use std::time::Duration;
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nth = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("cl-review-user-{}-{nth}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    let models = dir.join("models");
    let reviews = dir.join("reviews");
    std::fs::create_dir_all(&models).unwrap();
    let store = UserKeptStore::new(&models, &reviews);
    let editor = || {
        Editor::with_storage(Arc::new(FileStorage::new(&models).unwrap()))
            .with_kept_store(Arc::new(store.clone()))
    };
    let open = |editor: Editor, path: &str| {
        let session = match editor.handle(Request {
            id: 1,
            command: Command::SessionOpen { path: path.into() },
        }) {
            Reply::Ok {
                body: ResponseBody::Session { session },
                ..
            } => session,
            other => panic!("{other:?}"),
        };
        Fixture::on(editor, session)
    };
    let listing = |dir: &std::path::Path| {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    let first = editor();
    let session = match first.handle(Request {
        id: 1,
        command: Command::SessionNew {
            name: None,
            source: None,
        },
    }) {
        Reply::Ok {
            body: ResponseBody::Session { session },
            ..
        } => session,
        other => panic!("{other:?}"),
    };
    let mut f = Fixture::on(first, session);
    f.save("m.clm");
    let thread = f.thread_with_image(PNG);

    assert_eq!(listing(&models), ["m.clm"], "nothing beside the model");
    let ids = listing(&reviews);
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0].len(), 32);
    let kept = reviews.join(&ids[0]);
    assert_eq!(listing(&kept), ["1.png", "model", "review.json"]);
    assert_eq!(std::fs::read(kept.join("1.png")).unwrap(), PNG);
    let m = std::fs::canonicalize(models.join("m.clm")).unwrap();
    assert_eq!(
        std::fs::read_to_string(kept.join("model")).unwrap(),
        m.to_str().unwrap()
    );
    assert!(f.review().kept_problem.is_none());

    // A relative key and the absolute path reach the same review.
    for path in ["m.clm", m.to_str().unwrap()] {
        let mut g = open(editor(), path);
        assert_eq!(g.review().threads[0].comments, thread.comments);
        assert_eq!(g.bytes(1).unwrap().bytes, PNG);
    }
    assert_eq!(listing(&reviews), ids);

    // Another model gets its own directory.
    f.save("n.clm");
    let both = listing(&reviews);
    assert_eq!(both.len(), 2);
    let n = reviews.join(both.iter().find(|id| **id != ids[0]).unwrap());
    assert_eq!(listing(&n), ["1.png", "model", "review.json"]);

    // Neither a recently used review nor one whose model is still there
    // is pruned, nor a directory that does not say whose it is.
    std::fs::create_dir_all(reviews.join("unknown")).unwrap();
    std::fs::remove_file(models.join("n.clm")).unwrap();
    assert_eq!(store.unused(Duration::from_secs(3600)).unwrap(), []);
    let unused = store.unused(Duration::ZERO).unwrap();
    assert_eq!(unused.len(), 1);
    assert_eq!(unused[0].dir, n);
    assert_eq!(unused[0].model, m.with_file_name("n.clm"));
    assert_eq!(
        unused[0].bytes,
        ["1.png", "model", "review.json"]
            .map(|f| std::fs::metadata(n.join(f)).unwrap().len())
            .iter()
            .sum::<u64>()
    );
    assert!(n.exists(), "listing removes nothing");

    // A model that comes back between listing and removal keeps its review.
    std::fs::write(models.join("n.clm"), b"").unwrap();
    assert!(store.remove(&unused[0]).is_err());
    assert!(n.exists());
    std::fs::remove_file(models.join("n.clm")).unwrap();

    assert_eq!(store.prune(Duration::ZERO).unwrap(), unused);
    assert!(!n.exists());
    assert!(kept.exists());
    assert!(reviews.join("unknown").exists());
    std::fs::remove_dir_all(&dir).ok();
}
