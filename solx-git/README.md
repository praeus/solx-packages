# solx-git

Git operations action package for solx-core, built on [git2](https://docs.rs/git2/latest/git2/)
(libgit2 bindings). One binary, four `Command` actions:

- **`git-clone`** — clone a repository into `path`, or, if a repo is
  already checked out there, fetch and hard-checkout it to the requested
  branch/rev instead of failing. Either way the working tree ends up
  matching what was asked for.
- **`git-branch`** — create a branch and/or switch the repo's current
  branch.
- **`git-commit`** — stage (`git add -A` by default) and commit a repo's
  current changes.
- **`git-patch`** — produce a unified diff of a repo's current changes
  (working tree vs `HEAD` by default — staged + unstaged + untracked; or
  index vs `HEAD` with `staged_only`).

```bash
solx exec /packages/solx-git/git-clone --json '{"url":"https://github.com/org/repo.git","path":"proj/repo"}'
solx exec /packages/solx-git/git-branch --json '{"path":"proj/repo","branch":"feature-x"}'
solx exec /packages/solx-git/git-commit --json '{"path":"proj/repo","message":"do the thing"}'
solx exec /packages/solx-git/git-patch --json '{"path":"proj/repo"}'
```

## Install

`solx install-package` registers all four actions via `install.solx`.
Before installing, build the binary (`cargo build --release` in this
directory) and edit `install.solx`'s `action_config.cwd` (and
`solx-package.json`'s `command_actions[*].cwd`) to the absolute path of
this package's `target/release` directory, or `solx save action` the same
reference again afterward to correct it.

## `path`: every action takes one, resolved the same way

Every action's `path` is either:

- **Relative** — resolved inside solx-core's file store: `files_directory`
  from `solx-config.json` (in the appdata dir — `%APPDATA%/praeus/solx` on
  Windows, `SOLX_APPDATA_DIR` to override), defaulting to `<appdata>/files`
  when unset. `path: "proj/repo"` with the default files dir resolves to
  `<appdata>/files/proj/repo`. A relative path may not contain `..` or a
  rooted component — it can't escape the files directory.
- **Absolute** — must be inside (or equal to) one of the directories listed
  in `<appdata>/solx-git.json`'s `allowed_paths` (subtree match — listing a
  root also covers everything nested under it). This is how you point
  solx-git at a repo that already lives somewhere outside the file store.
  **Missing file, missing field, or no matching entry all mean "not
  allowed"** — this is a deny-by-default allowlist, not a denylist, the
  same convention `solx-config.json`'s own `allowed_base_urls` uses for
  outbound HTTP.

`<appdata>/solx-git.json`:

```json
{
  "allowed_paths": [
    "D:/work/some-external-repo",
    "D:/other/root"
  ]
}
```

Every action's result echoes back the *resolved* absolute `path`, not the
input string, so a caller can see exactly where the repo ended up.

## git-clone

Params:

| field    | required | description                                                              |
|----------|----------|---------------------------------------------------------------------------|
| `url`    | yes      | Remote URL — `https://...` or an SSH form (`git@host:org/repo.git`, `ssh://...`) |
| `path`   | yes      | Where the repo is cloned into, or an existing checkout to sync           |
| `branch` | no       | Branch to check out. Ignored if `rev` is set                             |
| `rev`    | no       | A specific commit or tag to check out (detached HEAD unless it's a branch tip). Takes priority over `branch` |
| `depth`  | no       | Shallow-clone/fetch depth. Omit for full history                        |

Result: `{ path, url, branch, commit, cloned }` — `cloned` is `true` for a
fresh clone, `false` when `path` already held a repo that was fetched and
re-synced instead.

**Checkout is always hard** (`git checkout --force`-equivalent): any local
modifications already sitting at `path` are discarded. This action
reliably reproduces a source tree, it does not preserve local edits — read
those out first with `git-patch` if they matter.

### Credentials

Resolved from the environment, only for whichever credential kind libgit2
actually asks for:

- **SSH** — `GIT_SSH_KEY_PATH` (+ optional `GIT_SSH_KEY_PASSPHRASE`) if set,
  otherwise the running ssh-agent.
- **HTTPS** — `GIT_USERNAME` + `GIT_PASSWORD` if both are set, otherwise
  `GIT_TOKEN` alone (sent as the username with an empty password — the
  convention GitHub/GitLab personal-access tokens expect over HTTPS).
- A public repo needing no auth never triggers any of the above.

Set these via the action's `action_config.env` (same mechanism
`solx-quickjs`/`solx-omniparse` use for `SOLX_SERVER_URL`), not baked into
`install.solx`.

## git-branch

Params:

| field         | required | description                                                                 |
|---------------|----------|-------------------------------------------------------------------------------|
| `path`        | yes      | A path inside a git working directory                                       |
| `branch`      | yes      | Branch name to switch to (and, per `create`, to make first)                 |
| `create`      | no       | Create `branch` at `start_point` if it doesn't exist yet. Default `true`. If `false` and the branch is missing, this is an error instead of an implicit create |
| `start_point` | no       | Revspec the branch is created at. Only consulted when actually creating or moving it. Default `HEAD` |
| `force`       | no       | If `branch` already exists, move it to `start_point` instead of leaving it where it is. Default `false` |

Result: `{ path, branch, commit, created }` — `created` is `true` if this
call created the branch, `false` if it already existed.

Always ends with a hard checkout of the resulting branch, same discard
semantics as `git-clone`.

## git-commit

Params:

| field           | required | description                                                                 |
|-----------------|----------|-------------------------------------------------------------------------------|
| `path`          | yes      | A path inside a git working directory                                       |
| `message`       | yes      | Commit message                                                               |
| `all`           | no       | Stage all changes (new/modified/deleted, tracked and untracked) before committing — `git add -A`. Default `true`. `false` commits only what's already staged |
| `author_name`   | no       | Overrides the commit author/committer name. Requires `author_email` too      |
| `author_email`  | no       | Overrides the commit author/committer email. Requires `author_name` too      |

Result: `{ path, commit, branch }` — `branch` is `null` on a detached HEAD.

Without `author_name`/`author_email`, uses the repo's configured
`user.name`/`user.email` (`repo.signature()` — repo-local or global git
config); if neither is set anywhere, the action errors rather than
guessing.

## git-patch

Params:

| field                | required | description                                                                 |
|----------------------|----------|------------------------------------------------------------------------------|
| `path`               | yes      | A path inside a git working directory                                       |
| `staged_only`        | no       | Restrict to staged changes (index vs `HEAD`) instead of all current changes. Default `false` |
| `include_untracked`  | no       | Include untracked files as additions. Ignored when `staged_only` is set. Default `true` |
| `context_lines`      | no       | Lines of context around each hunk. Default `3`                              |

Result: `{ path, patch, files_changed, insertions, deletions }` — `patch`
is unified diff text, ready to write to a `.patch` file or feed to
`git apply`.

## Build

```bash
cargo build --release
```

Produces `target/release/solx-git.exe` (Windows) — no `wit`/wasm toolchain
involved; this is a plain native `Command` action like `solx-quickjs` and
`solx-omniparse`, not a wasm guest action like `solx-names`.
