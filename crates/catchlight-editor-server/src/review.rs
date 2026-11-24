//! A session's review threads. Kept beside the model, never in it: no
//! revision, no history entry, nothing saved.

use std::time::Duration;

use catchlight_core::{Model, ModelError};
use catchlight_editor_protocol::*;

use crate::{Captured, Editor, EditorError};

/// A `review watch` polls about once a second; a few missed beats is a gone
/// agent, not a slow one.
const LISTENING_FOR: Duration = Duration::from_secs(5);
/// An agent that took a thread and died must not hold it forever.
const WORKING_FOR: Duration = Duration::from_secs(30 * 60);
const MAX_AGENT_NAME_BYTES: usize = 64;
const MAX_RENAMES: usize = 256;

/// `Instant` panics on wasm32-unknown-unknown, where no agent can attach
/// anyway: there nothing is ever recent and nothing ever lapses.
#[derive(Debug, Clone, Copy)]
struct Stamp(#[cfg(not(target_arch = "wasm32"))] std::time::Instant);

impl Stamp {
    fn now() -> Self {
        Self(
            #[cfg(not(target_arch = "wasm32"))]
            std::time::Instant::now(),
        )
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn age(self) -> Option<Duration> {
        Some(self.0.elapsed())
    }

    #[cfg(target_arch = "wasm32")]
    fn age(self) -> Option<Duration> {
        None
    }

    fn within(self, window: Duration) -> bool {
        self.age().is_some_and(|age| age < window)
    }

    fn older_than(self, window: Duration) -> bool {
        self.age().is_some_and(|age| age >= window)
    }
}

#[derive(Debug)]
struct Working {
    agent: String,
    since: Stamp,
}

#[derive(Debug)]
struct Thread {
    id: ThreadId,
    target: ReviewTarget,
    status: ThreadStatus,
    pose: Vec<ParamPose>,
    comments: Vec<ReviewComment>,
    working: Option<Working>,
}

impl Thread {
    fn handed_over(&self) -> bool {
        self.status == ThreadStatus::Open
            && self
                .comments
                .iter()
                .any(|c| c.delivery == ReviewDelivery::Sent)
    }

    fn wire(&self, model: &Model) -> ReviewThread {
        ReviewThread {
            thread: self.id,
            target_missing: !exists(model, &self.target),
            target: self.target.clone(),
            status: self.status,
            pose: self.pose.clone(),
            comments: self.comments.clone(),
            working: self
                .working
                .as_ref()
                .filter(|w| !w.since.older_than(WORKING_FOR))
                .map(|w| w.agent.clone()),
        }
    }
}

#[derive(Debug)]
struct Agent {
    name: String,
    polled: Option<Stamp>,
}

impl Agent {
    fn listening(&self) -> bool {
        self.polled.is_some_and(|at| at.within(LISTENING_FOR))
    }
}

#[derive(Debug, Default)]
pub(crate) struct ReviewState {
    threads: Vec<Thread>,
    last_thread: u64,
    last_comment: u64,
    agent: Option<Agent>,
    /// What [`Self::follow_model`] walks to find where a renamed target went
    /// once history navigation moves the model under the threads.
    renames: Vec<Rename>,
    /// Set by [`Self::rename`], which runs inside an edit and cannot notify.
    touched: bool,
}

fn exists(model: &Model, target: &ReviewTarget) -> bool {
    match target {
        ReviewTarget::Model => true,
        ReviewTarget::Node { node } => model.node(node).is_some(),
        ReviewTarget::Param { param } => model.param(param).is_some(),
        ReviewTarget::Texture { texture } => model.texture(texture).is_some(),
        ReviewTarget::Slot { node, slot } => model.slot(node, slot).is_some(),
    }
}

fn require(model: &Model, target: &ReviewTarget) -> Result<(), EditorError> {
    match target {
        ReviewTarget::Model => Ok(()),
        ReviewTarget::Node { node } => model
            .node(node)
            .map(drop)
            .ok_or_else(|| EditorError::NoNode(node.clone())),
        ReviewTarget::Param { param } => model
            .param(param)
            .map(drop)
            .ok_or_else(|| EditorError::NoParam(param.clone())),
        ReviewTarget::Texture { texture } => model
            .texture(texture)
            .map(drop)
            .ok_or_else(|| EditorError::NoTexture(texture.clone())),
        ReviewTarget::Slot { node, slot } => {
            if model.node(node).is_none() {
                return Err(EditorError::NoNode(node.clone()));
            }
            model
                .slot(node, slot)
                .map(drop)
                .ok_or(EditorError::Edit(ModelError::UnknownSlot))
        }
    }
}

/// `target` as `rename` leaves it, run forward or (`back`) undone.
fn renamed(target: &ReviewTarget, rename: &Rename, back: bool) -> Option<ReviewTarget> {
    fn pick<'a, T>(from: &'a T, to: &'a T, back: bool) -> (&'a T, &'a T) {
        if back {
            (to, from)
        } else {
            (from, to)
        }
    }
    match (target, rename) {
        (ReviewTarget::Node { node }, Rename::Node { from, to }) => {
            let (old, new) = pick(from, to, back);
            (node == old).then(|| ReviewTarget::Node { node: new.clone() })
        }
        (ReviewTarget::Slot { node, slot }, Rename::Node { from, to }) => {
            let (old, new) = pick(from, to, back);
            (node == old).then(|| ReviewTarget::Slot {
                node: new.clone(),
                slot: slot.clone(),
            })
        }
        (ReviewTarget::Param { param }, Rename::Param { from, to }) => {
            let (old, new) = pick(from, to, back);
            (param == old).then(|| ReviewTarget::Param { param: new.clone() })
        }
        (ReviewTarget::Texture { texture }, Rename::Texture { from, to }) => {
            let (old, new) = pick(from, to, back);
            (texture == old).then(|| ReviewTarget::Texture {
                texture: new.clone(),
            })
        }
        (
            ReviewTarget::Slot { node, slot },
            Rename::Slot {
                node: part,
                from,
                to,
            },
        ) => {
            let (old, new) = pick(from, to, back);
            (node == part && slot == old).then(|| ReviewTarget::Slot {
                node: node.clone(),
                slot: new.clone(),
            })
        }
        _ => None,
    }
}

fn body_of(body: String) -> Result<String, EditorError> {
    let body = body.trim();
    if body.is_empty() {
        return Err(EditorError::BadRequest("a comment needs a body".into()));
    }
    Ok(body.to_owned())
}

fn agent_name(name: &str) -> Result<&str, EditorError> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_AGENT_NAME_BYTES {
        return Err(EditorError::BadRequest(format!(
            "an agent name is 1 to {MAX_AGENT_NAME_BYTES} bytes"
        )));
    }
    Ok(name)
}

