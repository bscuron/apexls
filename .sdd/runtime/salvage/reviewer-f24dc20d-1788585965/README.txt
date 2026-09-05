Salvaged uncommitted worktree state.
session_id: reviewer-f24dc20d
timestamp: 1788585965
untracked_count: 0

Restore with:
  git apply diff.patch
  python -c 'import json,base64,os;d=json.load(open("untracked.json"));\
    [open(f["path"],"wb").write(base64.b64decode(f["base64"])) for f in d["files"]]'
