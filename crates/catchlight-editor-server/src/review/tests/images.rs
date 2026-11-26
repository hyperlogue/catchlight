use super::*;
use crate::EditorError;
use crate::review::ReviewState;
use crate::review::images::{MAX_HELD_IMAGES, MAX_IMAGE_BYTES, MAX_SESSION_IMAGE_BYTES};

fn limit(reply: Reply) -> String {
    match reply {
        Reply::Err {
            code: ErrorCode::LimitExceeded,
            limit: Some(limit),
            ..
        } => limit.resource,
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_image_is_typed_by_its_bytes_and_anything_else_is_refused() {
    let mut f = Fixture::new();
    assert_eq!(f.image(PNG).media_type, "image/png");
    assert_eq!(
        f.image(&[0xff, 0xd8, 0xff, 0xe0, 1]).media_type,
        "image/jpeg"
    );
    let webp = f.image(b"RIFF\x04\0\0\0WEBPVP8 ");
    assert_eq!((webp.image, webp.media_type.as_str()), (3, "image/webp"));
    assert!(matches!(
        f.upload(b"GIF89a"),
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
    assert_eq!(f.review_events(), 0, "a held image is nobody's news yet");
    assert_eq!(f.rev(), 0);
}

#[test]
fn an_image_rides_on_one_comment_and_reads_back_byte_for_byte() {
    let mut f = Fixture::new();
    let image = f.image(PNG);
    assert_eq!(image.byte_length as usize, PNG.len());
    let thread = match f.say(vec![image.image], "  ") {
        Reply::Ok {
            body: ResponseBody::ReviewThread { thread },
            ..
        } => thread,
        other => panic!("{other:?}"),
    };
    assert_eq!(thread.comments[0].body, "");
    assert_eq!(thread.comments[0].images, vec![image.clone()]);
    let payload = f.bytes(image.image).unwrap();
    assert_eq!(
        (payload.content_type, payload.bytes.as_slice()),
        ("image/png", PNG)
    );

    assert!(matches!(
        f.say(vec![image.image], "again"),
        Reply::Err {
            code: ErrorCode::NoImage,
            ..
        }
    ));
    assert!(matches!(
        f.say(Vec::new(), " "),
        Reply::Err {
            code: ErrorCode::BadRequest,
            ..
        }
    ));
}

#[test]
fn a_refused_comment_leaves_its_images_held() {
    let mut f = Fixture::new();
    let image = f.image(PNG).image;
    assert!(matches!(
        f.say(vec![image, 99], "x"),
        Reply::Err {
            code: ErrorCode::NoImage,
            ..
        }
    ));
    assert!(matches!(
        f.say(vec![image, image], "x"),
        Reply::Err {
            code: ErrorCode::NoImage,
            ..
        }
    ));
    let session = f.session;
    assert_eq!(
        f.refused(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(9),
            author: human(),
            body: "x".into(),
            images: vec![image],
            key: None,
            version: None,
        }),
        ErrorCode::NoThread
    );
    assert!(matches!(f.say(vec![image], "x"), Reply::Ok { .. }));
}

#[test]
fn image_limits_name_their_budget() {
    let mut f = Fixture::new();
    let mut big = PNG.to_vec();
    big.resize(MAX_IMAGE_BYTES + 1, 0);
    assert_eq!(limit(f.upload(&big)), "review_image_bytes");
    let five: Vec<u64> = (0..5).map(|_| f.image(PNG).image).collect();
    assert_eq!(limit(f.say(five, "x")), "review_comment_images");

    big.truncate(MAX_IMAGE_BYTES);
    let mut state = ReviewState::default();
    let fits = MAX_SESSION_IMAGE_BYTES / MAX_IMAGE_BYTES;
    for _ in 0..fits {
        let id = state.hold_image(big.clone()).unwrap().image;
        // Claimed, so the held-image cap does not make room.
        state.claim(&[id]);
    }
    assert!(matches!(
        state.hold_image(big),
        Err(EditorError::Limit {
            resource: "review_session_image_bytes",
            ..
        })
    ));
}

#[test]
fn a_retry_with_images_is_told_apart_by_their_bytes() {
    let mut f = Fixture::new();
    let session = f.session;
    let reply = |f: &mut Fixture, image: u64| {
        f.send(Command::ReviewCommentAdd {
            session,
            thread: ThreadId(1),
            author: agent("claude"),
            body: "after".into(),
            images: vec![image],
            key: Some("k".into()),
            version: None,
        })
    };
    f.open(ReviewTarget::Model, human());
    let first = f.image(PNG).image;
    assert!(matches!(reply(&mut f, first), Reply::Ok { .. }));
    let again = f.image(PNG).image;
    assert!(matches!(reply(&mut f, again), Reply::Ok { .. }));
    assert_eq!(f.review().threads[0].comments.len(), 2);
    assert_eq!(
        f.bytes(again).unwrap_err(),
        ErrorCode::NoImage,
        "a repeat's upload is dropped"
    );
    let other = f.image(b"\x89PNG\r\n\x1a\nother").image;
    assert!(matches!(
        reply(&mut f, other),
        Reply::Err {
            code: ErrorCode::KeyConflict,
            ..
        }
    ));
}

#[test]
fn deleting_a_comment_or_thread_frees_its_images() {
    let mut f = Fixture::new();
    let session = f.session;
    let first = f.image(PNG).image;
    let second = f.image(PNG).image;
    assert!(matches!(f.say(vec![first], "one"), Reply::Ok { .. }));
    f.ok(Command::ReviewCommentAdd {
        session,
        thread: ThreadId(1),
        author: human(),
        body: "two".into(),
        images: vec![second],
        key: None,
        version: None,
    });
    f.ok(Command::ReviewCommentDelete {
        session,
        thread: ThreadId(1),
        comment: 2,
    });
    assert_eq!(f.bytes(second).unwrap_err(), ErrorCode::NoImage);
    assert!(f.bytes(first).is_ok());
    f.ok(Command::ReviewThreadDelete {
        session,
        thread: ThreadId(1),
    });
    assert_eq!(f.bytes(first).unwrap_err(), ErrorCode::NoImage);
}

#[test]
fn an_image_nobody_claims_is_dropped_as_newer_ones_arrive() {
    let mut f = Fixture::new();
    let kept = f.image(PNG).image;
    assert!(matches!(f.say(vec![kept], "x"), Reply::Ok { .. }));
    let ids: Vec<u64> = (0..=MAX_HELD_IMAGES).map(|_| f.image(PNG).image).collect();
    assert_eq!(f.bytes(ids[0]).unwrap_err(), ErrorCode::NoImage);
    assert!(f.bytes(ids[1]).is_ok());
    assert!(f.bytes(kept).is_ok(), "a claimed image is not held");
    assert_eq!(*ids.last().unwrap(), kept + MAX_HELD_IMAGES as u64 + 1);
}
