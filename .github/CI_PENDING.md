# CI is pending — please paste this workflow once

  > This file is here because the agent that bootstrapped `chikaharu/tren-crc`
  > authenticated through the Replit GitHub integration, whose OAuth App is
  > only granted `repo` scope (not `workflow`). GitHub explicitly refuses to
  > let an OAuth App without the `workflow` scope create or update files
  > under `.github/workflows/`, both via `git push` (`! [remote rejected] …
  > refusing to allow an OAuth App to create or update workflow … without
  > 'workflow' scope`) and via the REST APIs (Contents API and Git Database
  > API both 404 on the path).
  >
  > The CI YAML the agent wanted to commit is reproduced verbatim below.
  > Pasting it through the GitHub web editor (which uses your own user
  > credentials, which **do** have `workflow` scope) takes about 30
  > seconds and leaves the rest of the four-branch / relay setup intact.

  ## One-time manual install

  1. Open <https://github.com/chikaharu/tren-crc/new/develop?filename=.github/workflows/ci.yml>
  2. Paste the YAML block from the next section into the editor.
  3. Set the commit message to:

     ```
     ci: add GitHub Actions workflow

     Authored-on: develop
     ```

  4. Commit directly to `develop`. (Branch protection allows this — only
     force-push and deletion are blocked.)
  5. Optionally fast-forward `experiment` to the new tip so the alternation
     discipline records the next commit on `experiment`:

     ```bash
     git fetch
     git checkout experiment
     git merge --ff-only origin/develop
     git push
     ```

  6. After CI runs once green, you can also delete this `CI_PENDING.md`
     file (it has no behavioural effect).

  ## Paste this YAML into `.github/workflows/ci.yml`

  ```yaml
  name: CI

  on:
    push:
      branches: [develop, future, main, experiment]
    pull_request:
      branches: [develop, future, main]
    workflow_dispatch:

  env:
    CARGO_TERM_COLOR: always

  jobs:
    test:
      name: cargo test (release)
      runs-on: ubuntu-latest
      steps:
        - name: Checkout
          uses: actions/checkout@v4
        - name: Install Rust stable
          uses: dtolnay/rust-toolchain@stable
        - name: Cache cargo
          uses: actions/cache@v4
          with:
            path: |
              ~/.cargo/registry
              ~/.cargo/git
              target
            key: ${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}
            restore-keys: |
              ${{ runner.os }}-cargo-
        - name: Build (release)
          run: cargo build --release --all-targets
        - name: Library + priority tests
          run: cargo test --release --lib
          timeout-minutes: 10
        - name: CRC-32C detection tests
          run: cargo test --release --test crc_detect
          timeout-minutes: 10
        - name: Frame32 smoke tests
          run: cargo test --release --test smoke frame32
          timeout-minutes: 10

    relay-discipline:
      name: develop⇄experiment alternation check
      runs-on: ubuntu-latest
      if: github.event_name == 'push' && (github.ref == 'refs/heads/develop' || github.ref == 'refs/heads/experiment')
      steps:
        - uses: actions/checkout@v4
          with:
            fetch-depth: 50
        - name: Verify Authored-on alternation between HEAD and HEAD~1
          run: |
            set -e
            trailer() { git log -1 --pretty=%B "$1" | sed -n 's/^Authored-on:[[:space:]]*//p' | tail -n1 | tr -d '[:space:]'; }
            h0=$(trailer HEAD)
            h1=$(trailer HEAD~1 || true)
            echo "HEAD trailer    = $h0"
            echo "HEAD~1 trailer  = $h1"
            if [ -z "$h0" ]; then
              echo "::error::HEAD has no Authored-on trailer (required on develop/experiment by COMMIT_RULE.md)"
              exit 1
            fi
            if [ -n "$h1" ] && [ "$h0" = "$h1" ]; then
              echo "::error::Two consecutive commits authored on $h0 (relay rule violation; see COMMIT_RULE.md)"
              exit 1
            fi
  ```

  ## Why an OAuth App scope, not a code change

  `workflow` is a separate OAuth scope from `repo` precisely so that integrations
  that need to push code can't silently inject a new self-hosted runner workflow
  that exfiltrates secrets. This protection is doing its job here. The fix is to
  have a human (you) commit the workflow once; subsequent edits made by anyone
  with the `workflow` scope on their `git push` credential will work normally.

  If this `.github/CI_PENDING.md` file is still present after CI is wired up,
  it's safe to delete — it has no functional effect, it's documentation only.
  