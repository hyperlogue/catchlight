# Goal

The goal of `catchlight` is to create software stack for 2.5D character
animation based on mesh deformation.

`catchlight` is a pure Rust library. The core needs to run on Linux, Windows,
MacOS, iOS, Android, as well as Web browser through WebGPU with WebGL fallback.

# Tips

## Dev environment

The dev environment is managed by nix dev shell. It's likely already been setup
through direnv so there is no action for you to complete.
If you want to update the dev environment, look at `nix/shell.nix`.

## Make changes

Prefer making small, verifiable changes. Prioritize building test infra to make
sure all potential changes can be verified in a tight feedback loop.

## git commits

- Keep working on the current branch, unless you are told to create or jump to a
  branch.
- Before you stage, check for a concurrent committer. A non-empty index you
  didn't create means another agent is mid-commit. Run
  `git diff --cached --name-only`; if it lists anything, back off and re-check on
  a 5s → 10s → 30s schedule. Still staged after that? **Stop and ask the user** —
  never commit over another agent's staged work.
- **Commit only what you changed.** You can stage with `git add` or stage hunks
  through `git diff` and `git apply`, or with `git commit -m "<message>" --
<file_1> <file_2>` if the commit has only a few files. If staging or committing
  hits a blocker, pause and ask.
- Never amend or rewrite a commit unless the user explicitly asks.
- **Commit message** Subject: `<scope>: <imperative summary>`. lowercase, no
  trailing period, less than 72 chars. Body: (blank line, wrapped ~72) only when
  the _why_ isn't obvious from subject + diff; don't narrate the diff.
