use std::sync::{Arc, Mutex};

use catchlight_editor_protocol::*;

use crate::{Attachments, Editor, Payload};

struct Fixture {
    editor: Editor,
    session: SessionId,
    events: Arc<Mutex<Vec<Event>>>,
    next: u64,
}

impl Fixture {
    fn new() -> Self {
        let editor = Editor::new();
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

    fn send(&mut self, command: Command) -> Reply {
        self.next += 1;
        self.editor.handle(Request {
            id: self.next,
            command,
        })
    }

    fn ok(&mut self, command: Command) -> ResponseBody {
        match self.send(command) {
            Reply::Ok { body, .. } => body,
            other => panic!("{other:?}"),
        }
    }

    fn refused(&mut self, command: Command) -> ErrorCode {
        match self.send(command) {
            Reply::Err { code, .. } => code,
            other => panic!("{other:?}"),
        }
    }

    fn rev(&mut self) -> u64 {
        let session = self.session;
        match self.send(Command::ReviewGet { session }) {
            Reply::Ok { rev: Some(rev), .. } => rev,
            other => panic!("{other:?}"),
        }
    }

    fn review(&mut self) -> Review {
        let session = self.session;
        match self.ok(Command::ReviewGet { session }) {
            ResponseBody::Review { review } => review,
            other => panic!("{other:?}"),
        }
    }

    fn review_events(&self) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::ReviewChanged { .. }))
            .count()
    }

    fn group(&mut self, id: &str) -> NodeId {
        let node = NodeId::new(id).unwrap();
        let session = self.session;
        self.ok(Command::NodeAdd {
            session,
            parent: NodeId::new("root").unwrap(),
            kind: NodeKindArg::Group,
            name: None,
            node: Some(node.clone()),
        });
        node
    }

    fn fetch(&mut self, name: &str) -> (Vec<ReviewThread>, Option<String>) {
        let session = self.session;
        match self.ok(Command::ReviewFetch {
            session,
            agent: who(name),
        }) {
            ResponseBody::ReviewFetched { threads, batch } => (threads, batch),
            other => panic!("{other:?}"),
        }
    }

    fn ack(
        &mut self,
        name: &str,
        batch: &str,
    ) -> Result<(Vec<ThreadId>, Vec<ReviewHeld>), ErrorCode> {
        let session = self.session;
        match self.send(Command::ReviewAck {
            session,
            agent: who(name),
            batch: batch.into(),
        }) {
            Reply::Ok {
                body: ResponseBody::ReviewAcked { claimed, held },
                ..
            } => Ok((claimed, held)),
            Reply::Err { code, .. } => Err(code),
            other => panic!("{other:?}"),
        }
    }

    fn claim(&mut self, name: &str, threads: &[u64]) -> Reply {
        let session = self.session;
        self.send(Command::ReviewClaim {
            session,
            agent: who(name),
            threads: threads.iter().copied().map(ThreadId).collect(),
        })
    }

    fn holder(&mut self, thread: usize) -> Option<String> {
        self.review().threads[thread].holder.clone().map(|h| h.name)
    }

    /// Sends the person's comments and hands them to `name`.
    fn take(&mut self, name: &str) -> Vec<ReviewThread> {
        let session = self.session;
        self.ok(Command::ReviewSubmit { session });
        let (threads, batch) = self.fetch(name);
        self.ack(name, &batch.unwrap()).unwrap();
        threads
    }

    fn keyed(&mut self, author: ReviewAuthor, thread: u64, body: &str, key: &str) -> Reply {
        let session = self.session;
        self.send(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(thread),
            author,
            body: body.into(),
            images: Vec::new(),
            key: Some(key.into()),
            version: None,
        })
    }

    fn open(&mut self, target: ReviewTarget, author: ReviewAuthor) -> ReviewThread {
        let session = self.session;
        match self.ok(Command::ReviewThreadAdd {
            session,
            target,
            author,
            body: "  look here \n".into(),
            pose: Vec::new(),
            images: Vec::new(),
            key: None,
            version: None,
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        }
    }
}

fn who(name: &str) -> ReviewIdentity {
    ReviewIdentity {
        id: format!("{name}:session"),
        name: name.into(),
    }
}

fn agent(name: &str) -> ReviewAuthor {
    let who = who(name);
    ReviewAuthor::Agent {
        id: who.id,
        name: who.name,
    }
}

fn human() -> ReviewAuthor {
    ReviewAuthor::Human {
        id: None,
        name: None,
    }
}

fn deliveries(thread: &ReviewThread) -> Vec<ReviewDelivery> {
    thread.comments.iter().map(|c| c.delivery).collect()
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-not-really-a-png";

impl Fixture {
    fn with(&mut self, command: Command, name: &str, bytes: &[u8]) -> (Reply, Option<Payload>) {
        self.next += 1;
        let mut attachments = Attachments::none();
        attachments.insert(name, bytes.to_vec());
        self.editor.handle_with(
            Request {
                id: self.next,
                command,
            },
            attachments,
        )
    }

    fn upload(&mut self, bytes: &[u8]) -> Reply {
        let session = self.session;
        self.with(Command::ReviewImageAdd { session }, "image", bytes)
            .0
    }

    fn image(&mut self, bytes: &[u8]) -> ReviewImage {
        match self.upload(bytes) {
            Reply::Ok {
                body: ResponseBody::ReviewImage { image },
                ..
            } => image,
            other => panic!("{other:?}"),
        }
    }

    fn bytes(&mut self, image: u64) -> Result<Payload, ErrorCode> {
        self.next += 1;
        let session = self.session;
        match self.editor.handle_with(
            Request {
                id: self.next,
                command: Command::ReviewImageGet { session, image },
            },
            Attachments::none(),
        ) {
            (Reply::Ok { .. }, Some(payload)) => Ok(payload),
            (Reply::Err { code, .. }, None) => Err(code),
            other => panic!("{other:?}"),
        }
    }

    fn say(&mut self, images: Vec<u64>, body: &str) -> Reply {
        let session = self.session;
        self.send(Command::ReviewThreadAdd {
            session,
            target: ReviewTarget::Model,
            author: human(),
            body: body.into(),
            pose: Vec::new(),
            images,
            key: None,
            version: None,
        })
    }
}

mod images;
mod kept;
mod state;