fn author_of(author: ReviewAuthor) -> Result<ReviewAuthor, EditorError> {
    Ok(match author {
        ReviewAuthor::Human => ReviewAuthor::Human,
        ReviewAuthor::Agent { name } => ReviewAuthor::Agent {
            name: agent_name(&name)?.to_owned(),
        },
    })
}

impl ReviewState {
    pub(crate) fn wire(&self, model: &Model) -> Review {
        Review {
            threads: self.threads.iter().map(|t| t.wire(model)).collect(),
            agent: self.agent.as_ref().map(|a| ReviewAgent {
                name: a.name.clone(),
                listening: a.listening(),
            }),
        }
    }

    /// Which targets the model no longer holds. Compared across an edit to
    /// decide whether clients must re-read.
    pub(crate) fn missing(&self, model: &Model) -> Vec<bool> {
        self.threads
            .iter()
            .map(|t| !exists(model, &t.target))
            .collect()
    }

    fn thread_mut(&mut self, id: ThreadId) -> Result<&mut Thread, EditorError> {
        self.threads
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(EditorError::NoThread(id))
    }

    fn comment(&mut self, author: ReviewAuthor, body: String, rev: u64) -> ReviewComment {
        self.last_comment += 1;
        ReviewComment {
            comment: self.last_comment,
            delivery: match author {
                ReviewAuthor::Human => ReviewDelivery::Unsent,
                ReviewAuthor::Agent { .. } => ReviewDelivery::Delivered,
            },
            author,
            body,
            rev,
        }
    }

