use catchlight_core::Model;

use super::*;
use crate::review::ReviewState;
use crate::review::state::{Draft, LEASE, Stamp};

#[test]
fn a_comment_travels_from_unsent_to_delivered_and_the_reply_ends_the_work() {
    let mut f = Fixture::new();
    let session = f.session;
    let rev = f.rev();

    let opened = f.open(ReviewTarget::Model, human());
    assert_eq!(opened.thread, ThreadId(1));
    assert_eq!(opened.comments[0].body, "look here");
    assert_eq!(opened.comments[0].comment, 1);
    assert_eq!(deliveries(&opened), [ReviewDelivery::Unsent]);

    assert!(matches!(
        f.ok(Command::ReviewPoll {
            session,
            agent: who("claude")
        }),
        ResponseBody::ReviewPending {
            pending: 0,
            batch: None
        }
    ));
    f.ok(Command::ReviewSubmit { session });
    assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Sent]);
    let polled = match f.ok(Command::ReviewPoll {
        session,
        agent: who("claude"),
    }) {
        ResponseBody::ReviewPending { pending: 1, batch } => batch,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        f.review().agents,
        [ReviewAgent {
            id: "claude:session".into(),
            name: "claude".into(),
            listening: true
        }]
    );

    let (fetched, batch) = f.fetch("claude");
    assert_eq!(polled, batch, "a poll names the batch a fetch would take");
    assert_eq!(fetched.len(), 1);
    assert_eq!(deliveries(&fetched[0]), [ReviewDelivery::Sent]);
    assert_eq!(fetched[0].holder, None);
    assert_eq!(
        f.fetch("claude"),
        (fetched, batch.clone()),
        "a fetch takes nothing"
    );
    assert_eq!(deliveries(&f.review().threads[0]), [ReviewDelivery::Sent]);

    let (claimed, held) = f.ack("claude", batch.as_deref().unwrap()).unwrap();
    assert_eq!((claimed, held), (vec![ThreadId(1)], Vec::new()));
    let review = f.review();
    assert_eq!(deliveries(&review.threads[0]), [ReviewDelivery::Delivered]);
    assert_eq!(review.threads[0].holder, Some(who("claude")));
    assert!(
        !review.agents[0].listening,
        "it took work, so it is not waiting"
    );
    assert_eq!(f.fetch("claude"), (Vec::new(), None));

    let replied = match f.ok(Command::ReviewCommentAdd {
        session,
        thread: ThreadId(1),
        author: agent("claude"),
        body: "done".into(),
        images: Vec::new(),
        key: None,
        version: None,
    }) {
        ResponseBody::ReviewThread { thread } => thread,
        other => panic!("{other:?}"),
    };
    assert_eq!(replied.holder, None);
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
    f.open(ReviewTarget::Model, human());
    assert_eq!(f.review_events(), 1);
    f.review();
    assert_eq!(f.review_events(), 1);

    let poll = Command::ReviewPoll {
        session,
        agent: who("claude"),
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
    assert!(
        f.events
            .lock()
            .unwrap()
            .iter()
            .all(|e| !matches!(e, Event::ModelChanged { .. }))
    );
}

#[test]
fn a_target_must_exist_and_a_body_must_say_something() {
    let mut f = Fixture::new();
    let session = f.session;
    let add = |target| Command::ReviewThreadAdd {
        session,
        target,
        author: human(),
        body: "x".into(),
        pose: Vec::new(),
        images: Vec::new(),
        key: None,
        version: None,
    };
    let node = NodeId::new("nope").unwrap();
    assert_eq!(
        f.refused(add(ReviewTarget::Node { node })),
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
            author: human(),
            body: " \n".into(),
            pose: Vec::new(),
            images: Vec::new(),
            key: None,
            version: None,
        }),
        ErrorCode::BadRequest
    );
    assert_eq!(
        f.refused(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(9),
            author: human(),
            body: "x".into(),
            images: Vec::new(),
            key: None,
            version: None,
        }),
        ErrorCode::NoThread
    );
    assert_eq!(
        f.refused(Command::ReviewPoll {
            session,
            agent: ReviewIdentity {
                id: " ".into(),
                name: "claude".into()
            }
        }),
        ErrorCode::BadRequest
    );
    assert!(f.review().threads.is_empty());
}

