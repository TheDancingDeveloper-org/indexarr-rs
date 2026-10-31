# Contributing to indexarr-rs

## How changes land

Changes land through graft, not by pushing branches. Workers land changesets
on the graft line `backlog-1` (repository `indexarr-rs` on graft-dev). Oversight
gathers the landed changesets into one batch pull request against `main`
(`graft-mirror pr-batch backlog-1 --base main`) and merges it with a rebase,
so each land keeps its own commit. Required checks on `main`: `policy`.
`main` is the only long-lived branch; releases are `v*` tags on `main`.
