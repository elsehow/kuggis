# Synthetic tree example

These declarations illustrate the format. Names and repository coordinates
are fictional; they do not describe a deployed environment. Inspect them with
`cell tree examples/tree`. Do not provision these placeholders unchanged.

`00000000-0000-4000-8000-000000000001.json` declares the parent named `project`.
Its sibling UUID directory contains the declaration for `worker`, with a subset
of its parent's grants. Filenames and directories use IDs, not mutable names. A cell can develop in
its own workspace; a child is useful for delegated work with separate authority.

Store real declarations under `.kuggis/tree/` or outside the source checkout.
Pinned identities go in that tree's `.pins/` directory and stay local. Keep
operator receipts, chat state, and raw deployment/benchmark evidence private.