    fn open(
        &mut self,
        model: &Model,
        rev: u64,
        target: ReviewTarget,
        author: ReviewAuthor,
        body: String,
        pose: Vec<ParamPose>,
    ) -> Result<ReviewThread, EditorError> {
        let body = body_of(body)?;
        let author = author_of(author)?;
        require(model, &target)?;
        let comment = self.comment(author, body, rev);
        self.last_thread += 1;
        let thread = Thread {
            id: ThreadId(self.last_thread),
            target,
            status: ThreadStatus::Open,
            pose,
            comments: vec![comment],
            working: None,
        };
        let wire = thread.wire(model);
        self.threads.push(thread);
        Ok(wire)
    }

    fn reply(
        &mut self,
        model: &Model,
        rev: u64,
        id: ThreadId,
        author: ReviewAuthor,
        body: String,
    ) -> Result<ReviewThread, EditorError> {
        let body = body_of(body)?;
        let author = author_of(author)?;
        self.thread_mut(id)?;
        let human = author == ReviewAuthor::Human;
        let comment = self.comment(author, body, rev);
        let thread = self.thread_mut(id)?;
        thread.comments.push(comment);
        if human {
            thread.status = ThreadStatus::Open;
        } else {
            thread.working = None;
        }
        Ok(thread.wire(model))
    }

    /// The reply and whether anything changed.
    fn set_status(
        &mut self,
        model: &Model,
        id: ThreadId,
        status: ThreadStatus,
    ) -> Result<(ReviewThread, bool), EditorError> {
        let thread = self.thread_mut(id)?;
        let changed = thread.status != status;
        thread.status = status;
        if status == ThreadStatus::Resolved {
            thread.working = None;
        }
        Ok((thread.wire(model), changed))
    }

    fn delete_thread(&mut self, id: ThreadId) -> Result<(), EditorError> {
        let at = self
            .threads
            .iter()
            .position(|t| t.id == id)
            .ok_or(EditorError::NoThread(id))?;
        self.threads.remove(at);
        Ok(())
    }

    fn delete_comment(&mut self, id: ThreadId, comment: u64) -> Result<(), EditorError> {
        let thread = self.thread_mut(id)?;
        let at = thread
            .comments
            .iter()
            .position(|c| c.comment == comment)
            .ok_or(EditorError::NoComment(comment))?;
        thread.comments.remove(at);
        if thread.comments.is_empty() {
            self.delete_thread(id)?;
        }
        Ok(())
    }

    fn submit(&mut self) -> bool {
        let mut changed = false;
        for thread in &mut self.threads {
            if thread.status != ThreadStatus::Open {
                continue;
            }
            for comment in &mut thread.comments {
                if comment.delivery == ReviewDelivery::Unsent {
                    comment.delivery = ReviewDelivery::Sent;
                    changed = true;
                }
            }
        }
        changed
    }

    fn pending(&self) -> u32 {
        let count = self.threads.iter().filter(|t| t.handed_over()).count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }

    /// Whether what [`Self::wire`] says about the agent changed.
    fn poll(&mut self, name: &str) -> bool {
        let was = self
            .agent
            .as_ref()
            .is_some_and(|a| a.name == name && a.listening());
        let polled = Some(Stamp::now());
        let agent = Agent {
            name: name.to_owned(),
            polled,
        };
        let now = agent.listening();
        let renamed = self.agent.as_ref().is_none_or(|a| a.name != name);
        self.agent = Some(agent);
        renamed || was != now
    }

    /// The threads taken, as they were handed over: the comments this fetch
    /// delivers still read `sent`, which is how a reader tells them from the
    /// thread's history. Second is whether anything changed.
    fn fetch(&mut self, model: &Model, name: &str) -> (Vec<ReviewThread>, bool) {
        let mut taken = Vec::new();
        for thread in &mut self.threads {
            if !thread.handed_over() {
                continue;
            }
            thread.working = Some(Working {
                agent: name.to_owned(),
                since: Stamp::now(),
            });
            taken.push(thread.wire(model));
            for comment in &mut thread.comments {
                if comment.delivery == ReviewDelivery::Sent {
                    comment.delivery = ReviewDelivery::Delivered;
                }
            }
        }
        let known = self.agent.as_ref().is_some_and(|a| a.name == name);
        let changed = !taken.is_empty() || !known;
        if !known || !taken.is_empty() {
            // An agent that took work is no longer waiting for it.
            self.agent = Some(Agent {
                name: name.to_owned(),
                polled: None,
            });
        }
        (taken, changed)
    }

