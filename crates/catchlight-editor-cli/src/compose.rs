//! Caller-side shortcuts over exact reads and atomic writes.
//!
//! Every read must carry the captured revision the final `edit_apply` guards
//! with, so concurrent work is refused rather than overwritten.

use anyhow::{Result, anyhow, bail};
use catchlight_editor_protocol::*;

use crate::cli::{Cli, resolve_session};
use crate::print::code_name;
use crate::transport;

pub struct CapturedClient<'a> {
    stream: &'a mut transport::Transport,
    pub(crate) session: SessionId,
    revision: Option<u64>,
    next_id: u64,
}

impl<'a> CapturedClient<'a> {
    pub(crate) fn new(cli: &Cli, stream: &'a mut transport::Transport) -> Result<Self> {
        Ok(Self {
            stream,
            session: resolve_session(cli)?,
            revision: cli.if_rev,
            next_id: 1,
        })
    }

    pub(crate) fn send(
        &mut self,
        command: Command,
        siblings: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Reply> {
        let id = self.next_id;
        self.next_id += 1;
        self.stream.call(Request { id, command }, siblings)
    }

    pub(crate) fn read(&mut self, command: Command) -> Result<ResponseBody> {
        match self.send(command, serde_json::Map::new())? {
            Reply::Ok {
                rev: Some(revision),
                body,
                ..
            } => {
                capture_revision(&mut self.revision, revision)?;
                Ok(body)
            }
            Reply::Err { code, message, .. } => bail!("{}: {message}", code_name(code)),
            _ => bail!("captured model read omitted its revision"),
        }
    }

    pub(crate) fn guard(&mut self) -> Result<u64> {
        if self.revision.is_none() {
            self.read(Command::Status {
                session: self.session,
            })?;
        }
        self.revision
            .ok_or_else(|| anyhow!("captured model read omitted its revision"))
    }

    pub(crate) fn batch(&mut self, edits: Vec<EditOp>) -> Result<Command> {
        Ok(Command::EditApply {
            session: self.session,
            if_rev: self.guard()?,
            edits,
        })
    }

    pub(crate) fn mesh(&mut self, node: &NodeId) -> Result<MeshInfo> {
        match self.read(Command::MeshGet {
            session: self.session,
            node: node.clone(),
            if_rev: self.revision,
        })? {
            ResponseBody::MeshInfo { mesh, .. } => Ok(mesh),
            _ => bail!("mesh_get returned an unexpected body"),
        }
    }

    pub(crate) fn binding(
        &mut self,
        node: &NodeId,
        params: &BindingParams,
        target: BindingTarget,
    ) -> Result<BindingInfo> {
        let ResponseBody::Bindings { bindings } = self.read(Command::BindingList {
            session: self.session,
            node: node.clone(),
        })?
        else {
            bail!("binding_list returned an unexpected body");
        };
        bindings
            .into_iter()
            .find(|binding| {
                binding.param == params.param
                    && binding.param_y == params.param_y
                    && binding.target == target
            })
            .ok_or_else(|| anyhow!("binding does not exist; create it with binding add"))
    }

    pub(crate) fn cells(
        &mut self,
        node: &NodeId,
        params: BindingParams,
        target: BindingTarget,
        cells: Vec<[u32; 2]>,
        derived: bool,
    ) -> Result<Vec<BindingCellRead>> {
        if cells.is_empty() {
            return Ok(Vec::new());
        }
        let ResponseBody::BindingCells { cells, .. } = self.read(Command::BindingCellsGet {
            session: self.session,
            if_rev: self.revision,
            node: node.clone(),
            params,
            target,
            cells,
            include_derived: derived,
            vertices: None,
        })?
        else {
            bail!("binding_cells_get returned an unexpected body");
        };
        Ok(cells)
    }
}

fn capture_revision(captured: &mut Option<u64>, revision: u64) -> Result<()> {
    if captured.is_some_and(|held| held != revision) {
        bail!("revision_conflict: model changed between reads; no edit was sent");
    }
    *captured = Some(revision);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coherent_reads_capture_once_and_reject_later_revisions() {
        let mut revision = None;
        capture_revision(&mut revision, 42).unwrap();
        capture_revision(&mut revision, 42).unwrap();
        assert!(capture_revision(&mut revision, 43).is_err());
        assert_eq!(revision, Some(42));
    }
}
