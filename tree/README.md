# tree

The kuggis tree as it is applied: one directory, checked by `cell tree tree`
and planned by `cell plan tree`. The directory is the tree.

A cell file is `<name>.json`, holding the cell's name and its grants:

```json
{"name":"kuggis-dev","grants":[
  {"agent":"claude"},
  {"code":{"repo":"elsehow/kuggis","write":false}}
]}
```

A cell does not know where it sits. `tree/kuggis-dev.json` is held by the
human; a child of it would be `tree/kuggis-dev/<child>.json`, and
re-parenting is `mv`. A code grant names a repository at exe.dev's GitHub
edge, `write` true or false, and nothing else.

`.pins/` holds each box's public key, `.pins/<name>.pub`, pinned on first
contact. A key that changes is refused until the pin is removed by hand.

See `cell/README.md` for the types, the plan and the wall.