    pub(crate) fn take_touched(&mut self) -> bool {
        std::mem::take(&mut self.touched)
    }

    pub(crate) fn rename(&mut self, rename: &Rename) {
        for thread in &mut self.threads {
            if let Some(next) = renamed(&thread.target, rename, false) {
                thread.target = next;
                self.touched = true;
            }
        }
        if !self.threads.is_empty() {
            if self.renames.len() == MAX_RENAMES {
                self.renames.remove(0);
            }
            self.renames.push(rename.clone());
        }
    }

    /// After history navigation: a thread whose target is gone takes the Id
    /// a recorded rename, undone or redone, gives it, if the model holds that.
    pub(crate) fn follow_model(&mut self, model: &Model) -> bool {
        let mut moved = false;
        for thread in &mut self.threads {
            // A chain of renames is walked one link per pass.
            for _ in 0..self.renames.len() {
                if exists(model, &thread.target) {
                    break;
                }
                let next = self.renames.iter().rev().find_map(|rename| {
                    [true, false]
                        .into_iter()
                        .filter_map(|back| renamed(&thread.target, rename, back))
                        .find(|candidate| exists(model, candidate))
                });
                match next {
                    Some(next) => {
                        thread.target = next;
                        moved = true;
                    }
                    None => break,
                }
            }
        }
        moved
    }
}

