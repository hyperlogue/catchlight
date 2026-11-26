//! The review commands a session answers.

use catchlight_core::Model;
use catchlight_editor_protocol::*;

use crate::session::{Captured, Session};
use crate::{Attachments, Editor, EditorError, Payload};

use super::document::kept_image;
use super::images::image_name;
use super::state::{Draft, LEASE, ReviewState, identity};

impl Editor {
    /// Versions are never removed, so one found here is there when the
    /// comment naming it lands.
    fn check_version(&self, session: SessionId, version: Option<u64>) -> Result<(), EditorError> {
        let Some(version) = version else {
            return Ok(());
        };
        self.with_session(session, |s| {
            if s.versions.has(version) {
                Ok(())
            } else {
                Err(EditorError::NoVersion(version))
            }
        })
    }

    /// Runs `f`, which may change what is kept, under the session lock and,
    /// when it reports a change, tells observers once the lock is released.
    fn review_session(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut ReviewState, &Model, u64) -> Result<(ResponseBody, bool), EditorError>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        self.reviewed(id, |s| {
            let rev = s.rev;
            // The change is kept before it is published: a store that
            // refuses leaves the review as every client last read it.
            let mut next = s.review.clone();
            let attended = next.attendance();
            let (body, changed) = f(&mut next, &s.model, rev)?;
            next.generation += 1;
            let out = (body, changed || next.attendance() != attended);
            s.review_keeping
                .write(&*self.kept_store, s.file.as_deref(), &next)?;
            s.review = next;
            Ok(out)
        })
    }

    /// For what changes only who attends and what they hold, which no store
    /// keeps: `f` changes the review in place, and must refuse before it
    /// changes anything.
    fn attend_session(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut ReviewState, &Model) -> Result<ResponseBody, EditorError>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        self.reviewed(id, |s| {
            let attended = s.review.attendance();
            let body = f(&mut s.review, &s.model)?;
            Ok((body, s.review.attendance() != attended))
        })
    }

    fn reviewed(
        &self,
        id: SessionId,
        f: impl FnOnce(&mut Session) -> Result<(ResponseBody, bool), EditorError>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        let captured = self.with_session_captured(id, |s| {
            s.writable()?;
            f(s)
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

    pub(crate) fn review(
        &self,
        command: Command,
        attachments: &mut Attachments,
        payload: &mut Option<Payload>,
    ) -> Result<Captured<ResponseBody>, EditorError> {
        match command {
            Command::ReviewImageAdd { session } => {
                let bytes = attachments.take("image").ok_or_else(|| {
                    EditorError::BadRequest(
                        "review_image_add needs the attachment \"image\"".into(),
                    )
                })?;
                self.attend_session(session, |review, _| {
                    let image = review.hold_image(bytes)?;
                    Ok(ResponseBody::ReviewImage { image })
                })
            }
            Command::ReviewImageGet { session, image } => {
                let captured = self.with_session_captured(session, |s| {
                    let info = s.review.image_info(image)?;
                    let stored = s
                        .review
                        .images
                        .get(&image)
                        .ok_or(EditorError::NoImage(image))?;
                    let bytes = match (&stored.bytes, s.file.as_deref()) {
                        (Some(bytes), _) => bytes.to_vec(),
                        (None, Some(key)) => kept_image(
                            &*self.kept_store,
                            key,
                            &image_name(image, stored.media_type),
                        )?,
                        (None, None) => return Err(EditorError::NoImage(image)),
                    };
                    Ok((info, stored.media_type, bytes))
                })?;
                let (image, content_type, bytes) = captured.value;
                *payload = Some(Payload {
                    content_type,
                    bytes,
                });
                Ok(Captured {
                    value: ResponseBody::ReviewImage { image },
                    rev: captured.rev,
                })
            }
            Command::ReviewGet { session } => self.with_session_captured(session, |s| {
                let kept = s.review_keeping.kept(&*self.kept_store, s.file.as_deref());
                let problem = (kept == Kept::Unreadable)
                    .then(|| s.review_keeping.unreadable.clone())
                    .flatten();
                Ok(ResponseBody::Review {
                    review: s.review.wire(&s.model, kept, problem),
                })
            }),
            Command::ReviewThreadAdd {
                session,
                target,
                author,
                body,
                pose,
                images,
                key,
                version,
            } => {
                self.check_version(session, version)?;
                self.review_session(session, |review, model, rev| {
                    let (thread, wrote) = review.open(
                        model,
                        rev,
                        target,
                        Draft {
                            author,
                            body,
                            images,
                            key,
                            version,
                        },
                        pose,
                    )?;
                    Ok((ResponseBody::ReviewThread { thread }, wrote))
                })
            }
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
                images,
                key,
                version,
            } => {
                self.check_version(session, version)?;
                self.review_session(session, |review, model, rev| {
                    let (thread, wrote) = review.reply(
                        model,
                        rev,
                        thread,
                        Draft {
                            author,
                            body,
                            images,
                            key,
                            version,
                        },
                    )?;
                    Ok((ResponseBody::ReviewThread { thread }, wrote))
                })
            }
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
                let agent = identity(agent)?;
                self.attend_session(session, |review, _| {
                    let (pending, batch) = review.poll(&agent);
                    Ok(ResponseBody::ReviewPending { pending, batch })
                })
            }
            Command::ReviewFetch { session, agent } => {
                let agent = identity(agent)?;
                self.attend_session(session, |review, model| {
                    let (threads, batch) = review.fetch(model, &agent);
                    Ok(ResponseBody::ReviewFetched { threads, batch })
                })
            }
            Command::ReviewAck {
                session,
                agent,
                batch,
            } => {
                let agent = identity(agent)?;
                self.review_session(session, |review, _, _| {
                    let (claimed, held) = review.ack(&agent, &batch)?;
                    Ok((ResponseBody::ReviewAcked { claimed, held }, true))
                })
            }
            Command::ReviewClaim {
                session,
                agent,
                threads,
            } => {
                let agent = identity(agent)?;
                self.attend_session(session, |review, _| {
                    review.claim_threads(&agent, &threads)?;
                    Ok(ResponseBody::ReviewClaimed {
                        threads,
                        lease_seconds: LEASE.as_secs() as u32,
                    })
                })
            }
            Command::ReviewRelease {
                session,
                agent,
                threads,
            } => {
                let agent = identity(agent)?;
                self.attend_session(session, |review, _| {
                    review.release(&agent, &threads)?;
                    Ok(ResponseBody::Empty)
                })
            }
            other => Err(EditorError::BadRequest(format!(
                "{} is not a review command",
                other.tag()
            ))),
        }
    }
}
