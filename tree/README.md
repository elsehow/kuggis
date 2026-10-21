# tree

The kuggis tree as applied, checked by `cell tree tree`, planned by `cell plan tree`.

A cell file is `<name>.json`, its name and grants; `tree/kuggis/kuggis-dev.json`:

```json
{"name":"kuggis-dev","grants":[
  {"agent":"claude"},
  {"code":{"repo":"elsehow/kuggis","write":true,"via":"elsehow-cells/kuggis"}}
]}
```

A cell does not know where it sits. `tree/kuggis.json` is held by the
human; `tree/kuggis/kuggis-dev.json` sits under kuggis, and re-parenting is
`mv`. A code grant names a repository at exe.dev's GitHub edge, `write` true
or false, and optionally `via`, a fork that carries it: the edge holds the
fork, the child pushes branches there, and its parent, which carries the
upstream, opens and merges the pull request.

`.pins/` holds each box's public key, `.pins/<name>.pub`, pinned on first
contact. A key that changes is refused until the pin is removed by hand.

See `cell/README.md` for the types, the plan and the wall.
