# Pilot task definitions (shared by ALL arms — decomposition output only,
# no contract content). One block per task id.

- **362-1** Path provenance walk: given a repo path, walk native commit
  history (tip to genesis) and report, per commit that touched the path,
  the provenance recorded on the commit (intent, proposal revision,
  decision, evidence digest, actor, authored time).
- **362-2** Line attribution engine: for a file at HEAD, attribute every
  line to the commit that last changed it, building on the path walk.
- **362-3** CLI surface: a `forge blame <path>` command with human and
  --json output following the repo's envelope conventions.
- **362-4** Ledger enrichment: enrich blame output with intent title,
  decision status, and check status from the SQLite ledger.
- **362-5** Integration tests + docs for blame: end-to-end scenarios
  driving the real binary in temp repos, plus CLI docs.
- **382-1** Payload/docs honesty: qualify the workspace_path emitted by
  start/attempt-start as a materialization target (additive field + help
  text), so it no longer reads as an editing surface.
- **382-2** Drift guard: make `attempt attach` refuse (typed error, with
  an explicit override flag) when the target attempt's workspace dir has
  drifted from its recorded materialized content.
- **382-3** Integration tests for the drift guard: the silent-loss repro
  now fails loudly; override discards; no-drift attach unchanged;
  never-materialized attach unchecked.