#[test]
fn deleting_the_only_comment_deletes_the_thread_and_ids_are_not_reused() {
    let mut f = Fixture::new();
    let session = f.session;
    f.open(ReviewTarget::Model, human());
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
    f.open(ReviewTarget::Model, human());
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
        author: human(),
        body: "still wrong".into(),
        images: Vec::new(),
        key: None,
        version: None,
    }) {
        ResponseBody::ReviewThread { thread } => thread,
        other => panic!("{other:?}"),
    };
    assert_eq!(reopened.status, ThreadStatus::Open);

    f.take("claude");
    assert_eq!(f.holder(0).as_deref(), Some("claude"));
    let resolved = match f.ok(Command::ReviewThreadSet {
        session,
        thread: ThreadId(1),
        status: ThreadStatus::Resolved,
    }) {
        ResponseBody::ReviewThread { thread } => thread,
        other => panic!("{other:?}"),
    };
    assert_eq!(resolved.holder, None);
}

#[test]
fn a_deleted_target_reads_missing_until_undo_brings_it_back() {
    let mut f = Fixture::new();
    let session = f.session;
    let node = f.group("g");
    f.open(ReviewTarget::Node { node: node.clone() }, human());
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
    f.open(ReviewTarget::Node { node: from.clone() }, human());

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
    f.open(ReviewTarget::Model, human());
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
fn a_silent_agent_stops_listening_and_a_stale_claim_lapses() {
    let mut state = ReviewState::default();
    let model = Model::new();
    state
        .open(
            &model,
            0,
            ReviewTarget::Model,
            Draft {
                author: human(),
                body: "x".into(),
                images: Vec::new(),
                key: None,
                version: None,
            },
            Vec::new(),
        )
        .unwrap();
    state.submit();
    assert_eq!(state.poll(&who("claude")).0, 1);
    let Some(long_ago) = std::time::Instant::now().checked_sub(LEASE) else {
        return;
    };
    state.agents[0].polled = Some(Stamp(long_ago));
    assert!(!state.presence()[0].listening);

    state.claim_threads(&who("claude"), &[ThreadId(1)]).unwrap();
    assert_eq!(state.pending(&who("codex").id), 0, "held for claude");
    state.threads[0].claim.as_mut().unwrap().since = Stamp(long_ago);
    assert_eq!(
        state.wire(&model, Kept::Session, None).threads[0].holder,
        None
    );
    assert_eq!(
        state.pending(&who("codex").id),
        1,
        "a lapsed claim frees it"
    );

    state.agents[0].heard = Stamp(long_ago);
    assert!(state.presence().is_empty(), "nothing heard for an hour");
}

#[test]
fn a_fetch_is_acknowledged_whole_or_found_stale() {
    let mut f = Fixture::new();
    let session = f.session;
    f.open(ReviewTarget::Model, human());
    f.open(ReviewTarget::Model, human());
    f.ok(Command::ReviewSubmit { session });

    let (first, batch) = f.fetch("claude");
    assert_eq!(first.len(), 2);
    let (again, also) = f.fetch("codex");
    assert_eq!((again, &also), (first, &batch), "both see what waits");
    let events = f.review_events();
    f.ack("claude", batch.as_deref().unwrap()).unwrap();
    assert_eq!(f.review_events(), events + 1);
    assert_eq!(
        f.ack("codex", also.as_deref().unwrap()),
        Err(ErrorCode::BatchStale),
        "claude took them first"
    );

    f.open(ReviewTarget::Model, human());
    f.ok(Command::ReviewSubmit { session });
    let (_, batch) = f.fetch("claude");
    f.ok(Command::ReviewThreadDelete {
        session,
        thread: ThreadId(3),
    });
    assert_eq!(
        f.ack("claude", batch.as_deref().unwrap()),
        Err(ErrorCode::BatchStale)
    );
    assert_eq!(f.ack("claude", "x"), Err(ErrorCode::BadRequest));
}

#[test]
fn a_claim_keeps_a_thread_for_one_agent_until_its_reply() {
    let mut f = Fixture::new();
    let session = f.session;
    f.open(ReviewTarget::Model, human());
    f.open(ReviewTarget::Model, human());
    let taken = f.take("claude");
    assert_eq!(taken.len(), 2);
    assert_eq!(f.holder(0).as_deref(), Some("claude"));

    let refused = f.claim("codex", &[2, 1]);
    assert!(
        matches!(
            &refused,
            Reply::Err { code: ErrorCode::ThreadClaimed, message, .. }
                if message.contains("claude (claude:session)")
        ),
        "{refused:?}"
    );
    assert!(matches!(
        f.claim("claude", &[1]),
        Reply::Ok {
            body: ResponseBody::ReviewClaimed {
                lease_seconds: 3600,
                ..
            },
            ..
        }
    ));

    f.keyed(human(), 1, "and the other ear", "k");
    f.ok(Command::ReviewSubmit { session });
    assert!(f.fetch("codex").0.is_empty(), "claude holds thread 1");
    assert!(matches!(
        f.ok(Command::ReviewPoll {
            session,
            agent: who("codex")
        }),
        ResponseBody::ReviewPending {
            pending: 0,
            batch: None
        }
    ));
    let (mine, _) = f.fetch("claude");
    assert_eq!(mine.len(), 1);

    f.keyed(agent("codex"), 1, "not mine", "a");
    assert_eq!(
        f.holder(0).as_deref(),
        Some("claude"),
        "only its own reply releases"
    );
    f.keyed(agent("claude"), 1, "done", "b");
    assert_eq!(f.holder(0), None);

    f.ok(Command::ReviewRelease {
        session,
        agent: who("codex"),
        threads: vec![ThreadId(2)],
    });
    assert_eq!(
        f.holder(1).as_deref(),
        Some("claude"),
        "not codex's to release"
    );
    f.ok(Command::ReviewRelease {
        session,
        agent: who("claude"),
        threads: vec![ThreadId(2)],
    });
    assert_eq!(f.holder(1), None);
    assert!(matches!(f.claim("codex", &[2]), Reply::Ok { .. }));
    assert_eq!(f.holder(1).as_deref(), Some("codex"));
    assert!(matches!(
        f.claim("codex", &[9]),
        Reply::Err {
            code: ErrorCode::NoThread,
            ..
        }
    ));

    let names: Vec<String> = f.review().agents.into_iter().map(|a| a.name).collect();
    assert_eq!(names, ["claude", "codex"]);
}

#[test]
fn an_ack_leaves_a_thread_claimed_meanwhile_to_its_holder() {
    let mut f = Fixture::new();
    let session = f.session;
    f.open(ReviewTarget::Model, human());
    f.open(ReviewTarget::Model, human());
    f.ok(Command::ReviewSubmit { session });
    let (_, batch) = f.fetch("codex");
    assert!(matches!(f.claim("claude", &[1]), Reply::Ok { .. }));

    let (claimed, held) = f.ack("codex", batch.as_deref().unwrap()).unwrap();
    assert_eq!(claimed, [ThreadId(2)]);
    assert_eq!(
        held,
        [ReviewHeld {
            thread: ThreadId(1),
            holder: who("claude"),
        }]
    );
    let review = f.review();
    assert_eq!(deliveries(&review.threads[0]), [ReviewDelivery::Sent]);
    assert_eq!(deliveries(&review.threads[1]), [ReviewDelivery::Delivered]);

    let (theirs, batch) = f.fetch("claude");
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].thread, ThreadId(1));
    f.ack("claude", batch.as_deref().unwrap()).unwrap();
    assert_eq!(
        deliveries(&f.review().threads[0]),
        [ReviewDelivery::Delivered]
    );
}