impl Editor {
    /// Runs `f` under the session lock and, when it reports a change, tells
    /// observers once the lock is released.
    fn review_session(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut ReviewState, &Model, u64) -> Result<(ResponseBody, bool), EditorError>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let captured = self.with_session_captured(id, |s| {
            let rev = s.rev;
            f(&mut s.review, &s.model, rev)
        })?;
        let (body, changed) = captured.value;
        if changed {
            self.notify(Event::ReviewChanged { session: id });
        }
        Ok(Captured {
            value: body,
            rev: captured.rev,
        })
    }

    pub(crate) fn review(&self, command: Command) -> Result<Captured<ResponseBody>, EditorError> {
        match command {
            Command::ReviewGet { session } => self.review_session(session, |review, model, _| {
                Ok((
                    ResponseBody::Review {
                        review: review.wire(model),
                    },
                    false,
                ))
            }),
            Command::ReviewThreadAdd {
                session,
                target,
                author,
                body,
                pose,
            } => self.review_session(session, |review, model, rev| {
                let thread = review.open(model, rev, target, author, body, pose)?;
                Ok((ResponseBody::ReviewThread { thread }, true))
            }),
            Command::ReviewThreadSet {
                session,
                thread,
                status,
            } => self.review_session(session, |review, model, _| {
                let (thread, changed) = review.set_status(model, thread, status)?;
                Ok((ResponseBody::ReviewThread { thread }, changed))
            }),
            Command::ReviewThreadDelete { session, thread } => {
                self.review_session(session, |review, _, _| {
                    review.delete_thread(thread)?;
                    Ok((ResponseBody::Empty, true))
                })
            }
            Command::ReviewCommentAdd {
                session,
                thread,
                author,
                body,
            } => self.review_session(session, |review, model, rev| {
                let thread = review.reply(model, rev, thread, author, body)?;
                Ok((ResponseBody::ReviewThread { thread }, true))
            }),
            Command::ReviewCommentDelete {
                session,
                thread,
                comment,
            } => self.review_session(session, |review, _, _| {
                review.delete_comment(thread, comment)?;
                Ok((ResponseBody::Empty, true))
            }),
            Command::ReviewSubmit { session } => self.review_session(session, |review, _, _| {
                let changed = review.submit();
                Ok((ResponseBody::Empty, changed))
            }),
            Command::ReviewPoll { session, agent } => {
                self.review_session(session, |review, _, _| {
                    let changed = review.poll(agent_name(&agent)?);
                    Ok((
                        ResponseBody::ReviewPending {
                            pending: review.pending(),
                        },
                        changed,
                    ))
                })
            }
            Command::ReviewFetch { session, agent } => {
                self.review_session(session, |review, model, _| {
                    let (threads, changed) = review.fetch(model, agent_name(&agent)?);
                    Ok((ResponseBody::ReviewFetched { threads }, changed))
                })
            }
            other => Err(EditorError::BadRequest(format!(
                "{} is not a review command",
                other.tag()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use std::sync::{Arc, Mutex};

    use super::*;

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
            editor.subscribe(Box::new(move |event| {
                sink.lock().unwrap().push(event.clone());
            }));
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

        fn open(&mut self, target: ReviewTarget, author: ReviewAuthor) -> ReviewThread {
            let session = self.session;
            match self.ok(Command::ReviewThreadAdd {
                session,
                target,
                author,
                body: "  look here \n".into(),
                pose: Vec::new(),
            }) {
                ResponseBody::ReviewThread { thread } => thread,
                other => panic!("{other:?}"),
            }
        }
    }

    fn agent(name: &str) -> ReviewAuthor {
        ReviewAuthor::Agent { name: name.into() }
    }

    fn deliveries(thread: &ReviewThread) -> Vec<ReviewDelivery> {
        thread.comments.iter().map(|c| c.delivery).collect()
    }

    #[test]
    fn a_comment_travels_from_unsent_to_delivered_and_the_reply_ends_the_work() {
        let mut f = Fixture::new();
        let session = f.session;
        let rev = f.rev();

        let opened = f.open(ReviewTarget::Model, ReviewAuthor::Human);
        assert_eq!(opened.thread, ThreadId(1));
        assert_eq!(opened.comments[0].body, "look here");
        assert_eq!(opened.comments[0].comment, 1);
        assert_eq!(deliveries(&opened), [ReviewDelivery::Unsent]);

        assert!(matches!(
            f.ok(Command::ReviewPoll {
                session,
                agent: "claude".into()
            }),
            ResponseBody::ReviewPending { pending: 0 }
        ));
        f.ok(Command::ReviewSubmit { session });
        assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Sent]);
        assert!(matches!(
            f.ok(Command::ReviewPoll {
                session,
                agent: "claude".into()
            }),
            ResponseBody::ReviewPending { pending: 1 }
        ));
        assert_eq!(
            f.review().agent,
            Some(ReviewAgent {
                name: "claude".into(),
                listening: true
            })
        );

        let fetched = match f.ok(Command::ReviewFetch {
            session,
            agent: "claude".into(),
        }) {
            ResponseBody::ReviewFetched { threads } => threads,
            other => panic!("{other:?}"),
        };
        assert_eq!(fetched.len(), 1);
        assert_eq!(deliveries(&fetched[0]), [ReviewDelivery::Sent]);
        assert_eq!(fetched[0].working.as_deref(), Some("claude"));

        let review = f.review();
        assert_eq!(deliveries(&review.threads[0]), [ReviewDelivery::Delivered]);
        assert_eq!(review.threads[0].working.as_deref(), Some("claude"));
        assert_eq!(
            review.agent,
            Some(ReviewAgent {
                name: "claude".into(),
                listening: false
            })
        );
        assert!(matches!(
            f.ok(Command::ReviewFetch {
                session,
                agent: "claude".into()
            }),
            ResponseBody::ReviewFetched { threads } if threads.is_empty()
        ));

        let replied = match f.ok(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: agent("claude"),
            body: "done".into(),
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(replied.working, None);
        assert_eq!(replied.comments[1].comment, 2);
        assert_eq!(
            deliveries(&replied),
            [ReviewDelivery::Delivered, ReviewDelivery::Delivered]
        );

        assert_eq!(f.rev(), rev, "a review moves no revision");
        assert!(matches!(
            f.ok(Command::Status { session }),
            ResponseBody::Status { status } if !status.dirty
        ));
        assert_eq!(
            f.refused(Command::Undo {
                session,
                if_rev: rev
            }),
            ErrorCode::NothingToUndo
        );
    }

    #[test]
    fn every_change_is_announced_and_a_steady_heartbeat_is_not() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
        assert_eq!(f.review_events(), 1);
        f.review();
        assert_eq!(f.review_events(), 1);

        let poll = Command::ReviewPoll {
            session,
            agent: "claude".into(),
        };
        f.ok(poll.clone());
        assert_eq!(f.review_events(), 2);
        f.ok(poll.clone());
        f.ok(poll);
        assert_eq!(f.review_events(), 2);

        f.ok(Command::ReviewSubmit { session });
        assert_eq!(f.review_events(), 3);
        f.ok(Command::ReviewSubmit { session });
        assert_eq!(f.review_events(), 3, "nothing was left to send");

        f.ok(Command::ReviewThreadSet {
            session,
            thread: ThreadId(1),
            status: ThreadStatus::Resolved,
        });
        assert_eq!(f.review_events(), 4);
        f.ok(Command::ReviewThreadDelete {
            session,
            thread: ThreadId(1),
        });
        assert_eq!(f.review_events(), 5);
        assert!(f
            .events
            .lock()
            .unwrap()
            .iter()
            .all(|e| !matches!(e, Event::ModelChanged { .. })));
    }

    #[test]
    fn a_target_must_exist_and_a_body_must_say_something() {
        let mut f = Fixture::new();
        let session = f.session;
        let add = |target| Command::ReviewThreadAdd {
            session,
            target,
            author: ReviewAuthor::Human,
            body: "x".into(),
            pose: Vec::new(),
        };
        let node = NodeId::new("nope").unwrap();
        assert_eq!(
            f.refused(add(ReviewTarget::Node { node: node.clone() })),
            ErrorCode::NoNode
        );
        assert_eq!(
            f.refused(add(ReviewTarget::Param {
                param: ParamId::new("nope").unwrap()
            })),
            ErrorCode::NoParam
        );
        assert_eq!(
            f.refused(add(ReviewTarget::Texture {
                texture: TexId::new("nope").unwrap()
            })),
            ErrorCode::NoTexture
        );
        let group = f.group("g");
        assert_eq!(
            f.refused(add(ReviewTarget::Slot {
                node: group,
                slot: SlotId::new("nope").unwrap()
            })),
            ErrorCode::UnknownSlot
        );
        assert_eq!(
            f.refused(Command::ReviewThreadAdd {
                session,
                target: ReviewTarget::Model,
                author: ReviewAuthor::Human,
                body: " \n".into(),
                pose: Vec::new(),
            }),
            ErrorCode::BadRequest
        );
        assert_eq!(
            f.refused(Command::ReviewCommentAdd {
                session,
                thread: ThreadId(9),
                author: ReviewAuthor::Human,
                body: "x".into(),
            }),
            ErrorCode::NoThread
        );
        assert_eq!(
            f.refused(Command::ReviewPoll {
                session,
                agent: " ".into()
            }),
            ErrorCode::BadRequest
        );
        assert!(f.review().threads.is_empty());
    }

    #[test]
    fn deleting_the_only_comment_deletes_the_thread_and_ids_are_not_reused() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
        assert_eq!(
            f.refused(Command::ReviewCommentDelete {
                session,
                thread: ThreadId(1),
                comment: 7
            }),
            ErrorCode::NoComment
        );
        f.ok(Command::ReviewCommentDelete {
            session,
            thread: ThreadId(1),
            comment: 1,
        });
        assert!(f.review().threads.is_empty());
        assert_eq!(
            f.refused(Command::ReviewThreadDelete {
                session,
                thread: ThreadId(1)
            }),
            ErrorCode::NoThread
        );
        let next = f.open(ReviewTarget::Model, agent("claude"));
        assert_eq!(next.thread, ThreadId(2));
        assert_eq!(next.comments[0].comment, 2);
        assert_eq!(deliveries(&next), [ReviewDelivery::Delivered]);
    }

    #[test]
    fn a_resolved_thread_is_not_handed_over_and_a_human_reply_reopens_it() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
        f.ok(Command::ReviewThreadSet {
            session,
            thread: ThreadId(1),
            status: ThreadStatus::Resolved,
        });
        f.ok(Command::ReviewSubmit { session });
        assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Unsent]);

        let reopened = match f.ok(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: ReviewAuthor::Human,
            body: "still wrong".into(),
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(reopened.status, ThreadStatus::Open);

        f.ok(Command::ReviewSubmit { session });
        f.ok(Command::ReviewFetch {
            session,
            agent: "claude".into(),
        });
        let resolved = match f.ok(Command::ReviewThreadSet {
            session,
            thread: ThreadId(1),
            status: ThreadStatus::Resolved,
        }) {
            ResponseBody::ReviewThread { thread } => thread,
            other => panic!("{other:?}"),
        };
        assert_eq!(resolved.working, None);
    }

    #[test]
    fn a_deleted_target_reads_missing_until_undo_brings_it_back() {
        let mut f = Fixture::new();
        let session = f.session;
        let node = f.group("g");
        f.open(
            ReviewTarget::Node { node: node.clone() },
            ReviewAuthor::Human,
        );
        let before = f.review_events();

        f.ok(Command::NodeDelete { session, node });
        assert!(f.review().threads[0].target_missing);
        assert_eq!(f.review_events(), before + 1);

        let rev = f.rev();
        f.ok(Command::Undo {
            session,
            if_rev: rev,
        });
        assert!(!f.review().threads[0].target_missing);
        assert_eq!(f.review_events(), before + 2);
    }

    #[test]
    fn a_thread_follows_a_rename_through_undo_and_redo() {
        let mut f = Fixture::new();
        let session = f.session;
        let from = f.group("g");
        let to = NodeId::new("torso").unwrap();
        f.open(
            ReviewTarget::Node { node: from.clone() },
            ReviewAuthor::Human,
        );

        f.ok(Command::RenameId {
            session,
            rename: Rename::Node {
                from: from.clone(),
                to: to.clone(),
            },
        });
        let thread = &f.review().threads[0];
        assert_eq!(thread.target, ReviewTarget::Node { node: to.clone() });
        assert!(!thread.target_missing);

        let rev = f.rev();
        f.ok(Command::Undo {
            session,
            if_rev: rev,
        });
        let thread = &f.review().threads[0];
        assert_eq!(thread.target, ReviewTarget::Node { node: from });
        assert!(!thread.target_missing);

        let rev = f.rev();
        f.ok(Command::Redo {
            session,
            if_rev: rev,
        });
        let thread = &f.review().threads[0];
        assert_eq!(thread.target, ReviewTarget::Node { node: to });
        assert!(!thread.target_missing);
    }

    #[test]
    fn a_fork_starts_with_no_review() {
        let mut f = Fixture::new();
        let session = f.session;
        f.open(ReviewTarget::Model, ReviewAuthor::Human);
        let rev = f.rev();
        let fork = match f.ok(Command::SessionFork {
            session,
            if_rev: rev,
            name: None,
        }) {
            ResponseBody::SessionFork { session, .. } => session,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            f.ok(Command::ReviewGet { session: fork }),
            ResponseBody::Review { review } if review.threads.is_empty()
        ));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_silent_agent_stops_listening_and_stale_work_lapses() {
        let mut state = ReviewState::default();
        let model = Model::new();
        state
            .open(
                &model,
                0,
                ReviewTarget::Model,
                ReviewAuthor::Human,
                "x".into(),
                Vec::new(),
            )
            .unwrap();
        state.submit();
        assert!(state.poll("claude"));
        assert!(!state.poll("claude"));
        let Some(long_ago) = std::time::Instant::now().checked_sub(WORKING_FOR) else {
            return;
        };
        state.agent.as_mut().unwrap().polled = Some(Stamp(long_ago));
        assert!(!state.wire(&model).agent.unwrap().listening);
        assert!(state.poll("claude"), "coming back is a change");

        state.fetch(&model, "claude");
        assert!(state.wire(&model).threads[0].working.is_some());
        state.threads[0].working.as_mut().unwrap().since = Stamp(long_ago);
        assert_eq!(state.wire(&model).threads[0].working, None);
    }
}