#[test]
fn a_repeated_key_writes_once_and_a_reused_one_is_refused() {
    let mut f = Fixture::new();
    let session = f.session;
    f.open(ReviewTarget::Model, human());
    let first = f.keyed(agent("claude"), 1, "done", "k1");
    let again = f.keyed(agent("claude"), 1, "done", "k1");
    let (Reply::Ok { body: first, .. }, Reply::Ok { body: again, .. }) = (first, again) else {
        panic!("a repeat is answered, not refused");
    };
    assert!(matches!((&first, &again), (
        ResponseBody::ReviewThread { thread: a },
        ResponseBody::ReviewThread { thread: b },
    ) if a == b && b.comments.len() == 2));

    assert!(matches!(
        f.keyed(agent("claude"), 1, "something else", "k1"),
        Reply::Err {
            code: ErrorCode::KeyConflict,
            ..
        }
    ));
    assert!(
        matches!(f.keyed(agent("codex"), 1, "done", "k1"), Reply::Ok { .. }),
        "a key is the author's own"
    );
    assert_eq!(f.review().threads[0].comments.len(), 3);

    let open = |key: &str| Command::ReviewThreadAdd {
        session,
        target: ReviewTarget::Model,
        author: agent("claude"),
        body: "a question".into(),
        pose: Vec::new(),
        images: Vec::new(),
        key: Some(key.into()),
        version: None,
    };
    let thread = |body| match body {
        ResponseBody::ReviewThread { thread } => thread.thread,
        other => panic!("{other:?}"),
    };
    let opened = thread(f.ok(open("t")));
    assert_eq!(thread(f.ok(open("t"))), opened);
    assert_eq!(f.review().threads.len(), 2);
    assert_eq!(
        f.refused(open("k1")),
        ErrorCode::KeyConflict,
        "k1 wrote a reply"
    );
}
